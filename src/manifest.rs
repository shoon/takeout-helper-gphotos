// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! The `.gphotos-manifest.json` resume manifest.
//!
//! Version 2 records one entry for every organized output path. Paths are
//! stored relative to the output directory, so moving the library or invoking
//! the program from another working directory does not invalidate them. A
//! content-hash index keeps resume checks fast without collapsing distinct
//! files that happen to contain identical bytes.

use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Name of the manifest file inside the output directory.
pub const MANIFEST_FILE_NAME: &str = ".gphotos-manifest.json";

const MANIFEST_VERSION: u32 = 2;

/// One organized file, as recorded by a previous or the current run.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Blake3 hash of the file's contents, encoded as hexadecimal.
    pub hash: String,
    /// Path relative to the output directory.
    #[serde(rename = "path")]
    pub relative_path: PathBuf,
    /// Unix timestamp of when the file was processed.
    pub processed_at: u64,
}

impl ManifestEntry {
    /// Resolve this entry against the output directory selected for this run.
    pub fn resolve(&self, output_dir: &Path) -> PathBuf {
        output_dir.join(&self.relative_path)
    }
}

/// Manifest tracking every file that has been organized.
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    entries: Vec<ManifestEntry>,
    hash_index: HashMap<String, Vec<usize>>,
    path_index: HashMap<PathBuf, usize>,
}

impl Manifest {
    fn from_entries(entries: Vec<ManifestEntry>) -> Self {
        let mut validated = Vec::with_capacity(entries.len());
        let mut paths = HashMap::with_capacity(entries.len());

        for entry in entries {
            if let Err(error) = validate_relative_path(&entry.relative_path) {
                warn!(
                    "Ignoring unsafe manifest path {}: {}",
                    entry.relative_path.display(),
                    error
                );
                continue;
            }

            if let Some(index) = paths.get(&entry.relative_path).copied() {
                warn!(
                    "Manifest contains the path {} more than once; keeping the last entry",
                    entry.relative_path.display()
                );
                validated[index] = entry;
            } else {
                paths.insert(entry.relative_path.clone(), validated.len());
                validated.push(entry);
            }
        }

        let mut manifest = Self {
            entries: validated,
            hash_index: HashMap::new(),
            path_index: HashMap::new(),
        };
        manifest.rebuild_indexes();
        manifest
    }

    fn rebuild_indexes(&mut self) {
        self.hash_index.clear();
        self.path_index.clear();

        for (index, entry) in self.entries.iter().enumerate() {
            self.hash_index
                .entry(entry.hash.clone())
                .or_default()
                .push(index);
            self.path_index.insert(entry.relative_path.clone(), index);
        }
    }

    /// Iterate over every output path and its expected content hash.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &ManifestEntry> {
        self.entries.iter()
    }

    /// The first entry recorded for this content hash, if any.
    ///
    /// Use [`Self::entries_for_hash`] when every byte-identical path matters.
    pub fn lookup(&self, hash: &str) -> Option<&ManifestEntry> {
        let index = *self.hash_index.get(hash)?.first()?;
        self.entries.get(index)
    }

    /// Iterate over all output paths that have this content hash.
    pub fn entries_for_hash<'a>(
        &'a self,
        hash: &str,
    ) -> impl Iterator<Item = &'a ManifestEntry> + 'a {
        self.hash_index
            .get(hash)
            .into_iter()
            .flatten()
            .filter_map(|index| self.entries.get(*index))
    }

    /// Whether this content has been organized before.
    pub fn is_already_processed(&self, hash: &str) -> bool {
        self.hash_index.contains_key(hash)
    }

    /// Find an existing destination from an earlier run for this content.
    ///
    /// A manifest entry alone is not enough to skip a file because the user
    /// may have deleted or moved an output. All paths are resolved against the
    /// output directory provided by the current run.
    pub fn resume_destination(&self, output_dir: &Path, hash: &str) -> Option<PathBuf> {
        let mut first_destination = None;
        for entry in self.entries_for_hash(hash) {
            let destination = entry.resolve(output_dir);
            if !destination.exists() {
                debug!(
                    "Manifest entry for {} points at {}, which no longer exists; reprocessing",
                    hash,
                    destination.display()
                );
                return None;
            }
            first_destination.get_or_insert(destination);
        }
        first_destination
    }

    /// Record content at an output path.
    ///
    /// `output_path` must be contained by `output_dir`. Only its relative path
    /// is retained. Recording the same path again updates that entry, while
    /// recording the same hash at another path creates a separate entry.
    pub fn record(
        &mut self,
        output_dir: &Path,
        hash: String,
        output_path: &Path,
    ) -> io::Result<()> {
        let relative_path = relative_to_output_dir(output_dir, output_path)?;
        let processed_at = current_unix_timestamp();

        if let Some(index) = self.path_index.get(&relative_path).copied() {
            let previous_hash = self.entries[index].hash.clone();
            self.entries[index] = ManifestEntry {
                hash: hash.clone(),
                relative_path,
                processed_at,
            };

            if previous_hash != hash {
                self.rebuild_indexes();
            }
            return Ok(());
        }

        let index = self.entries.len();
        self.entries.push(ManifestEntry {
            hash: hash.clone(),
            relative_path: relative_path.clone(),
            processed_at,
        });
        self.hash_index.entry(hash).or_default().push(index);
        self.path_index.insert(relative_path, index);
        Ok(())
    }

    /// Number of recorded output paths.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Serialize, Deserialize)]
