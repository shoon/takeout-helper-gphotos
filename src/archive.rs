// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! Archive discovery and extraction.
//!
//! This module finds Google Takeout `.zip` / `.tgz` / `.tar.gz` shards and
//! extracts them into a scratch directory. Entries are never trusted. Limits
//! apply to bytes written rather than declared sizes. One bad entry does not
//! abort an otherwise valid archive.

use filetime::FileTime;
use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, info, warn};
use rand::{RngExt, distr::Alphanumeric};
use rayon::prelude::*;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use tar::Archive;
use walkdir::WalkDir;
use zip::ZipArchive;

/// Maximum number of entries a single archive may contain.
///
/// Real Google Takeout 50 GB shards routinely contain tens of thousands of
/// entries, so this only exists to stop pathological zip bombs.
pub const MAX_FILES_PER_ARCHIVE: usize = 100_000;

/// Maximum uncompressed size of a *single* file, in bytes (50 GB).
///
/// Exceeding this skips the entry; it never aborts the archive.
pub const MAX_UNCOMPRESSED_SIZE: u64 = 50 * 1024 * 1024 * 1024;

/// Maximum total uncompressed size of one archive, in bytes (100 GB).
pub const MAX_TOTAL_UNCOMPRESSED_SIZE: u64 = 100 * 1024 * 1024 * 1024;

/// Compression ratio above which we log a zip-bomb warning.
const MAX_COMPRESSION_RATIO: u64 = 100;

/// Maximum length of an extracted path.
const MAX_PATH_LENGTH: usize = 1024;

/// Maximum directory depth of an archive entry.
const MAX_DEPTH: usize = 100;

/// How often (in entries) long extraction loops sample the shutdown flag.
const SHUTDOWN_POLL_INTERVAL: usize = 32;

/// Prefix used for every scratch directory this crate creates and deletes.
///
/// [`TempDir::drop`] refuses to delete any directory whose name does not start
/// with this, so a user-supplied directory can never be removed by accident.
pub const TEMP_DIR_PREFIX: &str = "gphotos-takeout-";

/// Errors produced while extracting an archive.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// A shutdown was requested (Ctrl+C) while the archive was being extracted.
    #[error("extraction interrupted by shutdown request")]
    Interrupted,

    /// An I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// The archive could not be read.
    #[error("invalid archive: {0}")]
    InvalidArchive(String),

    /// An archive-level limit (entry count, total size, disk space) was hit.
    #[error("{0}")]
    LimitExceeded(String),

    /// Anything else.
    #[error("{0}")]
    Other(String),
}

impl From<zip::result::ZipError> for ArchiveError {
    fn from(e: zip::result::ZipError) -> Self {
        ArchiveError::InvalidArchive(e.to_string())
    }
}

impl From<indicatif::style::TemplateError> for ArchiveError {
    fn from(e: indicatif::style::TemplateError) -> Self {
        ArchiveError::Other(e.to_string())
    }
}

/// What one archive extraction actually did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractionSummary {
    /// Entries seen in the archive (files + directories).
    pub entries_seen: usize,
    /// Regular files written to disk.
    pub files_extracted: usize,
    /// Directories created from directory entries.
    pub dirs_created: usize,
    /// Entries skipped because they exceeded the per-file size limit.
    pub skipped_oversize: usize,
    /// Entries skipped because their path was unsafe (traversal, absolute, etc.).
    pub skipped_unsafe: usize,
    /// Total bytes actually written to disk.
    pub bytes_written: u64,
}

/// The limits applied to one extraction.
#[derive(Debug, Clone, Copy, Default)]
struct Limits {
    max_file_size: Option<u64>,
    max_archive_size: Option<u64>,
    max_files: Option<u64>,
}

impl Limits {
    fn new(
        max_file_size: Option<u64>,
        max_archive_size: Option<u64>,
        max_files: Option<u64>,
    ) -> Self {
        Limits {
            max_file_size,
            max_archive_size,
            max_files,
        }
    }

    /// Maximum uncompressed bytes for a single entry.
    fn file_size(&self) -> u64 {
        self.max_file_size.unwrap_or(MAX_UNCOMPRESSED_SIZE)
    }

    /// Maximum total uncompressed bytes for the whole archive.
    fn archive_size(&self) -> u64 {
        self.max_archive_size.unwrap_or(MAX_TOTAL_UNCOMPRESSED_SIZE)
    }

    /// Maximum number of entries in the archive.
    fn files(&self) -> u64 {
        self.max_files.unwrap_or(MAX_FILES_PER_ARCHIVE as u64)
    }
}

/// Which kind of archive a path looks like, based on its *file name*.
///
/// `Path::extension()` returns `"gz"` for `foo.tar.gz`, which is why this works
/// on the whole file name instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    Zip,
    Tgz,
}

fn classify_archive(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else if name.ends_with(".tgz") || name.ends_with(".tar.gz") {
        Some(ArchiveKind::Tgz)
    } else {
        None
    }
}

