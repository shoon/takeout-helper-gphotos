// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use chrono::{DateTime, TimeZone, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, error, info, warn};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, LazyLock, Mutex};

use crate::manifest::Manifest;
use crate::metadata::MediaMetadataPair;

/// Directory (relative to the output root) that receives files for which no
/// trustworthy capture date could be determined.
pub const UNKNOWN_DATE_DIR: &str = "unknown-date";

/// The output layout to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrganizeMode {
    /// `YYYY/MM/` is the default and the only layout guaranteed to have
    /// a home for every file.
    #[default]
    Date,
    /// `<album>/` for files that belong to a user album, falling back to the
    /// date layout for everything else (most of a takeout has no album).
    Album,
    /// Everything in the output root.
    Flat,
    /// `YYYY/MM/` for every file, **plus** a second copy under `<album>/` for
    /// album members, so the albums survive without breaking the chronology.
    DateAlbum,
}

impl OrganizeMode {
    /// Parse the `--organize` value. Returns `None` for anything unknown so the
    /// caller can produce a CLI error rather than silently picking a default.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "date" => Some(OrganizeMode::Date),
            "album" => Some(OrganizeMode::Album),
            "flat" => Some(OrganizeMode::Flat),
            "date-album" | "datealbum" => Some(OrganizeMode::DateAlbum),
            _ => None,
        }
    }

    /// The value string this mode parses back from.
    pub fn as_str(self) -> &'static str {
        match self {
            OrganizeMode::Date => "date",
            OrganizeMode::Album => "album",
            OrganizeMode::Flat => "flat",
            OrganizeMode::DateAlbum => "date-album",
        }
    }
}

/// Longest album directory name we will create, in bytes.
const MAX_ALBUM_NAME_LEN: usize = 100;

/// Names Windows refuses to use for a file or directory, whatever the extension.
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
fn truncate_on_char_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Turn an album name from the takeout into a directory name that is safe on
/// every platform we support.
///
/// Album names are *user data*: they can contain path separators, start with a
/// dot, be a Windows device name, or be long enough to blow the filesystem's
/// name limit. Returns `None` when nothing usable is left, in which case the
/// caller must fall back to the date layout rather than inventing a folder.
///
/// A name that would collide with the structure the tool itself creates, such as a
/// four-digit year or `unknown-date`, gets an `_album` suffix instead of
/// being silently merged into the date tree.
pub fn sanitize_album_name(name: &str) -> Option<String> {
    let mut cleaned = String::with_capacity(name.len());
    for ch in name.chars() {
        // Path separators would escape the album directory; the rest are
        // characters Windows rejects outright.
        let safe = match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 || c == '\u{7f}' => '_',
            c => c,
        };
        cleaned.push(safe);
    }

    // Leading dots would create a hidden directory (and `.` / `..` would not be
    // a directory at all); Windows silently strips trailing dots and spaces.
    let cleaned = cleaned.trim_start_matches(['.', ' ']);
    let cleaned = truncate_on_char_boundary(cleaned, MAX_ALBUM_NAME_LEN);
    let cleaned = cleaned.trim_end_matches(['.', ' ']);
    if cleaned.is_empty() {
        return None;
    }

    let lower = cleaned.to_lowercase();
    let stem = lower.split('.').next().unwrap_or(lower.as_str());
    let looks_like_a_year = cleaned.len() == 4 && cleaned.bytes().all(|b| b.is_ascii_digit());
    if RESERVED_DEVICE_NAMES.contains(&stem) || lower == UNKNOWN_DATE_DIR || looks_like_a_year {
        return Some(format!("{}_album", cleaned));
    }

    Some(cleaned.to_string())
}

/// Key identifying a Live Photo pair: the *directory* the file lives in plus its
/// lower-cased file stem.
///
/// Keying on the bare stem (as this used to) collides across the whole takeout.
/// `Photos from 2015/IMG_0001.jpg` and `Photos from 2022/IMG_0001.jpg` are two
/// different photos, and walk order would otherwise decide which one won. A Live
/// Photo's `.MP4`/`.MOV` companion always sits next to its still image, so the
/// parent directory is part of the identity.
pub type LivePhotoKey = (PathBuf, String);

/// Map from [`LivePhotoKey`] to the still image's authoritative capture date.
pub type LivePhotoDates = HashMap<LivePhotoKey, DateTime<Utc>>;

/// Build the [`LivePhotoKey`] for a media path. Both the producer (the pre-pass
/// in `app.rs`) and the consumer ([`extract_photo_date`]) must go through this.
pub fn live_photo_key(path: &Path) -> LivePhotoKey {
    let parent = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let stem = path
        .file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| path.to_string_lossy().to_lowercase());
    (parent, stem)
}

/// Safety valve for the `_N` suffix search so a pathological directory cannot
/// spin forever.
const MAX_NAME_ATTEMPTS: u32 = 100_000;

/// Wall-clock time at which this run started.
///
/// Filesystem mtimes at or after this instant were (almost certainly) produced
/// by this run itself, such as an extraction step that did not preserve entry
/// mtimes, and are therefore meaningless as capture dates.
static RUN_START: LazyLock<DateTime<Utc>> = LazyLock::new(Utc::now);

/// The instant this run started; see [`RUN_START`].
pub fn run_start() -> DateTime<Utc> {
    *RUN_START
}

/// Pin the run-start instant. Call once, early, so that mtimes written later in
/// the run are correctly classified as "not meaningful".
pub fn mark_run_start() {
    let _ = run_start();
}

/// Anything older than this is assumed to be a bogus/zeroed timestamp rather
/// than a real capture date.
static EARLIEST_PLAUSIBLE: LazyLock<DateTime<Utc>> = LazyLock::new(|| {
    Utc.with_ymd_and_hms(1980, 1, 1, 0, 0, 0)
        .single()
        .expect("valid constant date")
});

/// The media path of a pair (local helper; `metadata.rs` is owned elsewhere).
fn pair_path(pair: &MediaMetadataPair) -> &Path {
    match pair {
        MediaMetadataPair::WithMetadata(path, ..) => path,
        MediaMetadataPair::WithoutMetadata(path) => path,
    }
}

/// Error type for file organization operations
#[derive(Debug)]
pub enum OrganizerError {
    DirectoryCreation(String),
    FileCopy(String),
    TimestampParse(String),
    FileSystemMetadata(String),
}

impl std::fmt::Display for OrganizerError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            OrganizerError::DirectoryCreation(msg) => {
                write!(f, "directory creation error: {}", msg)
            }
            OrganizerError::FileCopy(msg) => write!(f, "file copy error: {}", msg),
            OrganizerError::TimestampParse(msg) => write!(f, "timestamp parse error: {}", msg),
            OrganizerError::FileSystemMetadata(msg) => {
                write!(f, "file system metadata error: {}", msg)
            }
        }
    }
}

impl std::error::Error for OrganizerError {}

