// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

// Import the stats module to access ProcessingStats
use crate::stats;

/// Every media extension Google Photos can put in a Takeout export.
///
/// Compared case-insensitively so less-common Takeout formats are not silently
/// dropped.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    // stills
    "jpg", "jpeg", "png", "heic", "heif", "avif", "gif", "webp", "bmp", "tif", "tiff",
    // raw
    "dng", "cr2", "cr3", "nef", "arw", "orf", "rw2", "raf", // video
    "mp4", "mov", "m4v", "3gp", "3g2", "avi", "mkv", "wmv", "mpg", "mpeg", "mts", "m2ts", "mp",
];

/// Extensions treated as the "photo half" of a Live Photo / motion photo pair.
const LIVE_PHOTO_STILL_EXTENSIONS: &[&str] = &["heic", "heif", "jpg", "jpeg", "png"];

/// Extensions treated as the "video half" of a Live Photo / motion photo pair.
const LIVE_PHOTO_VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "3gp"];

/// Localized variants of the `-edited` suffix Google appends to edited copies.
///
/// Edited copies never get a sidecar of their own; they inherit the base
/// photo's JSON.
const EDITED_SUFFIXES: &[&str] = &[
    "-edited",
    "-bearbeitet",
    "-bewerkt",
    "-modifié",
    "-modifie",
    "-editado",
    "-edité",
    "-edite",
    "-redigeret",
    "-redigerad",
    "-muokattu",
    "-upravené",
    "-upraveno",
    "-bewurk",
];

/// The full supplemental suffix; Google truncates it to any non-empty prefix.
const SUPPLEMENTAL: &str = "supplemental-metadata";

/// Takeout housekeeping JSON files that are *not* media sidecars and must never
/// be reported as orphans.
const HOUSEKEEPING_JSON: &[&str] = &[
    "metadata.json",
    "print-subscriptions.json",
    "shared_album_comments.json",
    "user-generated-memory-titles.json",
];

/// Struct representing the Google Photos JSON metadata format
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PhotoMetadata {
    #[serde(rename = "title")]
    pub title: Option<String>,

    #[serde(rename = "description")]
    pub description: Option<String>,

    #[serde(rename = "photoTakenTime")]
    pub photo_taken_time: Option<Timestamp>,

    #[serde(rename = "geoData")]
    pub geo_data: Option<GeoData>,

    #[serde(rename = "geoDataExif")]
    pub geo_data_exif: Option<GeoDataExif>,

    // Add other fields as needed
    #[serde(rename = "imageViews")]
    pub image_views: Option<String>,

    #[serde(rename = "creationTime")]
    pub creation_time: Option<Timestamp>,

    #[serde(rename = "modificationTime")]
    pub modification_time: Option<Timestamp>,

    #[serde(rename = "favorited")]
    pub favorited: Option<bool>,

    #[serde(rename = "archive")]
    pub archive: Option<String>,

    #[serde(rename = "migrated")]
    pub migrated: Option<String>,

    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,

    #[serde(rename = "isGooglePhotosMedia")]
    pub is_google_photos_media: Option<bool>,

    #[serde(rename = "isShared")]
    pub is_shared: Option<bool>,
}

/// Struct representing timestamp data in Google Photos JSON
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Timestamp {
    #[serde(rename = "timestamp")]
    pub timestamp: Option<String>,

    #[serde(rename = "formatted")]
    pub formatted: Option<String>,
}

/// Struct representing geo data in Google Photos JSON
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GeoData {
    #[serde(rename = "latitude")]
    pub latitude: Option<f64>,

    #[serde(rename = "longitude")]
    pub longitude: Option<f64>,

    #[serde(rename = "altitude")]
    pub altitude: Option<f64>,

    #[serde(rename = "latitudeSpan")]
    pub latitude_span: Option<f64>,

    #[serde(rename = "longitudeSpan")]
    pub longitude_span: Option<f64>,
}