/// Lexically normalize an archive entry name into a relative path.
///
/// Returns `None` if the name contains a `..` component, is empty, or resolves
/// to nothing. Absolute paths and Windows prefixes are stripped rather than
/// rejected (they are still confined to the base directory afterwards).
///
/// Both `/` and `\` are treated as separators so that Windows-style entry names
/// are normalized on Unix too.
fn normalize_entry_name(name: &str) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();

    for raw in name.split(['/', '\\']) {
        match raw {
            "" | "." => continue,
            ".." => return None,
            other => {
                // Re-run through `Path::components` so platform-specific
                // prefixes (e.g. `C:`) are handled rather than embedded.
                for component in Path::new(other).components() {
                    match component {
                        Component::Normal(part) => normalized.push(part),
                        Component::CurDir => {}
                        Component::ParentDir => return None,
                        Component::RootDir | Component::Prefix(_) => {}
                    }
                }
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        None
    } else {
        Some(normalized)
    }
}

/// Resolve an archive entry name to a path guaranteed to be inside `base_dir`.
///
/// `base_dir` **must already be canonicalized** (see [`canonical_base`]). The
/// entry path itself is normalized *lexically* and never canonicalized, so a
/// symlink planted inside the extraction directory cannot be used to escape it
/// by resolving through it.
fn sanitize_path(entry_name: &str, canonical_base: &Path) -> Result<PathBuf, ArchiveError> {
    let relative = normalize_entry_name(entry_name)
        .ok_or_else(|| ArchiveError::Other(format!("Path traversal detected: {}", entry_name)))?;

    let joined = canonical_base.join(&relative);

    // Lexical containment check: `relative` has no `..` components, so this can
    // only fail if something very strange happened above.
    if !joined.starts_with(canonical_base) {
        return Err(ArchiveError::Other(format!(
            "Path traversal detected: {}",
            entry_name
        )));
    }

    if joined.as_os_str().len() > MAX_PATH_LENGTH {
        return Err(ArchiveError::Other(format!(
            "Path too long: {}",
            entry_name
        )));
    }

    Ok(joined)
}

/// Canonicalize the extraction directory once per archive.
fn canonical_base(base_dir: &Path) -> Result<PathBuf, ArchiveError> {
    fs::create_dir_all(base_dir)?;
    base_dir.canonicalize().map_err(|e| {
        ArchiveError::Other(format!(
            "Cannot resolve extraction directory {}: {}",
            base_dir.display(),
            e
        ))
    })
}

/// Check if an archive entry name is safe.
///
/// Only *real* traversal is rejected: a `..` path component. File names that
/// merely contain dots (`photo..json`, `IMG_1234_c..json`) and album folders
/// whose name ends in `..` are legitimate Takeout content and must pass.
fn is_safe_path(path: &str) -> bool {
    // Reject absolute paths.
    if path.starts_with('/') || path.starts_with('\\') {
        return false;
    }
    if cfg!(windows) && path.contains(':') {
        return false;
    }

    if path.len() > MAX_PATH_LENGTH {
        return false;
    }

    let mut depth = 0usize;
    for component in path.split(['/', '\\']) {
        // A complete `..` component is traversal; `a..b` is just a file name.
        if component == ".." {
            return false;
        }
        if !component.is_empty() {
            depth += 1;
        }
    }

    if depth > MAX_DEPTH {
        return false;
    }

    true
}

/// Check available disk space in bytes
fn get_available_disk_space(path: &Path) -> Result<u64, ArchiveError> {
    let stat = fs2::statvfs(path)
        .map_err(|e| ArchiveError::Other(format!("Cannot stat filesystem: {}", e)))?;
    Ok(stat.available_space())
}

/// Check if there's enough disk space for extraction
fn check_disk_space(extract_dir: &Path, required_space: u64) -> Result<(), ArchiveError> {
    let available_space = get_available_disk_space(extract_dir)?;
    if available_space < required_space {
        return Err(ArchiveError::LimitExceeded(format!(
            "Insufficient disk space: {} bytes available, {} bytes required",
            available_space, required_space
        )));
    }

    Ok(())
}

/// Bail out of an extraction loop if the user pressed Ctrl+C.
fn check_shutdown(index: usize) -> Result<(), ArchiveError> {
    if index.is_multiple_of(SHUTDOWN_POLL_INTERVAL) && crate::is_shutdown() {
        return Err(ArchiveError::Interrupted);
    }
    Ok(())
}

/// Convert a zip entry timestamp (MS-DOS, no timezone) into a [`FileTime`].
fn zip_datetime_to_filetime(dt: &zip::DateTime) -> Option<FileTime> {
    let date = chrono::NaiveDate::from_ymd_opt(
        i32::from(dt.year()),
        u32::from(dt.month()),
        u32::from(dt.day()),
    )?;
    let naive = date.and_hms_opt(
        u32::from(dt.hour()),
        u32::from(dt.minute()),
        u32::from(dt.second()),
    )?;
    Some(FileTime::from_unix_time(naive.and_utc().timestamp(), 0))
}

/// Apply an entry's modification time to the extracted file.
///
/// Failures are logged, never fatal - a wrong mtime is far better than a lost
/// photo.
fn apply_mtime(path: &Path, mtime: Option<FileTime>) {
    if let Some(mtime) = mtime
        && let Err(e) = filetime::set_file_mtime(path, mtime)
    {
        debug!(
            "Could not set modification time on {}: {}",
            path.display(),
            e
        );
    }
}

/// A temporary directory that automatically cleans itself up when dropped.
///
/// Only directories whose name starts with [`TEMP_DIR_PREFIX`] are ever
/// deleted; see [`TempDir::create_inside`].
pub struct TempDir {
    path: PathBuf,
    owned: bool,
}

impl TempDir {
    /// Wrap an existing path.
    ///
    /// Retained for tests and for callers that manage the directory themselves.
    /// The wrapper never takes ownership, so `Drop` will not delete the path.
    /// Prefer [`TempDir::create_inside`] when automatic cleanup is wanted.
    pub fn new(path: PathBuf) -> Self {
        TempDir { path, owned: false }
    }

    /// Create a freshly named scratch directory *inside* `base`.
    ///
    /// `base` itself is created if missing but is **never** deleted; only the
    /// generated `gphotos-takeout-<random>` subdirectory is removed on drop.
    pub fn create_inside(base: &Path) -> io::Result<TempDir> {
        fs::create_dir_all(base)?;

        for _ in 0..32 {
            let random_string: String = rand::rng()
                .sample_iter(&Alphanumeric)
                .take(16)
                .map(char::from)
                .collect();

            let candidate = base.join(format!("{}{}", TEMP_DIR_PREFIX, random_string));
            match fs::create_dir(&candidate) {
                Ok(()) => {
                    info!(
                        "Created temporary extraction directory: {}",
                        candidate.display()
                    );
                    return Ok(TempDir {
                        path: candidate,
                        owned: true,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "Could not create a unique temporary directory inside {}",
                base.display()
            ),
        ))
    }

    /// Get a reference to the path of the temporary directory
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Consume the handle and keep the directory on disk (`--keep-temp`).
    ///
    /// Returns the path so the caller can tell the user where the extracted
    /// files were left.
    pub fn keep(self) -> PathBuf {
        let path = self.path.clone();
        std::mem::forget(self);
        path
    }

    /// Check if the temporary directory exists
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Convert the path to a string lossily
    pub fn to_string_lossy(&self) -> std::borrow::Cow<'_, str> {
        self.path.to_string_lossy()
    }

    /// Whether this path looks like one we generated and may therefore delete.
    fn is_owned_path(path: &Path) -> bool {
        path.file_name()
            .map(|name| name.to_string_lossy().starts_with(TEMP_DIR_PREFIX))
            .unwrap_or(false)
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    /// Remove the temporary directory - but only if we created it.
    fn drop(&mut self) {
        if !self.owned || !TempDir::is_owned_path(&self.path) {
            // Defense in depth: never `remove_dir_all` a directory the user
            // handed us, even when its name resembles one of our scratch dirs.
            // Better to leak a scratch dir than to delete data.
            crate::progress::eprintln(format!(
                "Warning: refusing to delete temporary directory {} (name does not start with '{}')",
                self.path.display(),
                TEMP_DIR_PREFIX
            ));
            return;
        }

        if !self.path.exists() {
            return;
        }

        if let Err(e) = fs::remove_dir_all(&self.path) {
            // We use eprintln here because we can't use the log crate during drop
            crate::progress::eprintln(format!(
                "Warning: Failed to remove temporary directory {}: {}",
                self.path.display(),
                e
            ));
        }
    }
}

/// Create a temporary directory for extraction inside the system temp dir.
pub fn create_temp_directory() -> Result<TempDir, Box<dyn std::error::Error>> {
    Ok(TempDir::create_inside(&std::env::temp_dir())?)
}

/// Find all archive files (`.zip`, `.tgz`, `.tar.gz`) in the input directory.
///
/// Logs a prominent warning when nothing is found, since that means the run
/// will produce an empty library.
pub fn find_archive_files(
    input_path: &Path,
    recursive: bool,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    info!("Searching for archive files in: {}", input_path.display());

    let mut archive_files = Vec::new();

    // Create a progress bar for file discovery
    let discovery_pb = crate::progress::add(ProgressBar::new_spinner());
    discovery_pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["▹▹▹▹▹", "▸▹▹▹▹", "▹▸▹▹▹", "▹▹▸▹▹", "▹▹▹▸▹", "▹▹▹▹▸", ""])
            .template("  {spinner:.green} Archive discovery | {wide_msg}")?,
    );
    discovery_pb.enable_steady_tick(std::time::Duration::from_millis(100));

    // Walk through the directory
    let walker = if recursive {
        WalkDir::new(input_path)
    } else {
        WalkDir::new(input_path).max_depth(1)
    };

    for entry in walker {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && classify_archive(path).is_some() {
            discovery_pb.set_message(format!(
                "Found: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            archive_files.push(path.to_path_buf());
        }
    }

    discovery_pb.finish_and_clear();

    if archive_files.is_empty() {
        warn!(
            "No .zip, .tgz or .tar.gz archives were found in {}{}. Nothing will be extracted.",
            input_path.display(),
            if recursive {
                " (recursive)"
            } else {
                " (non-recursive - try --recursive)"
            }
        );
        crate::progress::eprintln(format!(
            "Warning: no archives found in {} - nothing to do.",
            input_path.display()
        ));
    }

    Ok(archive_files)
}

/// Build a progress bar used for a standalone (non-batched) extraction.
fn standalone_progress_bar(label: &str) -> ProgressBar {
    let pb = crate::progress::add(ProgressBar::new_spinner());
    if let Ok(style) =
        ProgressStyle::default_spinner().template("  {spinner:.green} {pos} entries | {wide_msg}")
    {
        pb.set_style(style);
    }
    pb.set_message(label.to_string());
    pb
}

/// Extract a single ZIP archive to the specified directory with security checks.
///
/// * `max_file_size` bounds the uncompressed size of **one entry**; oversized
///   entries are skipped and counted, never fatal.
/// * `max_archive_size` bounds the **total uncompressed bytes** of the archive.
/// * `max_files` bounds the **number of entries** in the archive.
pub fn extract_single_archive(
    zip_path: &Path,
    extract_dir: &Path,
    max_file_size: Option<u64>,
    max_archive_size: Option<u64>,
    max_files: Option<u64>,
) -> Result<ExtractionSummary, ArchiveError> {
    let pb = standalone_progress_bar(&zip_path.file_name().unwrap_or_default().to_string_lossy());
    let result = extract_zip_inner(
        zip_path,
        extract_dir,
        Limits::new(max_file_size, max_archive_size, max_files),
        &pb,
    );
    pb.finish_and_clear();
    result
}

fn extract_zip_inner(
    zip_path: &Path,
    extract_dir: &Path,
    limits: Limits,
    pb: &ProgressBar,
) -> Result<ExtractionSummary, ArchiveError> {
    debug!("Extracting archive: {}", zip_path.display());

    let file = fs::File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)?;

    // Entry-count limit (zip-bomb guard) - distinct from any byte limit.
    let max_files = limits.files();
    if archive.len() as u64 > max_files {
        return Err(ArchiveError::LimitExceeded(format!(
            "Archive contains too many entries ({} > {})",
            archive.len(),
            max_files
        )));
    }

    // Pre-scan the central directory (cheap - no decompression) to get a
    // declared total for the disk-space check and to warn about zip bombs.
    let mut declared_total: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index_raw(i)?;
        let uncompressed = entry.size();
        let compressed = entry.compressed_size();
        declared_total = declared_total.saturating_add(uncompressed);

        if uncompressed > 0 && compressed > 0 {
            let ratio = uncompressed / compressed;
            if ratio > MAX_COMPRESSION_RATIO {
                warn!(
                    "High compression ratio detected for {}: {}:1 ({} -> {})",
                    entry.name(),
                    ratio,
                    compressed,
                    uncompressed
                );
            }
        }
    }

    let max_archive_size = limits.archive_size();
    if declared_total > max_archive_size {
        return Err(ArchiveError::LimitExceeded(format!(
            "Archive total uncompressed size too large ({} > {})",
            declared_total, max_archive_size
        )));
    }

    check_disk_space(extract_dir, declared_total)?;

    let base = canonical_base(extract_dir)?;

    pb.set_length(archive.len() as u64);

    let max_file_size = limits.file_size();
    let mut summary = ExtractionSummary::default();

    for i in 0..archive.len() {
        check_shutdown(i)?;

        let mut entry = archive.by_index(i)?;
        summary.entries_seen += 1;
        pb.inc(1);

        let name = entry.name().to_string();

        if !is_safe_path(&name) {
            warn!("Skipping unsafe entry in {}: {}", zip_path.display(), name);
            summary.skipped_unsafe += 1;
            continue;
        }

        let outpath = match sanitize_path(&name, &base) {
            Ok(p) => p,
            Err(e) => {
                warn!("Skipping entry in {}: {}", zip_path.display(), e);
                summary.skipped_unsafe += 1;
                continue;
            }
        };

        if entry.is_dir() || name.ends_with('/') || name.ends_with('\\') {
            fs::create_dir_all(&outpath)?;
            summary.dirs_created += 1;
            continue;
        }

        // Skip - do not abort - when a single entry is too big.
        if entry.size() > max_file_size {
            warn!(
                "Skipping oversized entry {} in {} ({} > {} bytes)",
                name,
                zip_path.display(),
                entry.size(),
                max_file_size
            );
            summary.skipped_oversize += 1;
            continue;
        }

        let mtime = entry
            .last_modified()
            .as_ref()
            .and_then(zip_datetime_to_filetime);

        if let Some(parent) = outpath.parent() {
            fs::create_dir_all(parent)?;
        }

        // Enforce the per-file limit against bytes actually written, not the
        // attacker-declared size in the header.
        let written = {
            let mut outfile = fs::File::create(&outpath)?;
            let mut limited = (&mut entry).take(max_file_size.saturating_add(1));
            io::copy(&mut limited, &mut outfile)?
        };

        if written > max_file_size {
            warn!(
                "Skipping entry {} in {}: actual size exceeds {} bytes",
                name,
                zip_path.display(),
                max_file_size
            );
            let _ = fs::remove_file(&outpath);
            summary.skipped_oversize += 1;
            continue;
        }

        summary.bytes_written = summary.bytes_written.saturating_add(written);
        summary.files_extracted += 1;

        if summary.bytes_written > max_archive_size {
            return Err(ArchiveError::LimitExceeded(format!(
                "Archive expanded past the total size limit ({} > {} bytes)",
                summary.bytes_written, max_archive_size
            )));
        }

        apply_mtime(&outpath, mtime);
    }

    Ok(summary)
}

/// Extract a single TGZ (`.tgz` / `.tar.gz`) archive with security checks.
///
/// The gzip stream is decompressed **once**: all limits are enforced while
/// extracting, against bytes actually written.
pub fn extract_single_tgz_archive(
    tgz_path: &Path,
    extract_dir: &Path,
    max_file_size: Option<u64>,
    max_archive_size: Option<u64>,
    max_files: Option<u64>,
) -> Result<ExtractionSummary, ArchiveError> {
    let pb = standalone_progress_bar(&tgz_path.file_name().unwrap_or_default().to_string_lossy());
    let result = extract_tgz_inner(
        tgz_path,
        extract_dir,
        Limits::new(max_file_size, max_archive_size, max_files),
        &pb,
    );
    pb.finish_and_clear();
    result
}

fn extract_tgz_inner(
    tgz_path: &Path,
    extract_dir: &Path,
    limits: Limits,
    pb: &ProgressBar,
) -> Result<ExtractionSummary, ArchiveError> {
    debug!("Extracting TGZ archive: {}", tgz_path.display());

    // We cannot know the uncompressed size without decompressing, so the only
    // pre-flight check is that we have at least the compressed size
    // available. Cumulative limits are enforced as we go.
    let compressed_len = fs::metadata(tgz_path)?.len();
    check_disk_space(extract_dir, compressed_len)?;

    let base = canonical_base(extract_dir)?;

    let file = fs::File::open(tgz_path)?;
    let gz_decoder = GzDecoder::new(file);
    let mut archive = Archive::new(gz_decoder);

    let max_file_size = limits.file_size();
    let max_archive_size = limits.archive_size();
    let max_files = limits.files();

    let mut summary = ExtractionSummary::default();

    for (index, entry_result) in archive.entries()?.enumerate() {
        check_shutdown(index)?;

        let mut entry = entry_result.map_err(|e| ArchiveError::InvalidArchive(e.to_string()))?;
        summary.entries_seen += 1;
        pb.inc(1);

        if summary.entries_seen as u64 > max_files {
            return Err(ArchiveError::LimitExceeded(format!(
                "Archive contains too many entries (> {})",
                max_files
            )));
        }

        let raw_path = entry
            .path()
            .map_err(|e| ArchiveError::InvalidArchive(e.to_string()))?
            .to_path_buf();
        let name = raw_path.to_string_lossy().to_string();

        if !is_safe_path(&name) {
            warn!("Skipping unsafe entry in {}: {}", tgz_path.display(), name);
            summary.skipped_unsafe += 1;
            continue;
        }

        let outpath = match sanitize_path(&name, &base) {
            Ok(p) => p,
            Err(e) => {
                warn!("Skipping entry in {}: {}", tgz_path.display(), e);
                summary.skipped_unsafe += 1;
                continue;
            }
        };

        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&outpath)?;
            summary.dirs_created += 1;
            continue;
        }

        if !entry.header().entry_type().is_file() {
            // Symlinks, hardlinks, devices, fifos - never materialize these.
            debug!("Skipping non-regular tar entry: {}", name);
            summary.skipped_unsafe += 1;
            continue;
        }

        if entry.size() > max_file_size {
            warn!(
                "Skipping oversized entry {} in {} ({} > {} bytes)",
                name,
                tgz_path.display(),
                entry.size(),
                max_file_size
            );
            summary.skipped_oversize += 1;
            // The tar reader skips the remaining body automatically on the
            // next iteration, so we can just continue.
            continue;
        }

        let mtime = entry
            .header()
            .mtime()
            .ok()
            .map(|secs| FileTime::from_unix_time(secs as i64, 0));

        if let Some(parent) = outpath.parent() {
            fs::create_dir_all(parent)?;
        }

        let written = {
            let mut outfile = fs::File::create(&outpath)?;
            let mut limited = (&mut entry).take(max_file_size.saturating_add(1));
            io::copy(&mut limited, &mut outfile)?
        };

        if written > max_file_size {
            warn!(
                "Skipping entry {} in {}: actual size exceeds {} bytes",
                name,
                tgz_path.display(),
                max_file_size
            );
            let _ = fs::remove_file(&outpath);
            summary.skipped_oversize += 1;
            continue;
        }

        summary.bytes_written = summary.bytes_written.saturating_add(written);
        summary.files_extracted += 1;

        if summary.bytes_written > max_archive_size {
            return Err(ArchiveError::LimitExceeded(format!(
                "Archive expanded past the total size limit ({} > {} bytes)",
                summary.bytes_written, max_archive_size
            )));
        }

        pb.set_message(format!(
            "{} ({} files, {} MiB)",
            tgz_path.file_name().unwrap_or_default().to_string_lossy(),
            summary.files_extracted,
            summary.bytes_written / (1024 * 1024)
        ));

        apply_mtime(&outpath, mtime);
    }

    Ok(summary)
}