struct ManifestFile {
    version: u32,
    entries: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
struct LegacyManifest {
    entries: HashMap<String, LegacyManifestEntry>,
}

#[derive(Deserialize)]
struct LegacyManifestEntry {
    hash: String,
    output_path: PathBuf,
    processed_at: u64,
}

/// Load the manifest from the output directory.
///
/// Version 1 manifests are migrated in memory. Their hash-keyed layout can
/// only contain one path per hash, but every path that layout retained remains
/// available for resume and is saved in version 2 form at the end of the run.
///
/// A missing, unreadable or corrupt manifest is never fatal. The run simply
/// starts from scratch. Losing resume information costs time, not data.
pub fn load_manifest(output_dir: &Path) -> Manifest {
    let manifest_path = output_dir.join(MANIFEST_FILE_NAME);
    if !manifest_path.exists() {
        debug!("No existing manifest found, starting fresh");
        return Manifest::default();
    }

    let content = match fs::read_to_string(&manifest_path) {
        Ok(content) => content,
        Err(error) => {
            warn!("Failed to read manifest: {}. Starting fresh.", error);
            return Manifest::default();
        }
    };

    let value = match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(value) => value,
        Err(error) => {
            warn!("Failed to parse manifest: {}. Starting fresh.", error);
            return Manifest::default();
        }
    };

    let manifest = match value.get("version").and_then(|version| version.as_u64()) {
        Some(version) if version == u64::from(MANIFEST_VERSION) => {
            match serde_json::from_value::<ManifestFile>(value) {
                Ok(file) => Manifest::from_entries(file.entries),
                Err(error) => {
                    warn!("Failed to parse manifest: {}. Starting fresh.", error);
                    return Manifest::default();
                }
            }
        }
        Some(version) => {
            warn!(
                "Manifest version {} is not supported. Starting fresh.",
                version
            );
            return Manifest::default();
        }
        None => match serde_json::from_value::<LegacyManifest>(value) {
            Ok(legacy) => migrate_legacy_manifest(legacy, output_dir),
            Err(error) => {
                warn!("Failed to parse manifest: {}. Starting fresh.", error);
                return Manifest::default();
            }
        },
    };

    info!(
        "Loaded manifest with {} entries from {}",
        manifest.len(),
        manifest_path.display()
    );
    manifest
}

fn migrate_legacy_manifest(legacy: LegacyManifest, output_dir: &Path) -> Manifest {
    let mut entries = Vec::with_capacity(legacy.entries.len());

    for (key_hash, entry) in legacy.entries {
        let hash = if entry.hash.is_empty() {
            key_hash
        } else {
            entry.hash
        };

        match legacy_relative_path(output_dir, &entry.output_path) {
            Ok(relative_path) => entries.push(ManifestEntry {
                hash,
                relative_path,
                processed_at: entry.processed_at,
            }),
            Err(error) => warn!(
                "Could not migrate legacy manifest path {}: {}",
                entry.output_path.display(),
                error
            ),
        }
    }

    warn!(
        "Migrated {} entries from the version 1 manifest format. Version 1 could omit differently named files with identical bytes; run once with --force --verify to rebuild a complete version 2 manifest",
        entries.len()
    );
    Manifest::from_entries(entries)
}