/// The resolved capture date of a media file.
///
/// `Unknown` means no *trustworthy* date exists: no sidecar timestamp, no Live
/// Photo mapping, and no meaningful filesystem mtime. Such files are filed under
/// [`UNKNOWN_DATE_DIR`] instead of being dumped into the current month.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoDate {
    Known(DateTime<Utc>),
    Unknown,
}

impl PhotoDate {
    /// The date, if one is known.
    pub fn known(self) -> Option<DateTime<Utc>> {
        match self {
            PhotoDate::Known(d) => Some(d),
            PhotoDate::Unknown => None,
        }
    }

    pub fn is_known(self) -> bool {
        matches!(self, PhotoDate::Known(_))
    }
}

impl From<Option<DateTime<Utc>>> for PhotoDate {
    fn from(value: Option<DateTime<Utc>>) -> Self {
        match value {
            Some(d) => PhotoDate::Known(d),
            None => PhotoDate::Unknown,
        }
    }
}

/// What the organizer did with one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Newly copied into the output tree.
    Organized,
    /// A byte-identical copy was already present, so nothing was written.
    Duplicate,
    /// Already recorded in the manifest by an earlier run, and the recorded
    /// output file is still there.
    Resumed,
    /// A Google-generated derivative, left behind because `--skip-derivatives`
    /// was given.
    DerivativeSkipped,
    /// `--dry-run`: this is where the file *would* have been copied.
    Planned,
    /// `--dry-run`: this file *would* have been skipped as a duplicate.
    PlannedDuplicate,
}

impl Disposition {
    /// Whether this disposition names a real (or planned) place in the library,
    /// i.e. whether [`FileOutcome::date`] and `destination` mean anything.
    pub fn places_a_file(self) -> bool {
        matches!(
            self,
            Disposition::Organized
                | Disposition::Duplicate
                | Disposition::Planned
                | Disposition::PlannedDuplicate
        )
    }

    /// Whether bytes were actually written for this file during this run.
    pub fn wrote_bytes(self) -> bool {
        self == Disposition::Organized
    }
}

/// What happened to one source file.
#[derive(Debug, Clone)]
pub struct FileOutcome {
    /// The source (temp-dir) path.
    pub source: PathBuf,
    /// Where it now lives in the output tree. For a skipped duplicate this is
    /// the pre-existing, identical file; for a resumed file it is what the
    /// manifest recorded. Empty for a skipped derivative.
    pub destination: PathBuf,
    /// The date used to file it. Only meaningful when
    /// [`Disposition::places_a_file`] is true. A file that was skipped before
    /// its date was resolved reports [`PhotoDate::Unknown`] because nothing was
    /// looked up, not because the date is unknowable.
    pub date: PhotoDate,
    /// What was done with the file.
    pub disposition: Disposition,
    /// blake3 content hash of the source, hex encoded, when one was computed.
    pub hash: Option<String>,
    /// Additional copies made under album folders (`date-album` mode).
    pub album_copies: Vec<PathBuf>,
    /// JSON sidecars copied next to the media file (`--copy-sidecars`).
    pub sidecars: Vec<PathBuf>,
    /// Non-fatal problems: the photo itself is in place, but something
    /// secondary (an album copy, a sidecar) did not work.
    pub warnings: Vec<String>,
}

impl FileOutcome {
    /// True when an identical file was already present and no copy was made.
    pub fn duplicate(&self) -> bool {
        matches!(
            self.disposition,
            Disposition::Duplicate | Disposition::PlannedDuplicate
        )
    }

    /// An outcome for a file that was skipped before it was ever placed.
    fn skipped(source: PathBuf, destination: PathBuf, disposition: Disposition) -> Self {
        FileOutcome {
            source,
            destination,
            date: PhotoDate::Unknown,
            disposition,
            hash: None,
            album_copies: Vec::new(),
            sidecars: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// Aggregate result of an organization run.
#[derive(Debug, Default)]
pub struct OrganizeSummary {
    /// Files newly copied into the output tree.
    pub organized: usize,
    /// Files skipped because a byte-identical copy was already present.
    pub duplicates_skipped: usize,
    /// Files filed under `unknown-date/` (a subset of the files placed).
    pub unknown_date: usize,
    /// Files skipped because the manifest says an earlier run already
    /// organized them and the recorded output file is still there.
    pub resumed_skips: usize,
    /// Files skipped by `--skip-derivatives`.
    pub derivatives_skipped: usize,
    /// Extra copies written under album folders (`date-album` mode).
    pub album_copies: usize,
    /// JSON sidecars copied next to their media file (`--copy-sidecars`).
    pub sidecars_copied: usize,
    /// `--dry-run`: files that would have been copied.
    pub planned: usize,
    /// `--dry-run`: files that would have been skipped as duplicates.
    pub planned_duplicates: usize,
    /// Per-file failures: (source path, error message).
    pub failures: Vec<(PathBuf, String)>,
    /// Per-file warnings: (source path, message). The photo is in place; a
    /// secondary copy or sidecar is not.
    pub warnings: Vec<(PathBuf, String)>,
    /// Sources skipped by `--skip-derivatives`, for the report.
    pub derivatives: Vec<PathBuf>,
    /// source -> destination mapping for every file that ended up in the output
    /// tree (including skipped duplicates and resumed files).
    pub destinations: Vec<(PathBuf, PathBuf)>,
    /// (content hash, destination) for every file placed in this run, ready to
    /// be written to the resume manifest.
    pub records: Vec<(String, PathBuf)>,
}

impl OrganizeSummary {
    /// Number of files that failed to be organized.
    pub fn failure_count(&self) -> usize {
        self.failures.len()
    }

    fn record(&mut self, outcome: FileOutcome) {
        let FileOutcome {
            source,
            destination,
            date,
            disposition,
            hash,
            album_copies,
            sidecars,
            warnings,
        } = outcome;

        match disposition {
            Disposition::Organized => self.organized += 1,
            Disposition::Duplicate => self.duplicates_skipped += 1,
            Disposition::Resumed => self.resumed_skips += 1,
            Disposition::Planned => self.planned += 1,
            Disposition::PlannedDuplicate => self.planned_duplicates += 1,
            Disposition::DerivativeSkipped => {
                self.derivatives_skipped += 1;
                self.derivatives.push(source);
                return;
            }
        }

        // Only count a file as undated when its date was actually resolved.
        if disposition.places_a_file() && !date.is_known() {
            self.unknown_date += 1;
        }

        self.album_copies += album_copies.len();
        self.sidecars_copied += sidecars.len();
        for warning in warnings {
            self.warnings.push((source.clone(), warning));
        }

        if matches!(disposition, Disposition::Organized | Disposition::Duplicate)
            && let Some(hash) = hash
        {
            self.records.push((hash, destination.clone()));
        }

        self.destinations.push((source, destination));
    }
}

/// Everything that varies between runs of the organizer.
///
/// Bundled into one struct so the per-file entry points keep a readable
/// signature as features accumulate.
#[derive(Clone, Copy)]
pub struct OrganizeOptions<'a> {
    /// The output layout to build.
    pub mode: OrganizeMode,
    /// Root of the extraction tree, used to derive album names. Without it no
    /// file has an album and the album modes fall back to the date layout.
    pub extract_root: Option<&'a Path>,
    /// Work out what would happen without writing anything.
    pub dry_run: bool,
    /// Skip byte-identical duplicates. `--no-dedup` clears this; the `_N`
    /// collision loop still runs, so nothing is ever overwritten.
    pub dedup: bool,
    /// Copy each media file's JSON sidecar next to the organized copy.
    pub copy_sidecars: bool,
    /// Leave Google-generated derivatives such as `-edited` and `-pano` behind.
    pub skip_derivatives: bool,
    /// Manifest from an earlier run. Files whose content it already records
    /// are skipped. `None` for `--force`, `--dry-run`, or a first run.
    pub resume: Option<&'a Manifest>,
    /// Compute a content hash for every file placed, so the run can be recorded
    /// in the manifest. Costs nothing extra: the hash is taken from the bytes
    /// as they are copied.
    pub record_manifest: bool,
}

impl Default for OrganizeOptions<'_> {
    fn default() -> Self {
        OrganizeOptions {
            mode: OrganizeMode::Date,
            extract_root: None,
            dry_run: false,
            dedup: true,
            copy_sidecars: false,
            skip_derivatives: false,
            resume: None,
            record_manifest: false,
        }
    }
}

/// A cached content hash together with the (size, mtime) it was computed for.
type HashEntry = (u64, i64, [u8; 32]);

/// Per-run caches shared by all worker threads.
///
/// * `dirs`: directories already passed to `create_dir_all` (#48: once per
///   directory, not once per file).
/// * `hashes`: blake3 content hashes computed during this run, keyed by path
///   and (size, mtime) so a stale entry cannot be reused.
/// * `inflight`: destination names this run is currently writing. Without this,
///   a thread that loses the `create_new` race could hash a half-written file and
///   wrongly conclude it is a *different* photo, producing a spurious `_1` copy.
/// * `planned`: destination names a `--dry-run` has already assigned, mapped to
///   the source that claimed them. Nothing is created on disk during a dry run,
///   so this map is the only thing that stops every copy of the same photo from
///   reporting the same destination as free.
#[derive(Default)]
pub struct OrganizeContext {
    dirs: Mutex<HashSet<PathBuf>>,
    hashes: Mutex<HashMap<PathBuf, HashEntry>>,
    inflight: Mutex<HashSet<PathBuf>>,
    inflight_done: Condvar,
    planned: Mutex<HashMap<PathBuf, PathBuf>>,
}

impl OrganizeContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to become the writer of `path`.
    ///
    /// Returns `true` if this thread now owns the name (and must call
    /// [`Self::release`] when done). Returns `false` after waiting for the
    /// current owner to finish, at which point the file on disk is complete.
    fn claim(&self, path: &Path) -> bool {
        let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if inflight.insert(path.to_path_buf()) {
            return true;
        }
        while inflight.contains(path) {
            inflight = self
                .inflight_done
                .wait(inflight)
                .unwrap_or_else(|e| e.into_inner());
        }
        false
    }