/// Struct representing EXIF geo data in Google Photos JSON
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GeoDataExif {
    #[serde(rename = "latitude")]
    pub latitude: Option<f64>,

    #[serde(rename = "longitude")]
    pub longitude: Option<f64>,

    #[serde(rename = "altitude")]
    pub altitude: Option<f64>,

    #[serde(rename = "latitudeSpan")]
    pub latitude_span: Option<f64>,

    #[serde(rename = "longitudeSpan")]
    pub longitude_span: Option<f64>,
}

/// Longest title we keep; anything longer is truncated rather than discarded.
const MAX_TITLE_LEN: usize = 1000;
/// Longest description we keep; anything longer is truncated.
const MAX_DESCRIPTION_LEN: usize = 10000;
/// Longest mime type we keep; anything longer is dropped.
const MAX_MIME_TYPE_LEN: usize = 100;
/// Sidecars are small JSON documents. Refuse unexpectedly huge files before
/// allocating a string for them; a malformed archive must not turn metadata
/// parsing into an unbounded memory allocation.
const MAX_SIDECAR_BYTES: u64 = 16 * 1024 * 1024;

/// Truncate a string to at most `max` bytes without splitting a UTF-8 char.
fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

impl PhotoMetadata {
    /// Validate the metadata fields.
    ///
    /// Kept for API compatibility. A sidecar is only *unusable* if it carries
    /// no recoverable information at all. Bad individual fields are handled by
    /// [`PhotoMetadata::sanitize`], which clamps or drops them instead of
    /// throwing away the whole file.
    pub fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    /// Clamp or drop unusable individual fields, logging what was changed.
    ///
    /// This never fails: a sidecar with one bad field is still worth far more
    /// than no sidecar at all (the alternative is filing the photo under the
    /// current month).
    pub fn sanitize(&mut self, source: &Path) {
        if let Some(title) = &self.title
            && title.len() > MAX_TITLE_LEN
        {
            warn!("Title too long in {}, truncating", source.display());
            self.title = Some(truncate_utf8(title, MAX_TITLE_LEN));
        }

        if let Some(description) = &self.description
            && description.len() > MAX_DESCRIPTION_LEN
        {
            warn!("Description too long in {}, truncating", source.display());
            self.description = Some(truncate_utf8(description, MAX_DESCRIPTION_LEN));
        }

        for (label, ts) in [
            ("photoTakenTime", &self.photo_taken_time),
            ("creationTime", &self.creation_time),
            ("modificationTime", &self.modification_time),
        ] {
            if let Some(ts) = ts
                && let Some(value) = &ts.timestamp
                && value.parse::<i64>().is_err()
            {
                warn!(
                    "Invalid {} timestamp {:?} in {}; will fall back to other date sources",
                    label,
                    value,
                    source.display()
                );
            }
        }

        // Out-of-range coordinates are dropped field-by-field; the rest of the
        // sidecar (crucially the timestamps) is kept.
        if let Some(geo) = &mut self.geo_data {
            if geo
                .latitude
                .is_some_and(|lat| !(-90.0..=90.0).contains(&lat))
            {
                warn!("Invalid geoData latitude in {}, dropping", source.display());
                geo.latitude = None;
            }
            if geo
                .longitude
                .is_some_and(|lon| !(-180.0..=180.0).contains(&lon))
            {
                warn!(
                    "Invalid geoData longitude in {}, dropping",
                    source.display()
                );
                geo.longitude = None;
            }
        }

        if let Some(geo) = &mut self.geo_data_exif {
            if geo
                .latitude
                .is_some_and(|lat| !(-90.0..=90.0).contains(&lat))
            {
                warn!(
                    "Invalid geoDataExif latitude in {}, dropping",
                    source.display()
                );
                geo.latitude = None;
            }
            if geo
                .longitude
                .is_some_and(|lon| !(-180.0..=180.0).contains(&lon))
            {
                warn!(
                    "Invalid geoDataExif longitude in {}, dropping",
                    source.display()
                );
                geo.longitude = None;
            }
        }

        if let Some(mime_type) = &self.mime_type
            && mime_type.len() > MAX_MIME_TYPE_LEN
        {
            warn!("Mime type too long in {}, dropping", source.display());
            self.mime_type = None;
        }
    }
}