/// Extract a single archive (ZIP or TGZ), dispatching on the file name.
pub fn extract_single_archive_auto(
    archive_path: &Path,
    extract_dir: &Path,
    max_file_size: Option<u64>,
    max_archive_size: Option<u64>,
    max_files: Option<u64>,
) -> Result<ExtractionSummary, ArchiveError> {
    match classify_archive(archive_path) {
        Some(ArchiveKind::Zip) => extract_single_archive(
            archive_path,
            extract_dir,
            max_file_size,
            max_archive_size,
            max_files,
        ),
        Some(ArchiveKind::Tgz) => extract_single_tgz_archive(
            archive_path,
            extract_dir,
            max_file_size,
            max_archive_size,
            max_files,
        ),
        None => Err(ArchiveError::Other(format!(
            "Unsupported archive format: {}",
            archive_path.display()
        ))),
    }
}

/// Extract all archives into `temp_dir`, in parallel.
///
/// Returns one entry per input archive, in the input order, so the caller can
/// report which shards succeeded and which did not. Errors are **not**
/// swallowed.
pub fn extract_archives(
    archive_files: Vec<PathBuf>,
    temp_dir: &Path,
    max_file_size: Option<u64>,
    max_archive_size: Option<u64>,
    max_files: Option<u64>,
) -> Vec<(PathBuf, Result<ExtractionSummary, ArchiveError>)> {
    info!(
        "Extracting {} archives to: {}",
        archive_files.len(),
        temp_dir.display()
    );

    if archive_files.is_empty() {
        return Vec::new();
    }

    let limits = Limits::new(max_file_size, max_archive_size, max_files);

    let multi = crate::progress::multi_progress();
    let overall = multi.add(ProgressBar::new(archive_files.len() as u64));
    if let Ok(style) = ProgressStyle::default_bar()
        .template("  {spinner:.green} Archives {pos}/{len} [{wide_bar:.cyan/blue}] ETA {eta}")
    {
        overall.set_style(style.progress_chars("#>-"));
    }

    let results: Vec<(PathBuf, Result<ExtractionSummary, ArchiveError>)> = archive_files
        .into_par_iter()
        .map(|archive_file| {
            let pb = multi.add(ProgressBar::new_spinner());
            if let Ok(style) = ProgressStyle::default_spinner()
                .template("  {spinner:.green} {pos} entries | {wide_msg}")
            {
                pb.set_style(style);
            }
            pb.set_message(
                archive_file
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            );

            let result = match classify_archive(&archive_file) {
                Some(ArchiveKind::Zip) => extract_zip_inner(&archive_file, temp_dir, limits, &pb),
                Some(ArchiveKind::Tgz) => extract_tgz_inner(&archive_file, temp_dir, limits, &pb),
                None => Err(ArchiveError::Other(format!(
                    "Unsupported archive format: {}",
                    archive_file.display()
                ))),
            };

            match &result {
                Ok(summary) => info!(
                    "Extracted {}: {} files, {} bytes ({} skipped oversize, {} skipped unsafe)",
                    archive_file.display(),
                    summary.files_extracted,
                    summary.bytes_written,
                    summary.skipped_oversize,
                    summary.skipped_unsafe
                ),
                Err(e) => log::error!("Failed to extract {}: {}", archive_file.display(), e),
            }

            pb.finish_and_clear();
            overall.inc(1);
            (archive_file, result)
        })
        .collect();

    overall.finish_and_clear();

    results
}