    fn release(&self, path: &Path) {
        let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        inflight.remove(path);
        drop(inflight);
        self.inflight_done.notify_all();
    }

    /// `create_dir_all`, but only the first time this path is seen.
    fn ensure_dir(&self, dir: &Path) -> Result<(), OrganizerError> {
        {
            let seen = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
            if seen.contains(dir) {
                return Ok(());
            }
        }
        create_date_directory(dir)?;
        let mut seen = self.dirs.lock().unwrap_or_else(|e| e.into_inner());
        seen.insert(dir.to_path_buf());
        Ok(())
    }

    fn cached_hash(&self, path: &Path, len: u64, mtime: i64) -> Option<[u8; 32]> {
        let cache = self.hashes.lock().unwrap_or_else(|e| e.into_inner());
        match cache.get(path) {
            Some((l, m, h)) if *l == len && *m == mtime => Some(*h),
            _ => None,
        }
    }

    fn store_hash(&self, path: &Path, len: u64, mtime: i64, hash: [u8; 32]) {
        let mut cache = self.hashes.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(path.to_path_buf(), (len, mtime, hash));
    }

    /// Reserve a destination name during a dry run.
    ///
    /// Returns `None` when `source` now owns the name, or `Some(other)` naming
    /// the source that already claimed it.
    fn claim_planned(&self, path: &Path, source: &Path) -> Option<PathBuf> {
        let mut planned = self.planned.lock().unwrap_or_else(|e| e.into_inner());
        match planned.get(path) {
            Some(existing) => Some(existing.clone()),
            None => {
                planned.insert(path.to_path_buf(), source.to_path_buf());
                None
            }
        }
    }
}

/// Organize media files chronologically based on their metadata or file system dates
pub fn organize_media_files(
    media_metadata_pairs: Vec<MediaMetadataPair>,
    output_path: &Path,
    live_photo_dates: &LivePhotoDates,
) -> Result<OrganizeSummary, Box<dyn std::error::Error>> {
    organize_media_files_with_options(
        media_metadata_pairs,
        output_path,
        live_photo_dates,
        &OrganizeOptions::default(),
    )
}

/// Organize media files, honouring the run's [`OrganizeOptions`].
pub fn organize_media_files_with_options(
    media_metadata_pairs: Vec<MediaMetadataPair>,
    output_path: &Path,
    live_photo_dates: &LivePhotoDates,
    options: &OrganizeOptions<'_>,
) -> Result<OrganizeSummary, Box<dyn std::error::Error>> {
    info!(
        "Starting file organization phase for {} files (layout: {}, dry run: {})",
        media_metadata_pairs.len(),
        options.mode.as_str(),
        options.dry_run
    );

    // Create a progress bar for file organization
    let organize_pb = ProgressBar::new(media_metadata_pairs.len() as u64);
    let label = if options.dry_run { "Plan" } else { "Organize" };
    organize_pb.set_style(
        ProgressStyle::default_bar()
            .template(&format!("  {{spinner:.green}} {} [{{bar:20.cyan/blue}}] files {{pos}}/{{len}} | {{elapsed_precise}} | ETA {{eta}}", label))?
            .progress_chars("#>-")
    );

    let context = OrganizeContext::new();

    // Process each media file in parallel. `None` means the item was skipped
    // because a shutdown was requested.
    let results: Vec<Option<(PathBuf, Result<FileOutcome, OrganizerError>)>> = media_metadata_pairs
        .into_par_iter()
        .map(|pair| {
            if crate::is_shutdown() {
                return None;
            }
            let source = pair_path(&pair).to_path_buf();
            let result = organize_one(pair, output_path, live_photo_dates, &context, options);
            organize_pb.inc(1);
            Some((source, result))
        })
        .collect();

    let mut summary = OrganizeSummary::default();
    let mut interrupted = 0usize;

    for entry in results {
        match entry {
            None => interrupted += 1,
            Some((_source, Ok(outcome))) => summary.record(outcome),
            Some((source, Err(e))) => {
                let msg = e.to_string();
                error!("Failed to organize {}: {}", source.display(), msg);
                summary.failures.push((source, msg));
            }
        }
    }

    if interrupted > 0 {
        warn!(
            "Organization interrupted: {} files were not processed",
            interrupted
        );
    }

    organize_pb.finish_with_message(if options.dry_run {
        format!(
            "Dry run completed: {} files would be organized, {} would be skipped as duplicates, {} undated, {} errors",
            summary.planned, summary.planned_duplicates, summary.unknown_date, summary.failures.len()
        )
    } else {
        format!(
            "Organization completed: {} organized, {} duplicates skipped, {} undated, {} errors",
            summary.organized,
            summary.duplicates_skipped,
            summary.unknown_date,
            summary.failures.len()
        )
    });

    Ok(summary)
}