/// Enum to represent media files with or without metadata.
///
/// The third field of [`MediaMetadataPair::WithMetadata`] is the path of the
/// JSON sidecar the metadata was loaded from. It is needed by `--copy-sidecars`,
/// which places a copy of the sidecar next to the organized file, and is
/// stored on the pair because the pairing rules know which JSON file matched.
/// Truncation, `(N)` counters, and inheritance by `-edited` copies cannot be
/// reconstructed from the media name alone.
#[derive(Clone)]
pub enum MediaMetadataPair {
    WithMetadata(PathBuf, Box<PhotoMetadata>, Option<PathBuf>),
    WithoutMetadata(PathBuf),
}

impl MediaMetadataPair {
    /// The media file this pair describes.
    pub fn path(&self) -> &Path {
        match self {
            MediaMetadataPair::WithMetadata(path, ..) => path,
            MediaMetadataPair::WithoutMetadata(path) => path,
        }
    }

    /// The JSON sidecar the metadata came from, when one was matched.
    pub fn json_path(&self) -> Option<&Path> {
        match self {
            MediaMetadataPair::WithMetadata(_, _, json) => json.as_deref(),
            MediaMetadataPair::WithoutMetadata(_) => None,
        }
    }
}

/// Folder names Google Takeout uses that are **not** user albums.
///
/// `photos from <year>` is the default chronological bucket. The rest are
/// Google's own special containers: putting them in an album tree would be
/// actively misleading (bef55e3 only knew about "photos from"/"google
/// photos"/"takeout" and would have created an album literally called "Trash").
const NON_ALBUM_FOLDERS: &[&str] = &[
    "google photos",
    "takeout",
    "archive",
    "trash",
    "bin",
    "locked folder",
    "failed videos",
    "untitled",
];

/// Non-`-edited` suffixes Google appends to derivative files it generated
/// itself. The `-edited` family is handled through [`EDITED_SUFFIXES`] so the
/// localized variants such as `-bearbeitet` and `-modifié` are covered too.
const DERIVATIVE_SUFFIXES: &[&str] = &[
    "-effects",
    "-collage",
    "-animation",
    "-pano",
    "-movie",
    "-mix",
    "-smile",
];

/// True when `file_name` looks like a Google-generated derivative rather than
/// an original the user uploaded.
///
/// Used only by `--skip-derivatives`. Note the polarity: bef55e3 skipped these
/// by *default*, which silently dropped user files whenever the heuristic
/// misfired (a photo genuinely named `sunset-pano.jpg`, for instance). Keeping
/// everything is the safe default; skipping is opt-in.
pub fn is_derivative(file_name: &str) -> bool {
    let (stem, _) = split_name(file_name);
    // Google puts the duplicate counter after the stem (`foo-edited(1).jpg`).
    let (stem, _) = strip_counter(stem);
    if strip_edited_suffix(stem).is_some() {
        return true;
    }
    let lower = stem.to_lowercase();
    DERIVATIVE_SUFFIXES
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

/// True when `folder` is one of Google's own containers rather than a user album.
fn is_non_album_folder(folder: &str) -> bool {
    let lower = folder.trim().to_lowercase();
    if lower.is_empty() || lower == "." || lower == ".." {
        return true;
    }
    // "Photos from 2023", and its localized cousins all share the year shape.
    if lower.starts_with("photos from ") {
        return true;
    }
    NON_ALBUM_FOLDERS.contains(&lower.as_str())
}

/// The user album a takeout file belongs to, if any.
///
/// Google Takeout lays media out as `Takeout/Google Photos/<folder>/file.jpg`,
/// where `<folder>` is either a user album or one of Google's own buckets
/// (`Photos from 2023`, `Trash`, `Archive`, and others). Only the former is an
/// album.
///
/// Returns `None` when the file is not under `extract_root`, sits directly in
/// one of Google's buckets, or has no parent folder to speak of.
pub fn extract_album_name(path: &Path, extract_root: &Path) -> Option<String> {
    let relative = path.strip_prefix(extract_root).ok()?;
    // Need at least `<folder>/file.ext`.
    let parent = relative.parent()?;
    let folder = parent.file_name()?.to_string_lossy().to_string();

    if is_non_album_folder(&folder) {
        return None;
    }

    Some(folder)
}

/// True when `ext` (any case) is a media extension we process.
pub fn is_media_extension(ext: &str) -> bool {
    MEDIA_EXTENSIONS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(ext))
}

