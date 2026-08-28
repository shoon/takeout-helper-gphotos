// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! Statistics module for the Google Photos Takeout Helper
//!
//! This module contains data structures and functions for tracking and reporting
//! processing statistics throughout the application lifecycle.
//!
//! Every counter here is derived from a real per-phase summary. Nothing is
//! assumed-success: if a number is printed, something actually happened to make
//! it true. Failure totals and exit status are kept in sync.

use std::path::PathBuf;

/// Struct to hold statistics for the summary report.
///
/// The counters are grouped by pipeline phase, in the order the phases run.
#[derive(Debug, Default, Clone)]
pub struct ProcessingStats {
    // --- Archive phase ---
    /// Archive files discovered in the input directory.
    pub archives_found: usize,
    /// Archives that extracted without error.
    pub archives_extracted: usize,
    /// Archives that failed to extract (any `ArchiveError`).
    pub archives_failed: usize,
    /// Archive entries skipped during extraction (oversize + unsafe path).
    pub entries_skipped: usize,

    // --- Discovery / pairing phase ---
    /// Media files found in the extracted tree.
    pub media_files_found: usize,
    /// Files skipped during discovery because their extension is not media.
    pub files_skipped_extension: usize,
    /// JSON sidecars that were successfully paired with a media file.
    pub metadata_json_files_found: usize,
    /// Media files for which no sidecar could be found.
    pub files_without_metadata: usize,
    /// JSON sidecars that matched no media file.
    pub orphan_sidecars: usize,

    // --- EXIF phase ---
    /// Files that received embedded EXIF metadata.
    pub exif_written: usize,
    /// Videos whose QuickTime `mvhd` creation/modification times were patched.
    pub video_dates_written: usize,
    /// Files that could only receive a corrected modification time.
    pub exif_mtime_only: usize,
    /// Files whose metadata write failed.
    pub exif_failures: usize,

    // --- Organization phase ---
    /// Files copied into the output tree.
    pub files_organized: usize,
    /// Files skipped because a byte-identical copy was already present.
    pub duplicates_skipped: usize,
    /// Files filed under `unknown-date/` (a subset of `files_organized`).
    pub unknown_date: usize,
    /// Files that could not be organized.
    pub organize_failures: usize,
    /// Files skipped because the resume manifest already records them.
    pub resumed_skips: usize,
    /// Files skipped by `--skip-derivatives`.
    pub derivatives_skipped: usize,
    /// Extra copies written under album folders (`--organize date-album`).
    pub album_copies: usize,
    /// JSON sidecars copied next to their media file (`--copy-sidecars`).
    pub sidecars_copied: usize,
    /// `--dry-run`: files that would have been copied.
    pub planned_organize: usize,
    /// `--dry-run`: files that would have been skipped as duplicates.
    pub planned_duplicates: usize,

    // --- Verification phase (`--verify`) ---
    /// Whether the verification pass ran at all.
    pub verify_ran: bool,
    /// Organized files whose content matched the manifest.
    pub verified: usize,
    /// Files the manifest records that are no longer on disk.
    pub verify_missing: usize,
    /// Files whose content no longer matches what was recorded.
    pub verify_mismatched: usize,

    // --- Run context (not counters) ---
    /// Where the organized library was written.
    pub output_dir: PathBuf,
    /// Path of the machine-readable failure report, when one was written.
    pub report_path: Option<PathBuf>,
    /// Path of the resume manifest, when one was written.
    pub manifest_path: Option<PathBuf>,
    /// Whether the run stopped early because of Ctrl+C.
    pub interrupted: bool,
    /// Whether this was a `--dry-run`: nothing outside the scratch directory
    /// was written, and the organization numbers are projections.
    pub dry_run: bool,
    /// The output layout that was built (`date`, `album`, `flat`, `date-album`).
    pub organize_mode: Option<&'static str>,

    /// Wall-clock duration of the run.
    pub total_processing_time: std::time::Duration,
}