/// Organize a single file based on its date, using a throwaway context.
pub fn organize_single_file(
    pair: MediaMetadataPair,
    output_path: &Path,
    live_photo_dates: &LivePhotoDates,
) -> Result<FileOutcome, OrganizerError> {
    let context = OrganizeContext::new();
    organize_single_file_with_context(pair, output_path, live_photo_dates, &context)
}

/// Organize a single file, sharing directory/hash caches through `context`.
pub fn organize_single_file_with_context(
    pair: MediaMetadataPair,
    output_path: &Path,
    live_photo_dates: &LivePhotoDates,
    context: &OrganizeContext,
) -> Result<FileOutcome, OrganizerError> {
    organize_one(
        pair,
        output_path,
        live_photo_dates,
        context,
        &OrganizeOptions::default(),
    )
}

/// The output directory a file's *primary* copy belongs in.
///
/// `date-album` files the primary copy chronologically. The album copy is an
/// addition, so renaming or deleting an album cannot remove the only organized
/// copy of a photo.
fn primary_output_dir(
    output_path: &Path,
    date: PhotoDate,
    album: Option<&str>,
    mode: OrganizeMode,
) -> PathBuf {
    match mode {
        // Files with no trustworthy date must not pollute the current month.
        OrganizeMode::Date | OrganizeMode::DateAlbum => output_path.join(destination_subdir(date)),
        OrganizeMode::Album => match album {
            Some(album) => output_path.join(album),
            // Most of a takeout is not in any album; those files still need a
            // sensible home, so they keep the date layout.
            None => output_path.join(destination_subdir(date)),
        },
        OrganizeMode::Flat => output_path.to_path_buf(),
    }
}

/// Organize a single file, honouring the run's [`OrganizeOptions`].
pub fn organize_one(
    pair: MediaMetadataPair,
    output_path: &Path,
    live_photo_dates: &LivePhotoDates,
    context: &OrganizeContext,
    options: &OrganizeOptions<'_>,
) -> Result<FileOutcome, OrganizerError> {
    let source = pair_path(&pair).to_path_buf();

    // `--skip-derivatives`: leave Google's own generated copies behind. Off by
    // default on purpose. The heuristic is a name match, and a photo the user
    // called `sunset-pano.jpg` is not Google's to discard.
    if options.skip_derivatives {
        let file_name = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if crate::metadata::is_derivative(&file_name) {
            debug!("Skipping derivative {}", source.display());
            return Ok(FileOutcome::skipped(
                source,
                PathBuf::new(),
                Disposition::DerivativeSkipped,
            ));
        }
    }

    // Resume: if a previous run already put this exact content somewhere and
    // that file is still there, there is nothing to do. Identity is the content
    // hash, not the path: the source lives in a scratch directory whose name is
    // different every run.
    let mut source_hash: Option<[u8; 32]> = None;
    if let Some(manifest) = options.resume {
        match hash_file_cached(&source, Some(context)) {
            Ok(hash) => {
                let hex = crate::dedup::hash_to_hex(&hash);
                if let Some(destination) = manifest.resume_destination(output_path, &hex) {
                    debug!(
                        "Resuming: {} is already organized at {}",
                        source.display(),
                        destination.display()
                    );
                    let mut outcome =
                        FileOutcome::skipped(source, destination, Disposition::Resumed);
                    outcome.hash = Some(hex);
                    return Ok(outcome);
                }
                source_hash = Some(hash);
            }
            Err(e) => debug!(
                "Could not hash {} for the resume check: {}; processing it",
                source.display(),
                e
            ),
        }
    }

    // Grab the sidecar path before the pair is consumed by the date lookup.
    let json_path = pair.json_path().map(|p| p.to_path_buf());

    // Extract the date for the file
    let (file_path, date) = extract_photo_date(pair, live_photo_dates)?;

    let album = options
        .extract_root
        .and_then(|root| crate::metadata::extract_album_name(&file_path, root))
        .and_then(|name| sanitize_album_name(&name));

    let organized_dir = primary_output_dir(output_path, date, album.as_deref(), options.mode);

    let place_mode = if options.dry_run {
        PlaceMode::Plan
    } else {
        PlaceMode::Copy
    };
    if place_mode == PlaceMode::Copy {
        context.ensure_dir(&organized_dir)?;
    }

    let placement = Placement {
        context: Some(context),
        dedup: options.dedup,
        mode: place_mode,
        known_hash: source_hash,
        hash_output: options.record_manifest || options.resume.is_some(),
    };

    // Copy the file to the organized location (atomically claiming its name,
    // skipping byte-identical duplicates).
    let copy = place_file(&file_path, &organized_dir, placement)?;

    // Propagate the resolved date to the output copy's mtime. `fs::copy`'s mtime
    // behaviour is platform dependent, and for formats we cannot write EXIF into
    // the mtime is the only date carrier we have.
    if place_mode == PlaceMode::Copy
        && !copy.duplicate
        && let PhotoDate::Known(d) = date
    {
        set_file_mtime(&copy.destination, &d);
    }

    let mut outcome = FileOutcome {
        source: file_path.clone(),
        destination: copy.destination.clone(),
        date,
        disposition: match (place_mode, copy.duplicate) {
            (PlaceMode::Copy, false) => Disposition::Organized,
            (PlaceMode::Copy, true) => Disposition::Duplicate,
            (PlaceMode::Plan, false) => Disposition::Planned,
            (PlaceMode::Plan, true) => Disposition::PlannedDuplicate,
        },
        hash: source_hash
            .or(copy.hash)
            .map(|h| crate::dedup::hash_to_hex(&h)),
        album_copies: Vec::new(),
        sidecars: Vec::new(),
        warnings: Vec::new(),
    };

    if options.copy_sidecars {
        copy_sidecar_for(
            &json_path,
            &file_path,
            &copy.destination,
            place_mode,
            &mut outcome,
        );
    }

    // `date-album`: an extra copy under the album folder, so the album survives
    // without the chronology losing the photo.
    if options.mode == OrganizeMode::DateAlbum
        && let Some(album) = &album
    {
        let album_dir = output_path.join(album);
        match place_album_copy(&file_path, &album_dir, date, placement, context) {
            Ok(Some(destination)) => {
                if options.copy_sidecars {
                    copy_sidecar_for(
                        &json_path,
                        &file_path,
                        &destination,
                        place_mode,
                        &mut outcome,
                    );
                }
                outcome.album_copies.push(destination);
            }
            Ok(None) => {}
            // The photo itself is in place; a missing album copy is
            // worth reporting but must not fail the file (and must not stop
            // it being recorded in the manifest).
            Err(e) => outcome.warnings.push(format!(
                "could not place the album copy in {}: {}",
                album_dir.display(),
                e
            )),
        }
    }

    Ok(outcome)
}