/// True when `name` (any case) ends in `.json`.
fn is_json_name(name: &str) -> bool {
    name.len() >= 5 && name[name.len() - 5..].eq_ignore_ascii_case(".json")
}

/// True when `name` is a Takeout housekeeping JSON rather than a media sidecar.
fn is_housekeeping_json(name: &str) -> bool {
    HOUSEKEEPING_JSON
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// Split a file name into `(stem, ext)` at the last `.`; `ext` is `""` if absent.
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

/// Strip a trailing `(N)` duplicate counter, returning it when present.
///
/// Google puts the counter *after* the extension in sidecar names
/// (`foo.jpg(1).json`) but *before* it in media names (`foo(1).jpg`), so both
/// sides are normalised through here.
fn strip_counter(s: &str) -> (&str, Option<u32>) {
    if let Some(head) = s.strip_suffix(')')
        && let Some(open) = head.rfind('(')
    {
        let digits = &head[open + 1..];
        if !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = digits.parse::<u32>()
        {
            return (&head[..open], Some(n));
        }
    }
    (s, None)
}

/// True when `segment` is a non-empty prefix of `supplemental-metadata`.
///
/// Google truncates the whole sidecar file name at 46 characters, so the
/// suffix shows up as `.supplemental-metadata`, `.supplemental-metad`,
/// `.suppl`, `.s`, another truncated form, or not at all (legacy exports).
fn is_supplemental_prefix(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= SUPPLEMENTAL.len()
        && SUPPLEMENTAL[..segment.len()].eq_ignore_ascii_case(segment)
}

/// Derive the media file name a JSON sidecar refers to.
///
/// Returns `(media_name, duplicate_counter)` where `media_name` is
/// `basename.ext` as Google wrote it, possibly truncated. Returns `None` for
/// names that are not `.json` at all.
pub fn derive_sidecar_target(json_name: &str) -> Option<(String, Option<u32>)> {
    if !is_json_name(json_name) {
        return None;
    }
    let base = &json_name[..json_name.len() - 5];

    // 1. duplicate counter, remembered so it can be re-inserted before the ext
    let (base, counter) = strip_counter(base);

    // 2. optional (possibly truncated) `.supplemental-metadata` suffix
    let base = match base.rfind('.') {
        Some(i) if i > 0 && is_supplemental_prefix(&base[i + 1..]) => &base[..i],
        _ => base,
    };

    Some((base.to_string(), counter))
}

/// Strip a localized `-edited` suffix from a stem, if present.
fn strip_edited_suffix(stem: &str) -> Option<&str> {
    for suffix in EDITED_SUFFIXES {
        // Split on a char boundary of the ORIGINAL stem: lowercasing can change
        // byte length (for example, U+212A becomes 'k'), so an index derived
        // from the lowercased copy could slice mid-character.
        if stem.len() > suffix.len() {
            let split = stem.len() - suffix.len();
            if stem.is_char_boundary(split) && stem[split..].to_lowercase() == *suffix {
                return Some(&stem[..split]);
            }
        }
    }
    None
}

/// A normalised, case-folded, counter-stripped file name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NameKey {
    /// `stem.ext` lowercased, counter removed.
    name: String,
    /// Stem lowercased, counter removed.
    stem: String,
    /// Extension lowercased (`""` when absent).
    ext: String,
    /// The `(N)` duplicate counter, if any.
    counter: Option<u32>,
}

impl NameKey {
    fn new(name: &str, counter: Option<u32>) -> Self {
        let (stem, ext) = split_name(name);
        // A media name carries its counter inside the stem (`foo(1).jpg`).
        let (stem, stem_counter) = strip_counter(stem);
        let stem = stem.to_lowercase();
        let ext = ext.to_lowercase();
        let full = if ext.is_empty() {
            stem.clone()
        } else {
            format!("{}.{}", stem, ext)
        };
        NameKey {
            name: full,
            stem,
            ext,
            counter: counter.or(stem_counter),
        }
    }

