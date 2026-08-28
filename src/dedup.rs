// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! Content hashing helpers shared by the organizer, the manifest and `--verify`.
//!
//! There is exactly one hashing implementation in this crate
//! ([`crate::organizer::hash_file`]); everything here is a thin wrapper around
//! it so a file can never be hashed two different ways and compared.

use log::debug;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Result of a dedup check
pub enum DedupResult {
    /// File is unique (first time seen)
    Unique,
    /// File is a duplicate of a previously seen file
    Duplicate(PathBuf),
}

/// In-memory index of content hashes, for callers that want to detect
/// duplicates among a set of *source* files.
///
/// The pipeline itself does not use this: [`crate::organizer`] compares against
/// what is actually in the output directory, which also catches copies left by
/// earlier runs.
pub struct DedupIndex {
    hashes: HashMap<String, PathBuf>,
}

impl Default for DedupIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl DedupIndex {
    pub fn new() -> Self {
        DedupIndex {
            hashes: HashMap::new(),
        }
    }

    /// Check if a file is a duplicate. If unique, registers it in the index.
    pub fn check_duplicate(
        &mut self,
        path: &Path,
    ) -> Result<DedupResult, Box<dyn std::error::Error>> {
        let hash = compute_file_hash(path)?;
        if let Some(original) = self.hashes.get(&hash) {
            debug!(
                "Duplicate detected: {} is a duplicate of {}",
                path.display(),
                original.display()
            );
            Ok(DedupResult::Duplicate(original.clone()))
        } else {
            self.hashes.insert(hash, path.to_path_buf());
            Ok(DedupResult::Unique)
        }
    }

    /// Get the hash for a file (computes if not cached)
    pub fn get_hash(&self, path: &Path) -> Result<String, Box<dyn std::error::Error>> {
        compute_file_hash(path)
    }
}

/// Hex-encode a raw blake3 digest.
///
/// The organizer caches digests as raw bytes (cheaper to compare); the manifest
/// stores them as hex so the file stays human-readable. This is the only place
/// that converts between the two, so the two representations cannot drift.
pub fn hash_to_hex(hash: &[u8; 32]) -> String {
    blake3::Hash::from(*hash).to_hex().to_string()
}

/// Compute the blake3 hash of a file, hex encoded.
pub fn compute_file_hash(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    Ok(hash_to_hex(&crate::organizer::hash_file(path)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_compute_file_hash() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "hello world").unwrap();

        let hash = compute_file_hash(&file_path).unwrap();
        // blake3 of "hello world"
        assert_eq!(hash, blake3::hash(b"hello world").to_hex().to_string());
    }

    /// The hex form must agree with the raw digest the organizer caches, or
    /// resume and verify would compare incompatible identities.
    #[test]
    fn test_hash_to_hex_matches_the_organizer_digest() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "photo bytes").unwrap();

        let raw = crate::organizer::hash_file(&file_path).unwrap();
        assert_eq!(hash_to_hex(&raw), compute_file_hash(&file_path).unwrap());
    }

    #[test]
    fn test_dedup_index_unique() {
        let temp_dir = TempDir::new().unwrap();
        let file1 = temp_dir.path().join("file1.txt");
        fs::write(&file1, "unique content").unwrap();

        let mut index = DedupIndex::new();
        match index.check_duplicate(&file1).unwrap() {
            DedupResult::Unique => {} // expected
            DedupResult::Duplicate(_) => panic!("First file should be unique"),
        }
    }

    #[test]
    fn test_dedup_index_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let file1 = temp_dir.path().join("file1.txt");
        let file2 = temp_dir.path().join("file2.txt");
        fs::write(&file1, "same content").unwrap();
        fs::write(&file2, "same content").unwrap();

        let mut index = DedupIndex::new();
        match index.check_duplicate(&file1).unwrap() {
            DedupResult::Unique => {}
            DedupResult::Duplicate(_) => panic!("First file should be unique"),
        }
        match index.check_duplicate(&file2).unwrap() {
            DedupResult::Unique => panic!("Second file should be a duplicate"),
            DedupResult::Duplicate(original) => {
                assert_eq!(original, file1);
            }
        }
    }
}