/// Place the album-folder copy, returning where it went (or `None` when an
/// identical copy was already there).
fn place_album_copy(
    file_path: &Path,
    album_dir: &Path,
    date: PhotoDate,
    placement: Placement<'_>,
    context: &OrganizeContext,
) -> Result<Option<PathBuf>, OrganizerError> {
    if placement.mode == PlaceMode::Copy {
        context.ensure_dir(album_dir)?;
    }
    let copy = place_file(file_path, album_dir, placement)?;
    if copy.duplicate {
        return Ok(None);
    }
    if placement.mode == PlaceMode::Copy
        && let PhotoDate::Known(d) = date
    {
        set_file_mtime(&copy.destination, &d);
    }
    Ok(Some(copy.destination))
}

/// Copy a sidecar next to `destination`, recording the result on `outcome`.
///
/// In [`PlaceMode::Plan`] the target name is worked out and reported but no
/// file is written, so a `--dry-run --copy-sidecars` still reports the count.
fn copy_sidecar_for(
    json_path: &Option<PathBuf>,
    source_media: &Path,
    destination: &Path,
    mode: PlaceMode,
    outcome: &mut FileOutcome,
) {
    let Some(json_path) = json_path else {
        return;
    };
    match copy_sidecar(json_path, source_media, destination, mode) {
        Ok(Some(path)) => outcome.sidecars.push(path),
        Ok(None) => {}
        Err(e) => {
            let message = format!("could not copy sidecar {}: {}", json_path.display(), e);
            warn!("{}", message);
            outcome.warnings.push(message);
        }
    }
}

/// The name a sidecar should take next to the organized media file.
///
/// Google's sidecars are named after the media file plus a suffix
/// (`IMG_0001.jpg.supplemental-metadata.json`), so the faithful rule is to keep
/// that suffix and rebase it onto the media file's **final** name, including
/// any `_N` the collision loop added, so the two stay paired.
///
/// When the sidecar name is not simply the media name plus a suffix, Google
/// truncates long names at 46 characters and moves the `(N)` duplicate counter
/// around, and `-edited` copies inherit the base photo's sidecar. In those cases we fall back
/// to `<final media name>.json`, which is still unambiguous and still pairs.
pub fn sidecar_destination_name(
    json_name: &str,
    source_media_name: &str,
    dest_media_name: &str,
) -> String {
    let matches_prefix = json_name.len() > source_media_name.len()
        && json_name
            .get(..source_media_name.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(source_media_name));

    if matches_prefix {
        format!(
            "{}{}",
            dest_media_name,
            &json_name[source_media_name.len()..]
        )
    } else {
        format!("{}.json", dest_media_name)
    }
}

