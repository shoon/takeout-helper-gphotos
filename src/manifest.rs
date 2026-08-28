// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! The `.gphotos-manifest.json` resume manifest.
//!
//! Entries are keyed by the **blake3 content hash** of the file that was
//! organized, never by its source path. The source lives in a scratch directory
//! whose name is different on every run (`gphotos-takeout-<random>`), so a path
//! key could never match on a later run and the manifest would grow by one
//! entry per photo *per run*. Hash keying makes a re-run cheap and idempotent,
//! and it is exactly the identity `--verify` needs: the organizer copies bytes
//! verbatim, so the hash of the source is also the expected hash of the output.

use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Name of the manifest file inside the output directory.
pub const MANIFEST_FILE_NAME: &str = ".gphotos-manifest.json";

/// One organized file, as recorded by a previous (or the current) run.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ManifestEntry {
    /// blake3 hash of the file's contents, hex encoded.
    pub hash: String,
    /// Where the file was organized to.
    pub output_path: PathBuf,
    /// Unix timestamp of when the file was processed.
    pub processed_at: u64,
}

/// Manifest tracking which content has already been organized.
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Manifest {
    /// Content hash -> entry.
    pub entries: HashMap<String, ManifestEntry>,
}

impl Manifest {
    /// The entry recorded for this content hash, if any.
    pub fn lookup(&self, hash: &str) -> Option<&ManifestEntry> {
        self.entries.get(hash)
    }

    /// Whether this content has been organized before.
    pub fn is_already_processed(&self, hash: &str) -> bool {
        self.entries.contains_key(hash)
    }

    /// Where an earlier run put this content, **if that file is still there**.
    ///
    /// A manifest entry alone is not enough to skip a file: the user may have
    /// deleted or moved the output since, in which case the photo has to be
    /// organized again.
    pub fn resume_destination(&self, hash: &str) -> Option<&Path> {
        let entry = self.entries.get(hash)?;
        if entry.output_path.exists() {
            Some(&entry.output_path)
        } else {
            debug!(
                "Manifest entry for {} points at {}, which no longer exists; reprocessing",
                hash,
                entry.output_path.display()
            );
            None
        }
    }

    /// Record that content with `hash` now lives at `output_path`.
    pub fn record(&mut self, hash: String, output_path: PathBuf) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.entries.insert(
            hash.clone(),
            ManifestEntry {
                hash,
                output_path,
                processed_at: now,
            },
        );
    }

    /// Number of recorded files.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Load the manifest from the output directory.
///
/// A missing, unreadable or corrupt manifest is never fatal: the run simply
/// starts from scratch. Losing resume information costs time, not data.
pub fn load_manifest(output_dir: &Path) -> Manifest {
    let manifest_path = output_dir.join(MANIFEST_FILE_NAME);
    if manifest_path.exists() {
        match fs::read_to_string(&manifest_path) {
            Ok(content) => match serde_json::from_str::<Manifest>(&content) {
                Ok(manifest) => {
                    info!(
                        "Loaded manifest with {} entries from {}",
                        manifest.len(),
                        manifest_path.display()
                    );
                    return manifest;
                }
                Err(e) => warn!("Failed to parse manifest: {}. Starting fresh.", e),
            },
            Err(e) => warn!("Failed to read manifest: {}. Starting fresh.", e),
        }
    }
    debug!("No existing manifest found, starting fresh");
    Manifest::default()
}

/// Save the manifest into the output directory, returning its path.
///
/// Written to a sibling temporary file and renamed into place, so a crash or a
/// second Ctrl+C during the save cannot leave a truncated manifest behind. A
/// corrupt manifest would silently disable resume for the whole library.
pub fn save_manifest(manifest: &Manifest, output_dir: &Path) -> io::Result<PathBuf> {
    let manifest_path = output_dir.join(MANIFEST_FILE_NAME);
    let temp_path = output_dir.join(format!("{}.tmp", MANIFEST_FILE_NAME));

    let content = serde_json::to_string_pretty(manifest).map_err(io::Error::other)?;
    fs::write(&temp_path, content)?;
    if let Err(e) = fs::rename(&temp_path, &manifest_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(e);
    }

    info!(
        "Saved manifest with {} entries to {}",
        manifest.len(),
        manifest_path.display()
    );
    Ok(manifest_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_manifest_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let mut manifest = Manifest::default();

        manifest.record(
            "abc123".to_string(),
            PathBuf::from("/output/2023/01/photo.jpg"),
        );

        save_manifest(&manifest, temp_dir.path()).unwrap();
        let loaded = load_manifest(temp_dir.path());

        assert_eq!(loaded.len(), 1);
        assert!(loaded.is_already_processed("abc123"));
        assert!(!loaded.is_already_processed("different"));
        assert_eq!(
            loaded.lookup("abc123").unwrap().output_path,
            PathBuf::from("/output/2023/01/photo.jpg")
        );
    }

    #[test]
    fn test_manifest_missing_file() {
        let temp_dir = TempDir::new().unwrap();
        let loaded = load_manifest(temp_dir.path());
        assert!(loaded.is_empty());
    }

    /// Recording the same content twice must not grow the manifest. The key is
    /// the content, not the (per-run, throwaway) source path.
    #[test]
    fn test_record_is_idempotent_per_content() {
        let mut manifest = Manifest::default();
        manifest.record("h".to_string(), PathBuf::from("/out/a.jpg"));
        manifest.record("h".to_string(), PathBuf::from("/out/a.jpg"));
        assert_eq!(manifest.len(), 1);
    }

    /// A manifest entry whose output file has been deleted must not skip the
    /// file: the photo is no longer in the library.
    #[test]
    fn test_resume_destination_requires_the_file_to_exist() {
        let temp_dir = TempDir::new().unwrap();
        let present = temp_dir.path().join("present.jpg");
        fs::write(&present, "bytes").unwrap();

        let mut manifest = Manifest::default();
        manifest.record("here".to_string(), present.clone());
        manifest.record("gone".to_string(), temp_dir.path().join("gone.jpg"));

        assert_eq!(manifest.resume_destination("here"), Some(present.as_path()));
        assert_eq!(manifest.resume_destination("gone"), None);
        assert_eq!(manifest.resume_destination("never-seen"), None);
    }

    /// A corrupt manifest must degrade to "no resume information", never abort.
    #[test]
    fn test_corrupt_manifest_starts_fresh() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join(MANIFEST_FILE_NAME), "{not json").unwrap();
        assert!(load_manifest(temp_dir.path()).is_empty());
    }

    /// The save must be atomic: no `.tmp` litter is left behind.
    #[test]
    fn test_save_leaves_no_temporary_file() {
        let temp_dir = TempDir::new().unwrap();
        let mut manifest = Manifest::default();
        manifest.record("h".to_string(), PathBuf::from("/out/a.jpg"));
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