impl ProcessingStats {
    /// Files that did not survive verification.
    pub fn verify_failures(&self) -> usize {
        self.verify_missing + self.verify_mismatched
    }

    /// Total number of failures across every phase.
    pub fn total_failures(&self) -> usize {
        self.archives_failed + self.exif_failures + self.organize_failures + self.verify_failures()
    }

    /// True when the run produced at least one hard failure.
    pub fn has_failures(&self) -> bool {
        self.total_failures() > 0
    }
}

/// Format a duration as `1h 02m 03s` / `2m 03s` / `3.4s`.
fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!(
            "{}h {:02}m {:02}s",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    } else if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Generate the end-of-run summary report.
///
/// The report names the output directory, calls out failures prominently and
/// points at the machine-readable report when there is one.
pub fn generate_summary(stats: &ProcessingStats) {
    let rule = "=".repeat(64);
    println!("\n{}", rule);
    if stats.dry_run {
        println!("DRY RUN: nothing was written outside the scratch directory.");
        println!("No photos were copied, no metadata was written, and no manifest");
        println!("or report was saved. Below is what a real run would have done.");
    }
    if stats.interrupted {
        println!("RUN INTERRUPTED (Ctrl+C): the numbers below cover only the");
        println!("work that completed before the shutdown request.");
    } else if !stats.dry_run {
        println!("RUN SUMMARY");
    }
    println!("{}", rule);

    println!("\nArchives");
    println!("  found                     : {}", stats.archives_found);
    println!("  extracted                 : {}", stats.archives_extracted);
    println!("  FAILED                    : {}", stats.archives_failed);
    println!(
        "  entries skipped           : {}  (oversize or unsafe path)",
        stats.entries_skipped
    );

    println!("\nDiscovery");
    println!("  media files found         : {}", stats.media_files_found);
    println!(
        "  skipped (non-media ext)   : {}",
        stats.files_skipped_extension
    );
    println!(
        "  JSON sidecars paired      : {}",
        stats.metadata_json_files_found
    );
    println!(
        "  media without metadata    : {}",
        stats.files_without_metadata
    );
    println!("  orphan JSON sidecars      : {}", stats.orphan_sidecars);

    println!("\nMetadata writing");
    if stats.dry_run {
        println!("  skipped (dry run)         : no file was modified");
    } else {
        println!("  EXIF embedded             : {}", stats.exif_written);
        println!(
            "  video dates patched       : {}  (MP4/MOV mvhd creation time)",
            stats.video_dates_written
        );
        println!(
            "  modification time only    : {}  (formats whose container we cannot write)",
            stats.exif_mtime_only
        );
        println!("  FAILED                    : {}", stats.exif_failures);
    }

    println!("\nOrganization");
    if let Some(mode) = stats.organize_mode {
        println!("  layout                    : {}", mode);
    }
    if stats.dry_run {
        println!("  would be organized        : {}", stats.planned_organize);
        println!(
            "  would be skipped as dup   : {}  (byte-identical copy already present)",
            stats.planned_duplicates
        );
    } else {
        println!("  files organized           : {}", stats.files_organized);
        println!(
            "  duplicates skipped        : {}  (byte-identical copy already present)",
            stats.duplicates_skipped
        );
    }
    if stats.resumed_skips > 0 {
        println!(
            "  resumed (already done)    : {}  (recorded in {})",
            stats.resumed_skips,
            crate::manifest::MANIFEST_FILE_NAME
        );
    }
    if stats.derivatives_skipped > 0 {
        println!(
            "  derivatives skipped       : {}  (--skip-derivatives)",
            stats.derivatives_skipped
        );
    }
    if stats.album_copies > 0 {
        println!(
            "  album copies {:<13}: {}  (extra copies under <album>/)",
            if stats.dry_run { "planned" } else { "made" },
            stats.album_copies
        );
    }
    if stats.sidecars_copied > 0 {
        println!(
            "  sidecars {:<17}: {}  (--copy-sidecars)",
            if stats.dry_run { "planned" } else { "copied" },
            stats.sidecars_copied
        );
    }
    println!(
        "  filed as unknown-date     : {}  (subset of the files above)",
        stats.unknown_date
    );
    println!("  FAILED                    : {}", stats.organize_failures);

    if stats.verify_ran {
        println!("\nVerification");
        println!("  verified                  : {}", stats.verified);
        println!("  MISSING                   : {}", stats.verify_missing);
        println!("  MISMATCHED                : {}", stats.verify_mismatched);
    }

    println!("\nOutput");
    println!(
        "  library                   : {}",
        stats.output_dir.display()
    );
    if let Some(path) = &stats.manifest_path {
        println!("  resume manifest           : {}", path.display());
    }
    if stats.unknown_date > 0 {
        println!(
            "  {} file(s) had no trustworthy date and {} filed under:",
            stats.unknown_date,
            if stats.dry_run { "would be" } else { "were" }
        );
        println!(
            "      {}",
            stats
                .output_dir
                .join(crate::organizer::UNKNOWN_DATE_DIR)
                .display()
        );
    }

    println!(
        "\nTotal processing time       : {}",
        format_duration(stats.total_processing_time)
    );

    if stats.has_failures() {
        println!("\n{}", "!".repeat(64));
        println!(
            "{} FAILURE(S): {} archive(s), {} metadata write(s), {} file(s) not organized, \
             {} file(s) failed verification.",
            stats.total_failures(),
            stats.archives_failed,
            stats.exif_failures,
            stats.organize_failures,
            stats.verify_failures()
        );
        if stats.archives_failed > 0 {
            println!("Do NOT delete your original archives: some of them did not extract.");
        }
        if stats.verify_failures() > 0 {
            println!(
                "Verification failed: a file the manifest records is missing or its content \
                 changed. Do NOT delete your original archives."
            );
        }
        match &stats.report_path {
            Some(p) => println!("Per-file details: {}", p.display()),
            None => println!("See the log output above for per-file details."),
        }
        println!("{}", "!".repeat(64));
    } else if !stats.interrupted {
        println!("\nNo failures.");
    }
    println!("{}\n", rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_failures_sums_every_phase() {
        let stats = ProcessingStats {
            archives_failed: 2,
            exif_failures: 3,
            organize_failures: 4,
            ..Default::default()
        };
        assert_eq!(stats.total_failures(), 9);
        assert!(stats.has_failures());
        assert!(!ProcessingStats::default().has_failures());
    }

    /// A file that vanished or changed under us is a hard failure, so the run
    /// exits non-zero instead of implying the library is intact.
    #[test]
    fn verification_failures_count_as_failures() {
        let stats = ProcessingStats {
            verify_ran: true,
            verified: 10,
            verify_missing: 1,
            verify_mismatched: 2,
            ..Default::default()
        };
        assert_eq!(stats.verify_failures(), 3);
        assert_eq!(stats.total_failures(), 3);
        assert!(stats.has_failures());

        let clean = ProcessingStats {
            verify_ran: true,
            verified: 10,
            ..Default::default()
        };
        assert!(!clean.has_failures());
    }

    /// The dry-run summary must not print counters no dry run could produce.
    #[test]
    fn dry_run_summary_does_not_panic() {
        let stats = ProcessingStats {
            dry_run: true,
            planned_organize: 4,
            planned_duplicates: 1,
            unknown_date: 1,
            organize_mode: Some("date-album"),
            ..Default::default()
        };
        generate_summary(&stats);
        assert!(!stats.has_failures());
    }

    #[test]
    fn format_duration_is_readable() {
        use std::time::Duration;
        assert_eq!(format_duration(Duration::from_secs_f64(3.42)), "3.4s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 05s");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1h 02m 05s");
    }
}