/// Copy a JSON sidecar next to the organized media file.
///
/// Returns the path written, or `None` when a sidecar of that name was already
/// there (a re-run, or the sidecar shared by an `-edited` pair).
fn copy_sidecar(
    json_path: &Path,
    source_media: &Path,
    dest_media: &Path,
    mode: PlaceMode,
) -> Result<Option<PathBuf>, io::Error> {
    let name_of = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    let dir = dest_media
        .parent()
        .ok_or_else(|| io::Error::other("organized file has no parent directory"))?;
    let target = dir.join(sidecar_destination_name(
        &name_of(json_path),
        &name_of(source_media),
        &name_of(dest_media),
    ));

    if mode == PlaceMode::Plan {
        // Report what would be written, but check the sidecar is actually
        // readable first so a dry run cannot promise a copy that would fail.
        File::open(json_path)?;
        return Ok(if target.exists() { None } else { Some(target) });
    }

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
    {
        Ok(mut out) => {
            let result = (|| -> io::Result<()> {
                let mut reader = File::open(json_path)?;
                io::copy(&mut reader, &mut out)?;
                out.flush()
            })();
            drop(out);
            if let Err(e) = result {
                let _ = fs::remove_file(&target);
                return Err(e);
            }
            Ok(Some(target))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(e) => Err(e),
    }
}

/// Best-effort mtime propagation; a failure here is logged, not fatal.
fn set_file_mtime(path: &Path, date: &DateTime<Utc>) {
    let ft = filetime::FileTime::from_unix_time(date.timestamp(), date.timestamp_subsec_nanos());
    if let Err(e) = filetime::set_file_mtime(path, ft) {
        warn!(
            "Failed to set modification time on {}: {}",
            path.display(),
            e
        );
    }
}

/// Extract photo date from a [`MediaMetadataPair`].
///
/// Priority: the file's own `photoTakenTime`, then its own `creationTime`, the
/// Live Photo mapping (videos only), and finally a meaningful filesystem mtime.
/// If none yields a trustworthy date, the result is [`PhotoDate::Unknown`]
/// instead of `Utc::now()`.
///
/// The Live Photo map ranks below the file's own sidecar. A video that has its
/// own metadata must not be overridden by the still image with the same stem.
pub fn extract_photo_date(
    pair: MediaMetadataPair,
    live_photo_dates: &LivePhotoDates,
) -> Result<(PathBuf, PhotoDate), OrganizerError> {
    let path = pair_path(&pair).to_path_buf();

    // Check 1 (JSON sidecar timestamps)
    if let MediaMetadataPair::WithMetadata(_, metadata, _) = &pair {
        // photoTakenTime is authoritative
        if let Some(ts) = metadata
            .photo_taken_time
            .as_ref()
            .and_then(|t| t.timestamp.as_ref())
        {
            match parse_unix_timestamp(ts) {
                Ok(date) => return Ok((path, PhotoDate::Known(date))),
                Err(e) => debug!(
                    "Failed to parse photoTakenTime for {}: {}. Trying creationTime.",
                    path.display(),
                    e
                ),
            }
        }

        // creationTime is a weaker but still real signal.
        if let Some(ts) = metadata
            .creation_time
            .as_ref()
            .and_then(|t| t.timestamp.as_ref())
        {
            match parse_unix_timestamp(ts) {
                Ok(date) => return Ok((path, PhotoDate::Known(date))),
                Err(e) => debug!(
                    "Failed to parse creationTime for {}: {}. Trying file system date.",
                    path.display(),
                    e
                ),
            }
        }
    }

    // Check 2 (Live Photo): a video with no usable sidecar of its own borrows
    // the date of the still image sitting next to it with the same stem.
    if let Some(extension) = path.extension() {
        let ext_str = extension.to_string_lossy().to_lowercase();
        if (ext_str == "mp4" || ext_str == "mov")
            && let Some(&date) = live_photo_dates.get(&live_photo_key(&path))
        {
            return Ok((path, PhotoDate::Known(date)));
        }
    }

    // Check 3: the filesystem mtime, but only when it can plausibly mean
    // something (archive extraction preserves entry mtimes).
    Ok((path.clone(), meaningful_filesystem_date(&path).into()))
}

/// The file's mtime, if it can plausibly be a capture date.
///
/// Rejects timestamps produced by this run itself (extraction/EXIF rewrites) and
/// obviously bogus values (epoch-ish or in the future).
pub fn meaningful_filesystem_date(path: &Path) -> Option<DateTime<Utc>> {
    let date = match get_file_creation_date(path) {
        Ok(d) => d,
        Err(e) => {
            debug!(
                "No filesystem date for {}: {} (filing as unknown-date)",
                path.display(),
                e
            );
            return None;
        }
    };

    if date < *EARLIEST_PLAUSIBLE {
        debug!(
            "Filesystem date {} for {} is implausibly old; treating as unknown",
            date,
            path.display()
        );
        return None;
    }

    // "now-ish" means written during this run and carries no information.
    if date + chrono::Duration::seconds(2) >= run_start() {
        debug!(
            "Filesystem date {} for {} was written by this run; treating as unknown",
            date,
            path.display()
        );
        return None;
    }

    Some(date)
}

/// Parse Unix timestamp string from PhotoMetadata to DateTime
pub fn parse_unix_timestamp(timestamp: &str) -> Result<DateTime<Utc>, OrganizerError> {
    debug!("Parsing timestamp: {}", timestamp);

    // Parse the timestamp as i64
    let unix_timestamp: i64 = timestamp
        .parse::<i64>()
        .map_err(|e| OrganizerError::TimestampParse(e.to_string()))?;

    // Convert to DateTime<Utc>
    let datetime = DateTime::<Utc>::from_timestamp(unix_timestamp, 0)
        .ok_or_else(|| OrganizerError::TimestampParse("Invalid timestamp value".to_string()))?;

    Ok(datetime)
}

/// Get the file modification date, with creation time as a last-resort fallback.
///
/// Archive extraction preserves modification times. On Windows,
/// creation time instead describes when the archive entry was extracted, so it
/// must not take precedence over the preserved timestamp.
pub fn get_file_creation_date(path: &Path) -> Result<DateTime<Utc>, OrganizerError> {
    debug!("Getting file system date for: {}", path.display());

    let metadata =
        fs::metadata(path).map_err(|e| OrganizerError::FileSystemMetadata(e.to_string()))?;

    let filesystem_time = metadata
        .modified()
        .or_else(|_| metadata.created())
        .map_err(|e| OrganizerError::FileSystemMetadata(e.to_string()))?;
    let duration = filesystem_time
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| OrganizerError::FileSystemMetadata(e.to_string()))?;

    let datetime =
        DateTime::<Utc>::from_timestamp(duration.as_secs() as i64, 0).ok_or_else(|| {
            OrganizerError::FileSystemMetadata("Invalid filesystem timestamp".to_string())
        })?;

    Ok(datetime)
}

/// Convert DateTime to YYYY/MM path format
pub fn format_date_path(date: &DateTime<Utc>) -> String {
    date.format("%Y/%m").to_string()
}

/// The output sub-path a given date maps to (`YYYY/MM` or `unknown-date`).
pub fn destination_subdir(date: PhotoDate) -> PathBuf {
    match date {
        PhotoDate::Known(d) => PathBuf::from(format_date_path(&d)),
        PhotoDate::Unknown => PathBuf::from(UNKNOWN_DATE_DIR),
    }
}

/// Create YYYY/MM directory structure in output path based on date
pub fn create_date_directory(dir_path: &Path) -> Result<(), OrganizerError> {
    debug!("Creating directory: {}", dir_path.display());

    fs::create_dir_all(dir_path).map_err(|e| {
        OrganizerError::DirectoryCreation(format!(
            "Failed to create directory {}: {}",
            dir_path.display(),
            e
        ))
    })?;

    Ok(())
}

/// Result of placing one file into the output tree.
#[derive(Debug, Clone)]
pub struct CopyOutcome {
    /// The file in the output tree (freshly written, or the identical one that
    /// was already there).
    pub destination: PathBuf,
    /// True when nothing was copied because an identical file already existed.
    pub duplicate: bool,
    /// blake3 hash of the source, when the placement computed one.
    pub hash: Option<[u8; 32]>,
}

/// Whether a placement writes bytes or only works out where they would go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaceMode {
    /// Actually write the file.
    Copy,
    /// `--dry-run`: decide the destination without touching the output tree.
    Plan,
}

/// How one file should be placed.
#[derive(Clone, Copy)]
struct Placement<'a> {
    /// Shared per-run caches and the in-flight/planned name registries.
    context: Option<&'a OrganizeContext>,
    /// Skip byte-identical duplicates instead of writing an `_N` copy.
    dedup: bool,
    mode: PlaceMode,
    /// The source hash, when the caller already computed it.
    known_hash: Option<[u8; 32]>,
    /// Hash the bytes as they are copied, so the run can be recorded in the
    /// manifest without a second pass over the file.
    hash_output: bool,
}

impl Default for Placement<'_> {
    fn default() -> Self {
        Placement {
            context: None,
            dedup: true,
            mode: PlaceMode::Copy,
            known_hash: None,
            hash_output: false,
        }
    }
}