/// A set of Takeout shards that were downloaded as one numbered sequence.
///
/// Google splits a large export into `takeout-<stamp>-001.zip`,
/// `takeout-<stamp>-002.zip`, and so on. Each part is a complete, independent
/// archive (not a split tar), so a missing part does not break extraction. It
/// silently removes photos from the result, which is why the tool looks for
/// gaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveGroup {
    /// The name shared by every part, e.g. `takeout-20240101T000000Z`.
    pub base_name: String,
    /// The parts, ordered by part number.
    pub parts: Vec<PathBuf>,
    /// The part numbers present, ascending.
    pub part_numbers: Vec<u32>,
    /// Part numbers between the lowest and the highest that are **absent**.
    pub missing: Vec<u32>,
}

impl ArchiveGroup {
    /// True when the numbered sequence has holes in it.
    pub fn has_gaps(&self) -> bool {
        !self.missing.is_empty()
    }
}

/// Strip a recognised archive extension, returning the remaining stem.
fn strip_archive_extension(name: &str) -> Option<&str> {
    let lower = name.to_lowercase();
    for ext in [".tar.gz", ".tgz", ".zip"] {
        if lower.ends_with(ext) {
            return Some(&name[..name.len() - ext.len()]);
        }
    }
    None
}

/// Split `takeout-...-001` into `("takeout-...", 1)`.
///
/// Requires at least two digits so an ordinary `IMG-1.zip` is not mistaken for
/// a shard sequence.
fn split_part_suffix(stem: &str) -> Option<(&str, u32, bool)> {
    let dash = stem.rfind('-')?;
    let digits = &stem[dash + 1..];
    if digits.len() < 2 || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let base = &stem[..dash];
    if base.is_empty() {
        return None;
    }
    let number = digits.parse::<u32>().ok()?;
    Some((base, number, digits.starts_with('0')))
}