    fn from_media(path: &Path) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        NameKey::new(&name, None)
    }

    /// Same key but with the stem replaced (used for the `-edited` fallback).
    fn with_stem(&self, stem: &str) -> Self {
        let stem = stem.to_lowercase();
        let name = if self.ext.is_empty() {
            stem.clone()
        } else {
            format!("{}.{}", stem, self.ext)
        };
        NameKey {
            name,
            stem,
            ext: self.ext.clone(),
            counter: self.counter,
        }
    }
}

/// One JSON sidecar in a directory, pre-parsed into its match key.
struct SidecarEntry {
    path: PathBuf,
    key: NameKey,
    used: bool,
}

/// Per-directory index of every `.json` sidecar, built with a single
/// `read_dir`, avoiding a separate filesystem lookup for every candidate.
#[derive(Default)]
struct SidecarIndex {
    entries: Vec<SidecarEntry>,
    by_name: HashMap<(String, Option<u32>), usize>,
    by_stem: HashMap<(String, Option<u32>), Vec<usize>>,
}

/// Minimum number of leading characters two names must share before we accept
/// a truncation-style (prefix) match.
const MIN_PREFIX_MATCH: usize = 4;

impl SidecarIndex {
    /// Walk `dir` once, partitioning entries into media and `.json` sidecars.
    fn build(dir: &Path) -> Self {
        let mut index = SidecarIndex::default();

        let read_dir = match fs::read_dir(dir) {
            Ok(read_dir) => read_dir,
            Err(e) => {
                error!("Failed to read directory {}: {}", dir.display(), e);
                return index;
            }
        };

        for entry in read_dir.flatten() {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();

            if !is_json_name(&name) {
                continue;
            }
            // Album-level bookkeeping is not a sidecar or an orphan.
            if is_housekeeping_json(&name) {
                debug!("Ignoring Takeout housekeeping JSON: {}", name);
                continue;
            }

            let Some((target, counter)) = derive_sidecar_target(&name) else {
                continue;
            };
            let key = NameKey::new(&target, counter);
            let idx = index.entries.len();

            index
                .by_name
                .entry((key.name.clone(), key.counter))
                .or_insert(idx);
            index
                .by_stem
                .entry((key.stem.clone(), key.counter))
                .or_default()
                .push(idx);
            index.entries.push(SidecarEntry {
                path: entry.path(),
                key,
                used: false,
            });
        }

        index
    }

    /// Exact (case-insensitive) name match.
    fn exact(&self, key: &NameKey) -> Option<usize> {
        self.by_name.get(&(key.name.clone(), key.counter)).copied()
    }

    /// Truncation-tolerant match: either name is a prefix of the other, or the
    /// extensions agree and one stem is a prefix of the other. The longest
    /// candidate wins so a truncated name never beats a more specific one.
    fn prefix(&self, key: &NameKey) -> Option<usize> {
        let mut best: Option<(usize, usize)> = None;

        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.key.counter != key.counter {
                continue;
            }
            let ek = &entry.key;

            let score = if ek.ext == key.ext
                && (key.stem.starts_with(&ek.stem) || ek.stem.starts_with(&key.stem))
            {
                key.stem.len().min(ek.stem.len())
            } else if key.name.starts_with(&ek.name) || ek.name.starts_with(&key.name) {
                key.name.len().min(ek.name.len())
            } else {
                continue;
            };

            if score < MIN_PREFIX_MATCH {
                continue;
            }
            if best.is_none_or(|(best_score, _)| score > best_score) {
                best = Some((score, idx));
            }
        }

        best.map(|(_, idx)| idx)
    }

    /// Any sidecar whose stem matches, optionally restricted to a set of
    /// extensions. Used for the Live Photo and `-edited` fallbacks.
    fn by_stem_lookup(
        &self,
        stem: &str,
        counter: Option<u32>,
        exts: Option<&[&str]>,
    ) -> Option<usize> {
        let candidates = self.by_stem.get(&(stem.to_lowercase(), counter))?;
        candidates.iter().copied().find(|&idx| match exts {
            Some(exts) => exts.iter().any(|e| *e == self.entries[idx].key.ext),
            None => true,
        })
    }
}

