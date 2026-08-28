// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

pub mod app;
pub mod archive;
pub mod dedup;
pub mod exif;
pub mod manifest;
pub mod metadata;
pub mod organizer;
pub mod stats;
pub mod verify;

use std::sync::atomic::{AtomicBool, Ordering};

/// Global shutdown flag, set when the user presses Ctrl+C.
///
/// Long-running loops should sample this periodically and stop early when it is
/// set. Use [`is_shutdown`] to read it and [`install_shutdown_handler`] to wire
/// it up to the process signal handler.
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Returns `true` once a shutdown has been requested (Ctrl+C).
pub fn is_shutdown() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

/// Request a shutdown. Exposed mainly for tests and for callers embedding the
/// library that want to cancel a run.
pub fn request_shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Install the Ctrl+C handler that sets [`SHUTDOWN`].
///
/// Idempotent: the underlying `ctrlc` crate allows exactly one handler per
/// process, so a second [`run`] in the same process (a test binary, or a
/// library consumer processing several takeouts) must not fail here.
pub fn install_shutdown_handler() -> Result<(), ctrlc::Error> {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    ctrlc::set_handler(|| {
        SHUTDOWN.store(true, Ordering::SeqCst);
        println!("\nReceived Ctrl+C, initiating graceful shutdown...");
    })
}

// Re-export commonly used items for integration testing and library consumers
pub use app::{AppConfig, REPORT_FILE_NAME, RunOutcome, parse_size_string, run};
pub use archive::{
    ArchiveError, ArchiveGroup, ExtractionSummary, create_temp_directory, detect_split_archives,
    find_archive_files,
};
pub use dedup::compute_file_hash;
pub use exif::{ExifBatchSummary, ExifError, escape_csv_field, unix_to_exif_datetime};
pub use manifest::{MANIFEST_FILE_NAME, Manifest, ManifestEntry, load_manifest, save_manifest};
pub use metadata::{
    GeoData, PhotoMetadata, Timestamp, derive_sidecar_target, find_media_files,
    find_media_files_with_stats, is_media_extension, load_metadata, pair_media_with_metadata,
};
pub use organizer::*;
pub use stats::{ProcessingStats, generate_summary};
pub use verify::{VerifyResult, verify_organized_files};