/// Group archive paths into numbered shard sequences.
///
/// Only sequences of **two or more** parts are reported, and only when they
/// look like a real Takeout download: either the shared base name mentions
/// `takeout`, or the part numbers are zero-padded the way Google writes them.
/// Without that guard a pair of unrelated files (`holiday-2018.zip`,
/// `holiday-2019.zip`) would be announced as a shard set with 200 missing
/// parts.
pub fn detect_split_archives(paths: &[PathBuf]) -> Vec<ArchiveGroup> {
    // Preserve first-seen order of bases so the output is deterministic before
    // the final sort.
    let mut groups: std::collections::HashMap<String, (Vec<(u32, PathBuf)>, bool)> =
        std::collections::HashMap::new();

    for path in paths {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let Some(stem) = strip_archive_extension(&name) else {
            continue;
        };
        let Some((base, number, padded)) = split_part_suffix(stem) else {
            continue;
        };

        let entry = groups
            .entry(base.to_string())
            .or_insert((Vec::new(), false));
        entry.0.push((number, path.clone()));
        entry.1 |= padded;
    }

    let mut result: Vec<ArchiveGroup> = groups
        .into_iter()
        .filter(|(base, (parts, padded))| {
            parts.len() >= 2 && (*padded || base.to_lowercase().contains("takeout"))
        })
        .map(|(base_name, (mut parts, _))| {
            parts.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

            let part_numbers: Vec<u32> = parts.iter().map(|(n, _)| *n).collect();
            let present: std::collections::HashSet<u32> = part_numbers.iter().copied().collect();
            let missing = match (part_numbers.first(), part_numbers.last()) {
                (Some(&min), Some(&max)) => (min..=max).filter(|n| !present.contains(n)).collect(),
                _ => Vec::new(),
            };

            ArchiveGroup {
                base_name,
                parts: parts.into_iter().map(|(_, p)| p).collect(),
                part_numbers,
                missing,
            }
        })
        .collect();

    result.sort_by(|a, b| a.base_name.cmp(&b.base_name));
    result
}