/// How a media file acquired its metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchKind {
    /// The sidecar names this file (possibly truncated / counter-shuffled).
    Own,
    /// Inherited from another file's sidecar (`-edited` copy or Live Photo video).
    Inherited,
}

/// Find all media files in the directory recursively
pub fn find_media_files(extract_dir: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut skipped = 0usize;
    find_media_files_counted(extract_dir, &mut skipped)
}

/// Like [`find_media_files`], but also records how many non-media, non-JSON
/// files were skipped because of their extension.
pub fn find_media_files_with_stats(
    extract_dir: &Path,
    stats: &mut stats::ProcessingStats,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut skipped = 0usize;
    let files = find_media_files_counted(extract_dir, &mut skipped)?;
    stats.files_skipped_extension += skipped;
    Ok(files)
}

fn find_media_files_counted(
    extract_dir: &Path,
    skipped: &mut usize,
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    info!("Searching for media files in: {}", extract_dir.display());

    let mut media_files = Vec::new();

    // Create a progress bar for file discovery
    let discovery_pb = crate::progress::add(ProgressBar::new_spinner());
    discovery_pb.set_style(
        ProgressStyle::default_spinner()
            .tick_strings(&["▹▹▹▹▹", "▸▹▹▹▹", "▹▸▹▹▹", "▹▹▸▹▹", "▹▹▹▸▹", "▹▹▹▹▸", ""])
            .template("  {spinner:.green} Media found: {pos}")?,
    );
    discovery_pb.enable_steady_tick(std::time::Duration::from_millis(100));

    // Walk through the directory recursively. `file_type()` comes from the
    // directory entry itself, so this costs no extra `stat` per file.
    for entry in WalkDir::new(extract_dir) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let (_, ext) = split_name(&name);

        if is_media_extension(ext) {
            media_files.push(path.to_path_buf());
            discovery_pb.inc(1);
        } else if !is_json_name(&name) {
            *skipped += 1;
            debug!("Skipping non-media file: {}", path.display());
        }
    }

    discovery_pb.finish_and_clear();

    if *skipped > 0 {
        info!("Skipped {} files with non-media extensions", skipped);
    }

    Ok(media_files)
}

/// Pair media files with their corresponding JSON metadata files.
///
/// Strategy: index every `.json` in a directory once, then
/// resolve each media file against that index using Google's actual naming
/// rules: optional `.supplemental-metadata` suffix (or any truncated prefix of
/// it, or none at all for legacy exports), a `(N)` duplicate counter that moves
/// after the extension, 46-character truncation, and case-insensitive
/// extensions. `-edited` copies and Live Photo videos, which get no sidecar of
/// their own, inherit the base file's metadata.
pub fn pair_media_with_metadata(
    media_files: Vec<PathBuf>,
    stats: &mut stats::ProcessingStats,
) -> Result<Vec<MediaMetadataPair>, Box<dyn std::error::Error>> {
    info!("Pairing {} media files with metadata", media_files.len());

    let mut pairs = Vec::new();
    let mut indexes: HashMap<PathBuf, SidecarIndex> = HashMap::new();
    let mut cache: HashMap<PathBuf, Option<PhotoMetadata>> = HashMap::new();

    // Create a progress bar for pairing
    let pairing_pb = crate::progress::add(ProgressBar::new(media_files.len() as u64));
    pairing_pb.set_style(
        ProgressStyle::default_bar()
            .template("  {spinner:.green} {percent:>3}% Pair {pos}/{len}")?
            .progress_chars("#>-"),
    );

    for media_file in media_files {
        let dir = media_file
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let index = indexes
            .entry(dir.clone())
            .or_insert_with(|| SidecarIndex::build(&dir));

        let key = NameKey::from_media(&media_file);
        let matched = resolve_sidecar(index, &key);

        let mut paired = None;
        if let Some((idx, kind)) = matched {
            index.entries[idx].used = true;
            let path = index.entries[idx].path.clone();
            if let Some(metadata) = load_cached(&mut cache, &path) {
                debug!(
                    "Paired {} with {} ({:?})",
                    media_file.display(),
                    path.display(),
                    kind
                );
                if kind == MatchKind::Own {
                    // Inherited metadata is not a newly discovered sidecar.
                    stats.metadata_json_files_found += 1;
                }
                paired = Some(MediaMetadataPair::WithMetadata(
                    media_file.clone(),
                    Box::new(metadata),
                    Some(path.clone()),
                ));
            }
        }

        match paired {
            Some(pair) => pairs.push(pair),
            None => {
                debug!("No metadata file found for: {}", media_file.display());
                stats.files_without_metadata += 1;
                pairs.push(MediaMetadataPair::WithoutMetadata(media_file.clone()));
            }
        }

        pairing_pb.inc(1);
    }

    pairing_pb.finish_and_clear();

    // Any sidecar nobody claimed is an orphan worth reporting.
    let orphans: Vec<&PathBuf> = indexes
        .values()
        .flat_map(|index| index.entries.iter())
        .filter(|entry| !entry.used)
        .map(|entry| &entry.path)
        .collect();
    if !orphans.is_empty() {
        warn!("{} JSON sidecars matched no media file", orphans.len());
        for path in &orphans {
            warn!("Orphan JSON sidecar: {}", path.display());
        }
    }
    stats.orphan_sidecars += orphans.len();

    Ok(pairs)
}

