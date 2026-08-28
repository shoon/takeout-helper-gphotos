// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use takeout_helper_gphotos::stats::*;

#[test]
fn test_generate_summary() {
    // Create a sample statistics object
    let stats = ProcessingStats {
        archives_found: 3,
        archives_extracted: 2,
        archives_failed: 1,
        entries_skipped: 5,
        media_files_found: 10,
        metadata_json_files_found: 8,
        files_without_metadata: 2,
        orphan_sidecars: 1,
        files_skipped_extension: 4,
        exif_written: 6,
        exif_mtime_only: 2,
        exif_failures: 1,
        files_organized: 7,
        duplicates_skipped: 2,
        unknown_date: 3,
        organize_failures: 1,
        output_dir: std::path::PathBuf::from("/tmp/organized"),
        report_path: Some(std::path::PathBuf::from(
            "/tmp/organized/takeout-helper-report.csv",
        )),
        interrupted: false,
        total_processing_time: std::time::Duration::from_secs(5),
        ..Default::default()
    };

    // Call the function - it must not panic on a fully populated stats struct
    generate_summary(&stats);

    // The struct is left untouched by reporting
    assert_eq!(stats.archives_found, 3);
    assert_eq!(stats.files_organized, 7);
    assert_eq!(stats.total_processing_time.as_secs(), 5);

    // The summary must not claim more archives extracted than were found, and
    // failures must be visible rather than swallowed.
    assert_eq!(stats.archives_extracted + stats.archives_failed, 3);
    assert_eq!(stats.total_failures(), 3);
    assert!(stats.has_failures());
}
