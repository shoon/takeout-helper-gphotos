// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use crate::dedup;
use crate::manifest::Manifest;
use log::{debug, error, info};
use std::path::{Path, PathBuf};

/// Result of the verification pass
#[derive(Debug, Default)]
pub struct VerifyResult {
    /// Number of files that passed verification
    pub verified: usize,
    /// Files that are in the manifest but missing from disk
    pub missing: Vec<PathBuf>,
    /// Files that exist but have a different hash than recorded
    pub mismatched: Vec<PathBuf>,
}

impl VerifyResult {
    /// Files that did not verify: missing plus mismatched.
    ///
    /// Any non-zero value here is a hard failure. A photo the library claims
    /// to hold is gone or corrupt.
    pub fn failures(&self) -> usize {
        self.missing.len() + self.mismatched.len()
    }
}

/// Verify that organized files match the manifest
pub fn verify_organized_files(output_dir: &Path, manifest: &Manifest) -> VerifyResult {
    info!("Starting verification pass on {}", output_dir.display());

    let mut verified = 0;
    let mut missing = Vec::new();
    let mut mismatched = Vec::new();

    for entry in manifest.entries.values() {
        let output_path = &entry.output_path;

        if !output_path.exists() {
            error!(
                "Verification failed: missing file {}",
                output_path.display()
            );
            missing.push(output_path.clone());
            continue;
        }

        // Verify hash
        match dedup::compute_file_hash(output_path) {
            Ok(hash) => {
                if hash == entry.hash {
                    debug!("Verified: {}", output_path.display());
                    verified += 1;
                } else {
                    error!(
                        "Verification failed: hash mismatch for {} (expected {}, got {})",
                        output_path.display(),
                        entry.hash,
                        hash
                    );
                    mismatched.push(output_path.clone());
                }
            }
            Err(e) => {
                error!(
                    "Verification failed: could not hash {}: {}",
                    output_path.display(),
                    e
                );
                mismatched.push(output_path.clone());
            }
        }
    }

    info!(
        "Verification complete: {} passed, {} missing, {} mismatched",
        verified,
        missing.len(),
        mismatched.len()
    );

    VerifyResult {
        verified,
        missing,
        mismatched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_verify_success() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        let hash = dedup::compute_file_hash(&file_path).unwrap();
        let mut manifest = Manifest::default();
        manifest.record(hash, file_path);

        let result = verify_organized_files(temp_dir.path(), &manifest);
        assert_eq!(result.verified, 1);
        assert!(result.missing.is_empty());
        assert!(result.mismatched.is_empty());
        assert_eq!(result.failures(), 0);
    }

    #[test]
    fn test_verify_missing() {
        let temp_dir = TempDir::new().unwrap();
        let mut manifest = Manifest::default();
        manifest.record("abc".to_string(), temp_dir.path().join("nonexistent.txt"));

        let result = verify_organized_files(temp_dir.path(), &manifest);
        assert_eq!(result.verified, 0);
        assert_eq!(result.missing.len(), 1);
        assert_eq!(result.failures(), 1);
    }

    /// A file that was edited or truncated after the run must be reported, not
    /// counted as verified.
    #[test]
    fn test_verify_detects_mismatched_content() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "original").unwrap();

        let hash = dedup::compute_file_hash(&file_path).unwrap();
        let mut manifest = Manifest::default();
        manifest.record(hash, file_path.clone());

        fs::write(&file_path, "tampered").unwrap();

        let result = verify_organized_files(temp_dir.path(), &manifest);
        assert_eq!(result.verified, 0);
        assert_eq!(result.mismatched, vec![file_path]);
        assert_eq!(result.failures(), 1);
    }
}