/// Log the shard sequences and warn about missing parts.
///
/// This check is advisory. Extraction still proceeds, but the warning lets the
/// user replace a missing part before relying on the output.
pub fn report_split_archives(groups: &[ArchiveGroup]) {
    for group in groups {
        info!(
            "Split archive set '{}': {} parts ({:?})",
            group.base_name,
            group.parts.len(),
            group.part_numbers
        );

        if group.has_gaps() {
            let missing: Vec<String> = group.missing.iter().map(|n| n.to_string()).collect();
            let message = format!(
                "Split archive set '{}' is INCOMPLETE: parts {} are missing (found {} of {}). \
                 Each part holds different photos, so anything in the missing parts will not \
                 appear in your library. Download the missing parts and re-run before deleting \
                 your originals.",
                group.base_name,
                missing.join(", "),
                group.part_numbers.len(),
                group.part_numbers.len() + group.missing.len()
            );
            warn!("{}", message);
            crate::progress::eprintln(format!("\nWARNING: {}\n", message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `TempDir::create_inside` must create a subdirectory and delete only that.
    #[test]
    fn test_temp_dir_create_inside_never_deletes_base() {
        let base = tempfile::TempDir::new().unwrap();
        let base_path = base.path().to_path_buf();

        // Something pre-existing in the base directory that must survive.
        let bystander = base_path.join("precious.txt");
        fs::write(&bystander, "do not delete me").unwrap();

        let temp_dir = TempDir::create_inside(&base_path).unwrap();
        let scratch = temp_dir.path().to_path_buf();

        assert!(scratch.starts_with(&base_path));
        assert_ne!(scratch, base_path);
        assert!(scratch.exists());
        assert!(
            scratch
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(TEMP_DIR_PREFIX)
        );

        fs::write(scratch.join("extracted.txt"), "temp").unwrap();

        drop(temp_dir);

        assert!(!scratch.exists(), "scratch dir should be removed");
        assert!(base_path.exists(), "base dir must survive");
        assert!(bystander.exists(), "pre-existing content must survive");
    }

    /// `TempDir::new` on a foreign path must refuse to delete it.
    #[test]
    fn test_temp_dir_refuses_foreign_path() {
        let base = tempfile::TempDir::new().unwrap();
        let user_dir = base.path().join("gphotos-takeout-user-data");
        fs::create_dir(&user_dir).unwrap();
        fs::write(user_dir.join("thesis.txt"), "years of work").unwrap();

        drop(TempDir::new(user_dir.clone()));

        assert!(user_dir.exists(), "user directory must not be deleted");
        assert!(user_dir.join("thesis.txt").exists());
    }

    /// Test that TempDir cleans up its directory when dropped
    #[test]
    fn test_temp_dir_cleanup() {
        let temp_dir = create_temp_directory().unwrap();
        let temp_path = temp_dir.path().to_path_buf();

        assert!(temp_path.exists());

        let test_file = temp_path.join("test_file.txt");
        fs::write(&test_file, "test content").expect("Failed to create test file");
        assert!(test_file.exists());

        drop(temp_dir);

        assert!(
            !temp_path.exists(),
            "Temporary directory should be deleted after drop"
        );
    }

    /// Test that attempting to extract a corrupt ZIP file returns an Err and does not panic
    #[test]
    fn test_extract_corrupt_zip() {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let corrupt_zip_path = temp_dir.path().join("corrupt.zip");
        fs::write(&corrupt_zip_path, "This is not a valid ZIP file content").unwrap();

        let extract_dir = temp_dir.path().join("extracted");
        fs::create_dir(&extract_dir).unwrap();

        let result = extract_single_archive(&corrupt_zip_path, &extract_dir, None, None, None);
        assert!(result.is_err());

        let extracted_files: Vec<_> = fs::read_dir(&extract_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .collect();
        assert_eq!(extracted_files.len(), 0);
    }

    /// `..` as a whole component is traversal; `..` inside a name is not.
    #[test]
    fn test_safe_path_with_dotdot_in_filename() {
        assert!(
            is_safe_path("5686D3D1-2D8E-4790-8C2B-C8B20AB37237_4_5005_c..json"),
            "File name with '..' should be safe"
        );
        assert!(
            is_safe_path("Takeout/Google Photos/Weird Album../IMG_0001.jpg"),
            "Album folder ending in '..' should be safe"
        );
        assert!(is_safe_path("a..b.json"), "'a..b.json' should be safe");

        assert!(!is_safe_path("../malicious_file.txt"));
        assert!(!is_safe_path("..\\malicious_file.txt"));
        assert!(!is_safe_path("subdir/../malicious_file.txt"));
        assert!(!is_safe_path("subdir\\..\\malicious_file.txt"));
        assert!(!is_safe_path("/etc/passwd"));
    }

    #[test]
    fn test_normalize_entry_name() {
        assert_eq!(
            normalize_entry_name("Takeout/Google Photos/a.jpg"),
            Some(PathBuf::from("Takeout/Google Photos/a.jpg"))
        );
        assert_eq!(
            normalize_entry_name("/abs/path.jpg"),
            Some(PathBuf::from("abs/path.jpg"))
        );
        assert_eq!(normalize_entry_name("a/../b"), None);
        assert_eq!(normalize_entry_name(".."), None);
        assert_eq!(
            normalize_entry_name("./x.jpg"),
            Some(PathBuf::from("x.jpg"))
        );
        assert_eq!(normalize_entry_name(""), None);
        assert_eq!(
            normalize_entry_name("album../a..b.json"),
            Some(PathBuf::from("album../a..b.json"))
        );
    }

    /// `sanitize_path` must keep working across repeated calls once the output
    /// directories exist (regression for the "aborts every archive after the
    /// first" bug), including for a relative base directory.
    #[test]
    fn test_sanitize_path_is_stable_for_relative_base() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let base = temp_dir.path().join("out");
        fs::create_dir_all(base.join("Takeout")).unwrap();

        let canonical = canonical_base(&base).unwrap();

        let first = sanitize_path("Takeout/a.jpg", &canonical).unwrap();
        fs::write(&first, b"x").unwrap();
        let second = sanitize_path("Takeout/b.jpg", &canonical).unwrap();

        assert!(first.starts_with(&canonical));
        assert!(second.starts_with(&canonical));
        assert!(sanitize_path("../escape.jpg", &canonical).is_err());
    }

    #[test]
    fn test_classify_archive_recognizes_tar_gz() {
        assert_eq!(
            classify_archive(Path::new("takeout-001.tar.gz")),
            Some(ArchiveKind::Tgz)
        );
        assert_eq!(
            classify_archive(Path::new("takeout-001.TAR.GZ")),
            Some(ArchiveKind::Tgz)
        );
        assert_eq!(
            classify_archive(Path::new("takeout-001.tgz")),
            Some(ArchiveKind::Tgz)
        );
        assert_eq!(classify_archive(Path::new("a.zip")), Some(ArchiveKind::Zip));
        assert_eq!(classify_archive(Path::new("a.gz")), None);
        assert_eq!(classify_archive(Path::new("a.tar")), None);
        assert_eq!(classify_archive(Path::new("readme.txt")), None);
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from("/in").join(n)).collect()
    }

    #[test]
    fn test_detect_split_archives_groups_and_orders_parts() {
        let groups = detect_split_archives(&paths(&[
            "takeout-20240101T000000Z-003.zip",
            "takeout-20240101T000000Z-001.zip",
            "takeout-20240101T000000Z-002.zip",
        ]));

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].base_name, "takeout-20240101T000000Z");
        assert_eq!(groups[0].part_numbers, vec![1, 2, 3]);
        assert!(!groups[0].has_gaps());
        assert!(groups[0].parts[0].ends_with("takeout-20240101T000000Z-001.zip"));
    }

    /// The whole point: 001 and 003 present, 002 absent means missing photos.
    #[test]
    fn test_detect_split_archives_reports_gaps() {
        let groups = detect_split_archives(&paths(&[
            "takeout-20240101T000000Z-001.zip",
            "takeout-20240101T000000Z-003.zip",
            "takeout-20240101T000000Z-006.zip",
        ]));

        assert_eq!(groups.len(), 1);
        assert!(groups[0].has_gaps());
        assert_eq!(groups[0].missing, vec![2, 4, 5]);
        // Advisory only: every present part is still listed for extraction.
        assert_eq!(groups[0].parts.len(), 3);
    }

    #[test]
    fn test_detect_split_archives_handles_tgz_and_tar_gz() {
        let groups = detect_split_archives(&paths(&[
            "takeout-20251111T183213Z-1-001.tgz",
            "takeout-20251111T183213Z-1-002.tgz",
            "other-20240101T000000Z-001.tar.gz",
            "other-20240101T000000Z-002.tar.gz",
        ]));

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].base_name, "other-20240101T000000Z");
        assert_eq!(groups[1].base_name, "takeout-20251111T183213Z-1");
        assert!(groups.iter().all(|g| !g.has_gaps()));
    }

    /// Unrelated files that merely end in `-<number>` must not be announced as
    /// an incomplete shard set.
    #[test]
    fn test_detect_split_archives_ignores_unrelated_names() {
        assert!(
            detect_split_archives(&paths(&["holiday-2018.zip", "holiday-2019.zip"])).is_empty()
        );
        // A lone shard is not a sequence.
        assert!(detect_split_archives(&paths(&["takeout-20240101T000000Z-001.zip"])).is_empty());
        // Single-digit suffixes are too ambiguous to treat as part numbers.
        assert!(detect_split_archives(&paths(&["IMG-1.zip", "IMG-2.zip"])).is_empty());
        // Non-archives are not shards.
        assert!(detect_split_archives(&paths(&["notes-001.txt", "notes-002.txt"])).is_empty());
    }

    #[test]
    fn test_report_split_archives_does_not_panic() {
        let groups = detect_split_archives(&paths(&[
            "takeout-20240101T000000Z-001.zip",
            "takeout-20240101T000000Z-003.zip",
        ]));
        report_split_archives(&groups);
    }

    #[test]
    fn test_limits_defaults_are_separate_concepts() {
        let limits = Limits::new(None, Some(1234), None);
        // A byte budget must not become a file-count budget.
        assert_eq!(limits.files(), MAX_FILES_PER_ARCHIVE as u64);
        assert_eq!(limits.archive_size(), 1234);
        assert_eq!(limits.file_size(), MAX_UNCOMPRESSED_SIZE);
    }
}