/// A writer that hashes everything passing through it.
///
/// Lets the copy produce the manifest's content hash for free, instead of
/// reading every file a second time just to hash it.
struct HashingWriter<W: Write> {
    inner: W,
    hasher: blake3::Hasher,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hasher.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Copy a file into `dir_path`, claiming its name atomically and skipping
/// byte-identical duplicates.
pub fn copy_file_to_organized_location(
    file_path: &Path,
    dir_path: &Path,
) -> Result<CopyOutcome, OrganizerError> {
    copy_file_with_context(file_path, dir_path, None)
}

/// Copy a file into `dir_path`, sharing this run's caches through `context`.
pub fn copy_file_with_context(
    file_path: &Path,
    dir_path: &Path,
    context: Option<&OrganizeContext>,
) -> Result<CopyOutcome, OrganizerError> {
    place_file(
        file_path,
        dir_path,
        Placement {
            context,
            ..Placement::default()
        },
    )
}

/// Core placement routine.
///
/// The destination name is claimed with `create_new(true)`, which is atomic, so
/// two threads racing on `IMG_0001.jpg` cannot both win. On `AlreadyExists`
/// the existing file is compared by size and then blake3 content hash: identical
/// content means the file is already in the library (Takeout emits the same photo
/// in "Photos from YYYY" *and* in every album) and is skipped; different content
/// bumps the `_N` counter and retries.
///
/// With `dedup` off the content comparison is skipped entirely, so an existing
/// file is never treated as the same photo. The `_N` loop still runs, so
/// nothing is ever overwritten.
///
/// In [`PlaceMode::Plan`] nothing is created: names are checked against the
/// filesystem and against the destinations this run has already planned, which
/// is what makes a dry run's duplicate count match what a real run would do.
fn place_file(
    file_path: &Path,
    dir_path: &Path,
    placement: Placement<'_>,
) -> Result<CopyOutcome, OrganizerError> {
    debug!(
        "Placing file {} into {} ({:?})",
        file_path.display(),
        dir_path.display(),
        placement.mode
    );

    let context = placement.context;

    let file_name = file_path
        .file_name()
        .ok_or_else(|| OrganizerError::FileCopy("Invalid file path".to_string()))?
        .to_os_string();
    let file_stem = file_path
        .file_stem()
        .ok_or_else(|| OrganizerError::FileCopy("Invalid file stem".to_string()))?
        .to_string_lossy()
        .to_string();
    let file_extension = file_path
        .extension()
        .map(|ext| ext.to_string_lossy().to_string())
        .unwrap_or_default();

    let source_len = fs::metadata(file_path)
        .map_err(|e| {
            OrganizerError::FileCopy(format!(
                "Failed to stat source {}: {}",
                file_path.display(),
                e
            ))
        })?
        .len();

    let mut counter: u32 = 0;
    loop {
        let candidate = if counter == 0 {
            dir_path.join(&file_name)
        } else {
            dir_path.join(candidate_name(&file_stem, &file_extension, counter))
        };

        if placement.mode == PlaceMode::Plan {
            match plan_candidate(file_path, source_len, &candidate, placement)? {
                Some(outcome) => return Ok(outcome),
                None => {
                    counter = bump_counter(counter, file_path, dir_path)?;
                    continue;
                }
            }
        }

        // Coordinate with the other threads of this run: if one of them is
        // already writing this exact name, wait for it to finish so we never
        // compare against a half-written file.
        if let Some(ctx) = context
            && !ctx.claim(&candidate)
        {
            // The owner finished; the file on disk is now complete. Retry
            // this same name, which will now take the AlreadyExists path.
            continue;
        }

        let opened = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate);

        match opened {
            Ok(mut dest) => {
                // We exclusively own this name; stream the bytes through the
                // handle we just created, hashing them on the way when the
                // manifest needs the hash.
                let want_hash = placement.hash_output && placement.known_hash.is_none();
                let mut hash = placement.known_hash;
                let copy_result = (|| -> io::Result<()> {
                    let mut reader = File::open(file_path)?;
                    if want_hash {
                        let mut writer = HashingWriter {
                            inner: &mut dest,
                            hasher: blake3::Hasher::new(),
                        };
                        io::copy(&mut reader, &mut writer)?;
                        writer.flush()?;
                        hash = Some(*writer.hasher.finalize().as_bytes());
                    } else {
                        io::copy(&mut reader, &mut dest)?;
                        dest.flush()?;
                    }
                    Ok(())
                })();
                drop(dest);

                if let Err(e) = copy_result {
                    let _ = fs::remove_file(&candidate);
                    if let Some(ctx) = context {
                        ctx.release(&candidate);
                    }
                    return Err(OrganizerError::FileCopy(format!(
                        "Failed to copy {} to {}: {}",
                        file_path.display(),
                        candidate.display(),
                        e
                    )));
                }

                if let Some(ctx) = context {
                    // The source is untouched by the copy, so its digest is
                    // reusable for the rest of the run (e.g. the album copy).
                    if let Some(hash) = hash {
                        store_source_hash(file_path, hash, ctx);
                    }
                    ctx.release(&candidate);
                }
                return Ok(CopyOutcome {
                    destination: candidate,
                    duplicate: false,
                    hash,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if let Some(ctx) = context {
                    ctx.release(&candidate);
                }
                // Somebody (this run, or a previous one) already put a file
                // here. If it is the same file, we are done.
                if placement.dedup {
                    match files_are_identical(file_path, source_len, &candidate, context) {
                        Ok(true) => {
                            debug!(
                                "Skipping {}: identical content already at {}",
                                file_path.display(),
                                candidate.display()
                            );
                            // `files_are_identical` just hashed the source, so
                            // this is a cache hit rather than a second read.
                            let hash = placement
                                .known_hash
                                .or_else(|| hash_file_cached(file_path, context).ok());
                            return Ok(CopyOutcome {
                                destination: candidate,
                                duplicate: true,
                                hash,
                            });
                        }
                        Ok(false) => {}
                        Err(err) => {
                            debug!(
                                "Could not compare {} with {}: {}; treating as distinct",
                                file_path.display(),
                                candidate.display(),
                                err
                            );
                        }
                    }
                }

                counter = bump_counter(counter, file_path, dir_path)?;
            }
            Err(e) => {
                if let Some(ctx) = context {
                    ctx.release(&candidate);
                }
                return Err(OrganizerError::FileCopy(format!(
                    "Failed to create {}: {}",
                    candidate.display(),
                    e
                )));
            }
        }
    }
}

/// Advance the `_N` counter, refusing to spin forever.
fn bump_counter(counter: u32, file_path: &Path, dir_path: &Path) -> Result<u32, OrganizerError> {
    let next = counter + 1;
    if next > MAX_NAME_ATTEMPTS {
        return Err(OrganizerError::FileCopy(format!(
            "Could not find a free filename for {} in {}",
            file_path.display(),
            dir_path.display()
        )));
    }
    Ok(next)
}

/// Decide whether `candidate` is the destination a dry run would use.
///
/// `Ok(Some(outcome))` settles the name; `Ok(None)` means "taken by something
/// different, try the next `_N`."
fn plan_candidate(
    file_path: &Path,
    source_len: u64,
    candidate: &Path,
    placement: Placement<'_>,
) -> Result<Option<CopyOutcome>, OrganizerError> {
    let context = placement.context;

    // A file already in the library from an earlier run.
    if candidate.exists() {
        if placement.dedup {
            match files_are_identical(file_path, source_len, candidate, context) {
                Ok(true) => {
                    return Ok(Some(CopyOutcome {
                        destination: candidate.to_path_buf(),
                        duplicate: true,
                        hash: placement.known_hash,
                    }));
                }
                Ok(false) => {}
                Err(e) => debug!(
                    "Could not compare {} with {}: {}; treating as distinct",
                    file_path.display(),
                    candidate.display(),
                    e
                ),
            }
        }
        return Ok(None);
    }

    // Nothing is written during a dry run, so the only thing that can make a
    // name unavailable is another file this run has already planned there.
    let Some(ctx) = context else {
        return Ok(Some(CopyOutcome {
            destination: candidate.to_path_buf(),
            duplicate: false,
            hash: placement.known_hash,
        }));
    };

    match ctx.claim_planned(candidate, file_path) {
        None => Ok(Some(CopyOutcome {
            destination: candidate.to_path_buf(),
            duplicate: false,
            hash: placement.known_hash,
        })),
        Some(other) => {
            if placement.dedup {
                match files_are_identical(file_path, source_len, &other, context) {
                    Ok(true) => {
                        return Ok(Some(CopyOutcome {
                            destination: candidate.to_path_buf(),
                            duplicate: true,
                            hash: placement.known_hash,
                        }));
                    }
                    Ok(false) => {}
                    Err(e) => debug!(
                        "Could not compare {} with {}: {}; treating as distinct",
                        file_path.display(),
                        other.display(),
                        e
                    ),
                }
            }
            Ok(None)
        }
    }
}

/// Remember a digest computed while copying, keyed the same way as
/// [`hash_file_cached`] so later lookups hit.
fn store_source_hash(path: &Path, hash: [u8; 32], ctx: &OrganizeContext) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    ctx.store_hash(path, meta.len(), mtime, hash);
}

fn candidate_name(stem: &str, extension: &str, counter: u32) -> String {
    if extension.is_empty() {
        format!("{}_{}", stem, counter)
    } else {
        format!("{}_{}.{}", stem, counter, extension)
    }
}

/// Size check first (different size means a different file, so no hash is
/// needed), then blake3 content hashes.
fn files_are_identical(
    source: &Path,
    source_len: u64,
    existing: &Path,
    context: Option<&OrganizeContext>,
) -> Result<bool, io::Error> {
    let existing_meta = fs::metadata(existing)?;
    if existing_meta.len() != source_len {
        return Ok(false);
    }

    let source_hash = hash_file_cached(source, context)?;
    let existing_hash = hash_file_cached(existing, context)?;
    Ok(source_hash == existing_hash)
}

fn hash_file_cached(path: &Path, context: Option<&OrganizeContext>) -> Result<[u8; 32], io::Error> {
    let meta = fs::metadata(path)?;
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    if let Some(ctx) = context
        && let Some(hash) = ctx.cached_hash(path, len, mtime)
    {
        return Ok(hash);
    }

    let hash = hash_file(path)?;

    if let Some(ctx) = context {
        ctx.store_hash(path, len, mtime, hash);
    }

    Ok(hash)
}

/// blake3 content hash of a file.
pub fn hash_file(path: &Path) -> Result<[u8; 32], io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(*hasher.finalize().as_bytes())
}