/// Resolve one media file against a directory's sidecar index.
fn resolve_sidecar(index: &SidecarIndex, key: &NameKey) -> Option<(usize, MatchKind)> {
    // 1. the file's own sidecar, by exact name
    if let Some(idx) = index.exact(key) {
        return Some((idx, MatchKind::Own));
    }

    // 2. `-edited` (and localized) copies inherit the base photo's sidecar.
    //    Checked before the prefix rule, since `IMG_1-edited.jpg` would
    //    otherwise look like a truncation match for `IMG_1.jpg`'s sidecar.
    if let Some(base_stem) = strip_edited_suffix(&key.stem) {
        let base_key = key.with_stem(base_stem);
        if let Some(idx) = index
            .exact(&base_key)
            .or_else(|| index.prefix(&base_key))
            .or_else(|| index.by_stem_lookup(&base_key.stem, base_key.counter, None))
        {
            return Some((idx, MatchKind::Inherited));
        }
    }

    // 3. the file's own sidecar, tolerating Google's 46-character truncation
    if let Some(idx) = index.prefix(key) {
        return Some((idx, MatchKind::Own));
    }

    // 4. Live Photo / motion photo videos inherit the still's sidecar
    if LIVE_PHOTO_VIDEO_EXTENSIONS.contains(&key.ext.as_str())
        && let Some(idx) =
            index.by_stem_lookup(&key.stem, key.counter, Some(LIVE_PHOTO_STILL_EXTENSIONS))
    {
        return Some((idx, MatchKind::Inherited));
    }

    None
}

/// Load a sidecar through a per-run cache so shared sidecars are read once.
fn load_cached(
    cache: &mut HashMap<PathBuf, Option<PhotoMetadata>>,
    path: &Path,
) -> Option<PhotoMetadata> {
    if let Some(cached) = cache.get(path) {
        return cached.clone();
    }
    let loaded = match load_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(e) => {
            error!("Failed to parse metadata {}: {}", path.display(), e);
            None
        }
    };
    cache.insert(path.to_path_buf(), loaded.clone());
    loaded
}