/// Save the manifest into the output directory, returning its path.
///
/// The new contents are written to a sibling temporary file and renamed into
/// place, so an interrupted write cannot leave a truncated manifest behind.
pub fn save_manifest(manifest: &Manifest, output_dir: &Path) -> io::Result<PathBuf> {
    let manifest_path = output_dir.join(MANIFEST_FILE_NAME);
    let temp_path = output_dir.join(format!("{}.tmp", MANIFEST_FILE_NAME));
    let file = ManifestFile {
        version: MANIFEST_VERSION,
        entries: manifest.entries.clone(),
    };

    let content = serde_json::to_string_pretty(&file).map_err(io::Error::other)?;
    fs::write(&temp_path, content)?;
    if let Err(error) = fs::rename(&temp_path, &manifest_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    info!(
        "Saved manifest with {} entries to {}",
        manifest.len(),
        manifest_path.display()
    );
    Ok(manifest_path)
}

fn current_unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn relative_to_output_dir(output_dir: &Path, output_path: &Path) -> io::Result<PathBuf> {
    let output_dir = normalize_absolute(output_dir)?;
    let output_path = normalize_absolute(output_path)?;
    let relative = output_path.strip_prefix(&output_dir).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "output path {} is outside output directory {}",
                output_path.display(),
                output_dir.display()
            ),
        )
    })?;
    validate_relative_path(relative)
}

fn normalize_absolute(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut normalized = PathBuf::new();

    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("path escapes its filesystem root: {}", path.display()),
                    ));
                }
            }
        }
    }

    Ok(normalized)
}

fn normalize_relative(path: &Path) -> io::Result<PathBuf> {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Normal(_) => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "relative path escapes its output directory: {}",
                            path.display()
                        ),
                    ));
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("expected a relative path, found {}", path.display()),
                ));
            }
        }
    }

    validate_relative_path(&normalized)
}

fn validate_relative_path(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "manifest path is not a confined relative path: {}",
                path.display()
            ),
        ));
    }
    Ok(path.to_path_buf())
}

fn legacy_relative_path(output_dir: &Path, legacy_path: &Path) -> io::Result<PathBuf> {
    if let Ok(relative) = relative_to_output_dir(output_dir, legacy_path) {
        return Ok(relative);
    }

    if !legacy_path.is_absolute() {
        let normalized_legacy = normalize_relative(legacy_path)?;
        if !output_dir.is_absolute() {
            let normalized_output = normalize_relative(output_dir)?;
            if let Ok(relative) = normalized_legacy.strip_prefix(&normalized_output) {
                return validate_relative_path(relative);
            }
        }

        if let Some(relative) = suffix_after_output_name(output_dir, &normalized_legacy) {
            return validate_relative_path(&relative);
        }
        return Ok(normalized_legacy);
    }

    if let Some(relative) = suffix_after_output_name(output_dir, legacy_path) {
        return validate_relative_path(&relative);
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "legacy path {} cannot be made relative to {}",
            legacy_path.display(),
            output_dir.display()
        ),
    ))
}