/// Generate a unique filename by appending numbers when conflicts occur.
///
/// NOTE: this is inherently racy and is kept only for callers that want to
/// *preview* a destination name. The actual copy path claims its name atomically
/// via [`copy_file_with_context`]; do not use this to decide where to write.
pub fn generate_unique_filename(
    file_path: &Path,
    dir_path: &Path,
) -> Result<PathBuf, OrganizerError> {
    let file_name = file_path
        .file_name()
        .ok_or_else(|| OrganizerError::FileCopy("Invalid file path".to_string()))?;
    let file_stem = file_path
        .file_stem()
        .ok_or_else(|| OrganizerError::FileCopy("Invalid file stem".to_string()))?
        .to_string_lossy()
        .to_string();
    let file_extension = file_path
        .extension()
        .map(|ext| ext.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut counter = 0u32;
    let mut unique_path = dir_path.join(file_name);

    while unique_path.exists() {
        counter += 1;
        unique_path = dir_path.join(candidate_name(&file_stem, &file_extension, counter));
    }

    Ok(unique_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;
    use std::fs;
    use tempfile::TempDir;

    fn set_mtime(path: &Path, unix: i64) {
        filetime::set_file_mtime(path, FileTime::from_unix_time(unix, 0)).unwrap();
    }

    /// A media pair without metadata must resolve to the file's *own* mtime.
    /// not to "roughly now", which any broken implementation would also satisfy.
    #[test]
    fn test_path_generation_fallback() {
        let temp_dir = TempDir::new().unwrap();

        let media_file = temp_dir.path().join("test_image.jpg");
        fs::write(&media_file, "fake image content").unwrap();

        // 2015-06-15T12:00:00Z
        let expected_ts = 1434369600i64;
        set_mtime(&media_file, expected_ts);

        let pair = MediaMetadataPair::WithoutMetadata(media_file.clone());
        let (path, date) = extract_photo_date(pair, &HashMap::new()).unwrap();

        assert_eq!(path, media_file);
        let date = date.known().expect("a real mtime must yield a known date");
        assert_eq!(
            date.timestamp(),
            expected_ts,
            "extracted date must come from the file's mtime"
        );
        assert_eq!(format_date_path(&date), "2015/06");
    }

    #[test]
    fn test_now_ish_mtime_is_not_a_date() {
        let temp_dir = TempDir::new().unwrap();
        let media_file = temp_dir.path().join("fresh.jpg");
        // Freshly created during this run => mtime is "now".
        fs::write(&media_file, "content").unwrap();

        let pair = MediaMetadataPair::WithoutMetadata(media_file);
        let (_, date) = extract_photo_date(pair, &HashMap::new()).unwrap();
        assert_eq!(date, PhotoDate::Unknown);
    }

    #[test]
    fn test_missing_file_is_unknown_not_now() {
        let pair = MediaMetadataPair::WithoutMetadata(PathBuf::from("/nonexistent/nope.jpg"));
        let (_, date) = extract_photo_date(pair, &HashMap::new()).unwrap();
        assert_eq!(date, PhotoDate::Unknown);
    }

    #[test]
    fn test_hash_file_matches_for_identical_content() {
        let temp_dir = TempDir::new().unwrap();
        let a = temp_dir.path().join("a.bin");
        let b = temp_dir.path().join("b.bin");
        fs::write(&a, "same").unwrap();
        fs::write(&b, "same").unwrap();
        assert_eq!(hash_file(&a).unwrap(), hash_file(&b).unwrap());

        let c = temp_dir.path().join("c.bin");
        fs::write(&c, "different").unwrap();
        assert_ne!(hash_file(&a).unwrap(), hash_file(&c).unwrap());
    }

    #[test]
    fn test_ensure_dir_is_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let ctx = OrganizeContext::new();
        let dir = temp_dir.path().join("2021").join("01");
        ctx.ensure_dir(&dir).unwrap();
        ctx.ensure_dir(&dir).unwrap();
        assert!(dir.is_dir());
        assert_eq!(ctx.dirs.lock().unwrap().len(), 1);
    }
}