/// Load and parse JSON metadata file.
///
/// Individual bad fields are clamped or dropped rather than causing the whole
/// sidecar to be discarded; only unreadable or unparseable JSON fails.
pub fn load_metadata(json_path: &Path) -> Result<PhotoMetadata, Box<dyn std::error::Error>> {
    debug!("Loading metadata from: {}", json_path.display());

    let file_size = fs::metadata(json_path)?.len();
    if file_size > MAX_SIDECAR_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "metadata sidecar is too large: {} bytes (limit {})",
                file_size, MAX_SIDECAR_BYTES
            ),
        )
        .into());
    }

    let content = fs::read_to_string(json_path)?;
    let mut metadata: PhotoMetadata = serde_json::from_str(&content)?;

    // Degrade gracefully instead of throwing the sidecar away.
    metadata.sanitize(json_path);

    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_sidecar_is_rejected_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversized.json");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_SIDECAR_BYTES + 1).unwrap();

        let error = load_metadata(&path).unwrap_err().to_string();
        assert!(error.contains("metadata sidecar is too large"));
    }

    fn derive(name: &str) -> (String, Option<u32>) {
        derive_sidecar_target(name).unwrap()
    }

    #[test]
    fn derives_supplemental_sidecars() {
        assert_eq!(
            derive("IMG_0001.jpg.supplemental-metadata.json"),
            ("IMG_0001.jpg".to_string(), None)
        );
    }

    #[test]
    fn derives_legacy_sidecars() {
        assert_eq!(
            derive("IMG_0002.jpg.json"),
            ("IMG_0002.jpg".to_string(), None)
        );
    }

    #[test]
    fn derives_truncated_suffixes() {
        for suffix in ["supplemental-metadat", "supplemental-me", "suppl", "s"] {
            assert_eq!(
                derive(&format!("IMG.jpg.{}.json", suffix)),
                ("IMG.jpg".to_string(), None),
                "suffix {}",
                suffix
            );
        }
    }

    #[test]
    fn derives_duplicate_counter_after_extension() {
        assert_eq!(
            derive("IMG_0003.jpg.supplemental-metadata(1).json"),
            ("IMG_0003.jpg".to_string(), Some(1))
        );
        assert_eq!(
            derive("IMG_0003.jpg(2).json"),
            ("IMG_0003.jpg".to_string(), Some(2))
        );
    }

    #[test]
    fn media_counter_normalises_to_same_key() {
        let media = NameKey::from_media(Path::new("/x/IMG_0003(1).jpg"));
        let (target, counter) = derive("IMG_0003.jpg.supplemental-metadata(1).json");
        let sidecar = NameKey::new(&target, counter);
        assert_eq!(media, sidecar);
    }

    #[test]
    fn rejects_non_json() {
        assert!(derive_sidecar_target("IMG_0001.jpg").is_none());
    }

    #[test]
    fn strips_localized_edited_suffixes() {
        assert_eq!(strip_edited_suffix("IMG_0004-edited"), Some("IMG_0004"));
        assert_eq!(strip_edited_suffix("IMG_0004-bearbeitet"), Some("IMG_0004"));
        assert_eq!(strip_edited_suffix("IMG_0004"), None);
    }

    #[test]
    fn media_allowlist_is_case_insensitive() {
        assert!(is_media_extension("JPG"));
        assert!(is_media_extension("m2ts"));
        assert!(is_media_extension("dng"));
        assert!(!is_media_extension("txt"));
        assert!(!is_media_extension("json"));
    }

    #[test]
    fn housekeeping_json_is_recognised() {
        assert!(is_housekeeping_json("metadata.json"));
        assert!(is_housekeeping_json("print-subscriptions.json"));
        assert!(!is_housekeeping_json("IMG_0001.jpg.json"));
    }

    #[test]
    fn sanitize_keeps_usable_parts() {
        let mut metadata = PhotoMetadata {
            title: Some("t".repeat(2000)),
            description: None,
            photo_taken_time: Some(Timestamp {
                timestamp: Some("1609459200".to_string()),
                formatted: None,
            }),
            geo_data: Some(GeoData {
                latitude: Some(999.0),
                longitude: Some(-74.0),
                altitude: None,
                latitude_span: None,
                longitude_span: None,
            }),
            geo_data_exif: None,
            image_views: None,
            creation_time: None,
            modification_time: None,
            favorited: None,
            archive: None,
            migrated: None,
            mime_type: None,
            is_google_photos_media: None,
            is_shared: None,
        };
        metadata.sanitize(Path::new("test.json"));
        assert_eq!(metadata.title.as_ref().unwrap().len(), MAX_TITLE_LEN);
        assert!(metadata.geo_data.as_ref().unwrap().latitude.is_none());
        assert_eq!(metadata.geo_data.as_ref().unwrap().longitude, Some(-74.0));
        assert!(metadata.photo_taken_time.is_some());
    }
}