fn suffix_after_output_name(output_dir: &Path, path: &Path) -> Option<PathBuf> {
    let output_name = output_dir.file_name()?;
    let components: Vec<_> = path.components().collect();
    let position = components.iter().rposition(|component| {
        matches!(component, Component::Normal(name) if path_names_equal(name, output_name))
    })?;

    let mut relative = PathBuf::new();
    for component in components.iter().skip(position + 1) {
        match component {
            Component::Normal(_) => relative.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    (!relative.as_os_str().is_empty()).then_some(relative)
}

fn path_names_equal(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_file(path: &Path, contents: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn manifest_roundtrip_uses_relative_paths() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("library");
        let output_path = output_dir.join("2023/01/photo.jpg");
        create_file(&output_path, b"photo");

        let mut manifest = Manifest::default();
        manifest
            .record(&output_dir, "abc123".to_string(), &output_path)
            .unwrap();

        save_manifest(&manifest, &output_dir).unwrap();
        let loaded = load_manifest(&output_dir);

        assert_eq!(loaded.len(), 1);
        assert!(loaded.is_already_processed("abc123"));
        assert!(!loaded.is_already_processed("different"));
        let entry = loaded.lookup("abc123").unwrap();
        assert_eq!(entry.relative_path, PathBuf::from("2023/01/photo.jpg"));
        assert_eq!(entry.resolve(&output_dir), output_path);
    }

    #[test]
    fn missing_manifest_starts_fresh() {
        let temp_dir = TempDir::new().unwrap();
        let loaded = load_manifest(temp_dir.path());
        assert!(loaded.is_empty());
    }

    #[test]
    fn records_every_byte_identical_output_path() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("library");
        let first = output_dir.join("2023/first.jpg");
        let second = output_dir.join("2023/second.jpg");
        create_file(&first, b"same bytes");
        create_file(&second, b"same bytes");

        let mut manifest = Manifest::default();
        manifest
            .record(&output_dir, "same-hash".to_string(), &first)
            .unwrap();
        manifest
            .record(&output_dir, "same-hash".to_string(), &second)
            .unwrap();

        assert_eq!(manifest.len(), 2);
        let paths: Vec<_> = manifest
            .entries_for_hash("same-hash")
            .map(|entry| entry.relative_path.clone())
            .collect();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&PathBuf::from("2023/first.jpg")));
        assert!(paths.contains(&PathBuf::from("2023/second.jpg")));

        save_manifest(&manifest, &output_dir).unwrap();
        let loaded = load_manifest(&output_dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.entries_for_hash("same-hash").count(), 2);
    }

    #[test]
    fn recording_the_same_path_updates_in_place() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("library");
        let output_path = output_dir.join("photo.jpg");
        create_file(&output_path, b"photo");

        let mut manifest = Manifest::default();
        manifest
            .record(&output_dir, "old-hash".to_string(), &output_path)
            .unwrap();
        manifest
            .record(&output_dir, "new-hash".to_string(), &output_path)
            .unwrap();

        assert_eq!(manifest.len(), 1);
        assert!(!manifest.is_already_processed("old-hash"));
        assert!(manifest.is_already_processed("new-hash"));
    }

    #[test]
    fn resume_destination_requires_an_existing_file() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("library");
        let present = output_dir.join("present.jpg");
        let gone = output_dir.join("gone.jpg");
        create_file(&present, b"bytes");

        let mut manifest = Manifest::default();
        manifest
            .record(&output_dir, "here".to_string(), &present)
            .unwrap();
        manifest
            .record(&output_dir, "gone".to_string(), &gone)
            .unwrap();

        assert_eq!(
            manifest.resume_destination(&output_dir, "here"),
            Some(present)
        );
        assert_eq!(manifest.resume_destination(&output_dir, "gone"), None);
        assert_eq!(manifest.resume_destination(&output_dir, "never-seen"), None);
    }

    #[test]
    fn resume_reprocesses_when_one_identical_output_path_is_missing() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("library");
        let present = output_dir.join("present.jpg");
        let missing = output_dir.join("missing.jpg");
        create_file(&present, b"same bytes");

        let mut manifest = Manifest::default();
        manifest
            .record(&output_dir, "same-hash".to_string(), &present)
            .unwrap();
        manifest
            .record(&output_dir, "same-hash".to_string(), &missing)
            .unwrap();

        assert_eq!(
            manifest.resume_destination(&output_dir, "same-hash"),
            None,
            "a surviving byte-identical path must not hide a missing output"
        );
    }

    #[test]
    fn relative_entries_resolve_against_the_supplied_output_root() {
        let temp_dir = TempDir::new().unwrap();
        let original_root = temp_dir.path().join("first-location/library");
        let original_path = original_root.join("2023/photo.jpg");
        create_file(&original_path, b"photo");

        let mut manifest = Manifest::default();
        manifest
            .record(&original_root, "hash".to_string(), &original_path)
            .unwrap();
        save_manifest(&manifest, &original_root).unwrap();

        let loaded = load_manifest(&original_root);
        let different_root = temp_dir.path().join("another-location/library");
        assert_eq!(
            loaded.lookup("hash").unwrap().resolve(&different_root),
            different_root.join("2023/photo.jpg")
        );

        let saved: ManifestFile = serde_json::from_str(
            &fs::read_to_string(original_root.join(MANIFEST_FILE_NAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(saved.version, MANIFEST_VERSION);
        assert_eq!(
            saved.entries[0].relative_path,
            PathBuf::from("2023/photo.jpg")
        );
        assert!(!saved.entries[0].relative_path.is_absolute());
    }

    #[test]
    fn migrates_legacy_hash_keyed_manifest_without_losing_resume_data() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("organized");
        let output_path = output_dir.join("2020/photo.jpg");
        create_file(&output_path, b"photo");

        let legacy = serde_json::json!({
            "entries": {
                "legacy-hash": {
                    "hash": "legacy-hash",
                    "output_path": output_path,
                    "processed_at": 1234
                }
            }
        });
        fs::write(
            output_dir.join(MANIFEST_FILE_NAME),
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let loaded = load_manifest(&output_dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.resume_destination(&output_dir, "legacy-hash"),
            Some(output_path.clone())
        );
        let entry = loaded.lookup("legacy-hash").unwrap();
        assert_eq!(entry.relative_path, PathBuf::from("2020/photo.jpg"));
        assert_eq!(entry.processed_at, 1234);

        save_manifest(&loaded, &output_dir).unwrap();
        let migrated: ManifestFile =
            serde_json::from_str(&fs::read_to_string(output_dir.join(MANIFEST_FILE_NAME)).unwrap())
                .unwrap();
        assert_eq!(migrated.version, MANIFEST_VERSION);
        assert_eq!(migrated.entries.len(), 1);
        assert_eq!(
            migrated.entries[0].relative_path,
            PathBuf::from("2020/photo.jpg")
        );
    }

    #[test]
    fn migrates_legacy_relative_paths_prefixed_with_the_old_output_name() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("organized");
        let output_path = output_dir.join("2020/photo.jpg");
        create_file(&output_path, b"photo");

        let legacy = serde_json::json!({
            "entries": {
                "legacy-hash": {
                    "hash": "legacy-hash",
                    "output_path": PathBuf::from("organized/2020/photo.jpg"),
                    "processed_at": 1234
                }
            }
        });
        fs::write(
            output_dir.join(MANIFEST_FILE_NAME),
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let loaded = load_manifest(&output_dir);
        assert_eq!(
            loaded.resume_destination(&output_dir, "legacy-hash"),
            Some(output_path)
        );
    }

    #[test]
    fn rejects_output_paths_outside_the_output_directory() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("library");
        let outside = temp_dir.path().join("outside.jpg");
        let mut manifest = Manifest::default();

        let error = manifest
            .record(&output_dir, "hash".to_string(), &outside)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(manifest.is_empty());
    }

    #[test]
    fn ignores_unsafe_paths_in_a_version_2_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("library");
        fs::create_dir_all(&output_dir).unwrap();
        let absolute_path = temp_dir.path().join("outside.jpg");
        let file = serde_json::json!({
            "version": MANIFEST_VERSION,
            "entries": [
                {
                    "hash": "traversal",
                    "path": "../outside.jpg",
                    "processed_at": 1
                },
                {
                    "hash": "absolute",
                    "path": absolute_path,
                    "processed_at": 2
                }
            ]
        });
        fs::write(
            output_dir.join(MANIFEST_FILE_NAME),
            serde_json::to_string_pretty(&file).unwrap(),
        )
        .unwrap();

        assert!(load_manifest(&output_dir).is_empty());
    }

    #[test]
    fn duplicate_paths_in_a_version_2_manifest_keep_the_last_entry() {
        let temp_dir = TempDir::new().unwrap();
        let output_dir = temp_dir.path().join("library");
        fs::create_dir_all(&output_dir).unwrap();
        let file = serde_json::json!({
            "version": MANIFEST_VERSION,
            "entries": [
                {
                    "hash": "old-hash",
                    "path": "2023/photo.jpg",
                    "processed_at": 1
                },
                {
                    "hash": "new-hash",
                    "path": "2023/photo.jpg",
                    "processed_at": 2
                }
            ]
        });
        fs::write(
            output_dir.join(MANIFEST_FILE_NAME),
            serde_json::to_string_pretty(&file).unwrap(),
        )
        .unwrap();

        let loaded = load_manifest(&output_dir);
        assert_eq!(loaded.len(), 1);
        assert!(!loaded.is_already_processed("old-hash"));
        assert_eq!(loaded.lookup("new-hash").unwrap().processed_at, 2);
    }

    #[test]
    fn corrupt_manifest_starts_fresh() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(MANIFEST_FILE_NAME), "{not json").unwrap();
        assert!(load_manifest(temp_dir.path()).is_empty());
    }

    #[test]
    fn save_leaves_no_temporary_file() {
        let temp_dir = TempDir::new().unwrap();
        let output_path = temp_dir.path().join("photo.jpg");
        create_file(&output_path, b"photo");
        let mut manifest = Manifest::default();
        manifest
            .record(temp_dir.path(), "h".to_string(), &output_path)
            .unwrap();
        let path = save_manifest(&manifest, temp_dir.path()).unwrap();

        assert_eq!(path.file_name().unwrap(), MANIFEST_FILE_NAME);
        assert!(
            !temp_dir
                .path()
                .join(format!("{}.tmp", MANIFEST_FILE_NAME))
                .exists()
        );
    }
}
