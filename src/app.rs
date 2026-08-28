// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! Application orchestration.
//!
//! This module owns the whole processing pipeline. The binary in `main.rs` is a
//! thin shim that parses CLI arguments, sets up logging and calls [`run`].

use chrono::{DateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};
use log::{error, info, warn};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::archive::ArchiveError;
use crate::exif::escape_csv_field;
use crate::manifest::Manifest;
use crate::metadata::{MEDIA_EXTENSIONS, MediaMetadataPair};
use crate::organizer::{LivePhotoDates, OrganizeMode, UNKNOWN_DATE_DIR, live_photo_key};
use crate::stats::ProcessingStats;
use crate::{archive, exif, manifest, metadata, organizer, stats, verify};

/// Name of the machine-readable per-file report written into the output dir.
pub const REPORT_FILE_NAME: &str = "takeout-helper-report.csv";

const OVERALL_PROGRESS_TEMPLATE: &str = "{spinner:.green} {msg}";
const VERIFY_PROGRESS_TEMPLATE: &str = "  {spinner:.green} {percent:>3}% Verify {pos}/{len}";
const SETUP_STAGE_COUNT: usize = 2;

/// Configuration for a single run of the pipeline.
///
/// No `clap` types are used here, which keeps the library independent of the CLI
/// layer.
#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    /// Directory containing the Google Photos takeout `.zip`/`.tgz` archives.
    pub input: PathBuf,
    /// Directory where the organized photos will be written.
    pub output: PathBuf,
    /// Search the input directory recursively.
    pub recursive: bool,
    /// Directory used for extraction (defaults to `<output>/temp`).
    pub temp_dir: Option<PathBuf>,
    /// Maximum uncompressed size per file, as a human readable string.
    pub max_file_size: Option<String>,
    /// Maximum total uncompressed size per archive, as a human readable string.
    pub max_archive_size: Option<String>,
    /// Maximum number of entries extracted from a single archive.
    pub max_files: Option<u64>,
    /// Leave the generated temporary extraction directory on disk.
    pub keep_temp: bool,
    /// Work out what the run would do, writing nothing outside the scratch
    /// directory: no EXIF, no copies, no manifest, no report.
    pub dry_run: bool,
    /// The output layout to build.
    pub organize: OrganizeMode,
    /// Ignore the resume manifest and reprocess every file.
    pub force: bool,
    /// Do not skip byte-identical duplicates (the `_N` collision loop still runs).
    pub no_dedup: bool,
    /// Copy each media file's JSON sidecar next to the organized copy.
    pub copy_sidecars: bool,
    /// Leave Google-generated derivatives such as `-edited` and `-pano` behind.
    pub skip_derivatives: bool,
    /// Re-hash the organized library against the manifest when the run ends.
    pub verify: bool,
    /// Timezone to render the EXIF date tags in. `None` derives each file's
    /// zone from its own GPS coordinates, falling back to UTC.
    pub timezone: Option<chrono_tz::Tz>,
}

/// How a run finished.
#[derive(Debug)]
pub enum RunOutcome {
    /// Everything the pipeline attempted succeeded. Exit code 0.
    Success(ProcessingStats),
    /// The pipeline ran to the end but something failed along the way. A failed
    /// archive, a failed metadata write, a file that could not be organized, or
    /// no archives found at all. Exit code 1.
    CompletedWithErrors(ProcessingStats),
    /// The run stopped early because a shutdown was requested (Ctrl+C).
    /// Exit code 130, the conventional `128 + SIGINT`.
    Interrupted(ProcessingStats),
}

impl RunOutcome {
    /// Process exit code corresponding to this outcome.
    pub fn exit_code(&self) -> i32 {
        match self {
            RunOutcome::Success(_) => 0,
            RunOutcome::CompletedWithErrors(_) => 1,
            RunOutcome::Interrupted(_) => 130,
        }
    }

    /// The statistics gathered before the run ended, whatever the outcome.
    pub fn stats(&self) -> &ProcessingStats {
        match self {
            RunOutcome::Success(s)
            | RunOutcome::CompletedWithErrors(s)
            | RunOutcome::Interrupted(s) => s,
        }
    }
}

/// One row of the end-of-run CSV report.
struct ReportRow {
    phase: &'static str,
    source: String,
    destination: String,
    detail: String,
}

/// State needed to close a run and its overall progress indicator.
struct FinalizeStatus<'a> {
    interrupted: bool,
    overall_progress: &'a ProgressBar,
}

/// User-visible stages in the order they advance the overall progress bar.
fn pipeline_stages(verify: bool) -> Vec<&'static str> {
    let mut stages = vec![
        "Preparing run",
        "Discovering archives",
        "Extracting archives",
        "Discovering media files",
        "Pairing media with sidecars",
        "Indexing Live Photos",
        "Writing EXIF metadata",
        "Organizing files",
    ];
    if verify {
        stages.push("Verifying organized files");
    }
    stages.push("Finalizing output");
    stages
}

/// Name the current processing step without pretending that equally sized
/// steps represent equal amounts of time or work.
fn set_processing_stage(progress: &ProgressBar, name: &str) {
    let total = progress.length().unwrap_or(0);
    let step = progress.position().saturating_add(1).min(total);
    progress.set_message(format!("Step {step} of {total}: {name}"));
}

/// The sidecar capture date of a pair, if it has one.
///
/// Deliberately *only* looks at the JSON sidecar: filesystem mtimes must never
/// seed the Live Photo map, or a video could inherit a meaningless date.
fn sidecar_date(pair: &MediaMetadataPair) -> Option<DateTime<Utc>> {
    let MediaMetadataPair::WithMetadata(_, metadata, _) = pair else {
        return None;
    };
    for ts in [
        metadata
            .photo_taken_time
            .as_ref()
            .and_then(|t| t.timestamp.as_ref()),
        metadata
            .creation_time
            .as_ref()
            .and_then(|t| t.timestamp.as_ref()),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(date) = organizer::parse_unix_timestamp(ts) {
            return Some(date);
        }
    }
    None
}

/// Map Live Photo still images to their authoritative sidecar dates, so their
/// `.MP4`/`.MOV` companions can borrow them.
///
/// Keyed by `(parent directory, lower-cased stem)`. A bare stem collides across
/// the whole takeout and lets `Photos from 2015/IMG_0001.jpg` date a completely
/// unrelated 2022 video.
fn build_live_photo_dates_map(media_metadata_pairs: &[MediaMetadataPair]) -> LivePhotoDates {
    let mut live_photo_dates: LivePhotoDates = HashMap::new();

    // Extensions that can be the still half of a Live Photo.
    const PRIMARY_PHOTO_EXTENSIONS: &[&str] = &["heic", "heif", "jpg", "jpeg"];

    for pair in media_metadata_pairs {
        let path = match pair {
            MediaMetadataPair::WithMetadata(path, ..) => path,
            MediaMetadataPair::WithoutMetadata(path) => path,
        };

        let Some(extension) = path.extension() else {
            continue;
        };
        let ext_str = extension.to_string_lossy().to_lowercase();
        if !PRIMARY_PHOTO_EXTENSIONS.contains(&ext_str.as_str()) {
            continue;
        }

        // Only real sidecar dates may seed the map; undated photos and
        // filesystem mtimes must not poison their Live Photo videos.
        if let Some(date) = sidecar_date(pair) {
            live_photo_dates.insert(live_photo_key(path), date);
        }
    }

    live_photo_dates
}

/// Parse human-readable size string (e.g., "100M", "10G", "200 GB") into bytes.
///
/// Whitespace is ignored, so the forms used in the README parse.
pub fn parse_size_string(size_str: &str) -> Result<u64, Box<dyn std::error::Error>> {
    // Uppercase for easier parsing and drop *all* whitespace so "200 GB" works.
    let size_str: String = size_str
        .to_uppercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();

    if size_str.is_empty() {
        return Err("empty size value".into());
    }

    // Split the string into numeric and unit parts
    let mut numeric_part = String::new();
    let mut unit_part = String::new();

    let mut chars = size_str.chars();
    for ch in chars.by_ref() {
        if ch.is_ascii_digit() || ch == '.' {
            numeric_part.push(ch);
        } else {
            unit_part.push(ch);
            unit_part.extend(chars);
            break;
        }
    }

    // Parse the numeric part
    let numeric_value: f64 = numeric_part
        .parse()
        .map_err(|e| format!("invalid number '{}': {}", numeric_part, e))?;

    if !numeric_value.is_finite() || numeric_value < 0.0 {
        return Err(format!("size must be a positive number, got '{}'", numeric_part).into());
    }

    // Convert based on unit
    let multiplier = match unit_part.as_str() {
        "K" | "KB" => 1024u64,
        "M" | "MB" => 1024u64 * 1024u64,
        "G" | "GB" => 1024u64 * 1024u64 * 1024u64,
        "T" | "TB" => 1024u64 * 1024u64 * 1024u64 * 1024u64,
        "B" | "" => 1u64, // No unit, assume bytes
        _ => {
            return Err(format!(
                "unknown size unit '{}' (expected one of B, K/KB, M/MB, G/GB, T/TB)",
                unit_part
            )
            .into());
        }
    };

    // Calculate the final value
    Ok((numeric_value * multiplier as f64) as u64)
}

/// Parse an optional size string. A parse failure is **fatal** because silently
/// falling back to the default would let a typo quietly change the limits.
fn parse_optional_size(
    label: &str,
    size_str: Option<&String>,
) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    match size_str {
        Some(size_str) => match parse_size_string(size_str) {
            Ok(size) => Ok(Some(size)),
            Err(e) => Err(format!("invalid value '{}' for {}: {}", size_str, label, e).into()),
        },
        None => Ok(None),
    }
}

/// Write the per-file report CSV, returning its path when anything was written.
///
/// Every row is a file that needs the user's attention: a failure in any phase,
/// or a file that had to be filed under `unknown-date/`.
///
/// `unknown-date` and failure rows carry the **output** path wherever one is
/// known, because the temporary extraction directory is deleted at the end of
/// the run and its paths are useless to the reader.
fn write_report(output_dir: &Path, rows: &[ReportRow]) -> std::io::Result<Option<PathBuf>> {
    if rows.is_empty() {
        return Ok(None);
    }

    let path = output_dir.join(REPORT_FILE_NAME);
    let mut file = fs::File::create(&path)?;
    writeln!(file, "phase,source,destination,detail")?;
    for row in rows {
        writeln!(
            file,
            "{},{},{},{}",
            escape_csv_field(row.phase),
            escape_csv_field(&row.source),
            escape_csv_field(&row.destination),
            escape_csv_field(&row.detail)
        )?;
    }
    info!("Wrote {} report rows to {}", rows.len(), path.display());
    Ok(Some(path))
}

/// The message printed when the input directory contains no archives at all.
fn no_archives_message(input: &Path, recursive: bool) -> String {
    format!(
        "No archives found in {}.\n\
         Searched for: .zip, .tgz, .tar.gz{}\n\
         Check that the path is correct and that it contains your Google Takeout \
         download (the files are usually named takeout-<date>-NNN.zip).{}",
        input.display(),
        if recursive {
            " (recursively)"
        } else {
            " (top level only)"
        },
        if recursive {
            ""
        } else {
            "\nIf the archives are in subdirectories, re-run with --recursive."
        }
    )
}

/// Run the full Google Photos takeout processing pipeline.
pub fn run(config: AppConfig) -> Result<RunOutcome, Box<dyn std::error::Error>> {
    // Initialize overall progress tracking
    let verify_stage = config.verify && !config.dry_run;
    let stages = pipeline_stages(verify_stage);
    // Preparation and archive discovery establish what work exists but are not
    // numbered processing steps. Extraction starts at step 1.
    let processing_stage_count = stages.len().saturating_sub(SETUP_STAGE_COUNT);
    let overall_pb = crate::progress::add(ProgressBar::new(processing_stage_count as u64));
    overall_pb.set_style(
        ProgressStyle::default_bar()
            .template(OVERALL_PROGRESS_TEMPLATE)?
            .progress_chars("#>-"),
    );
    overall_pb.enable_steady_tick(Duration::from_millis(120));
    overall_pb.set_message(stages[0]);

    // Set up signal handler for Ctrl+C
    crate::install_shutdown_handler().expect("Error setting Ctrl+C handler");

    // Pin the run-start instant so that mtimes written later in this run are
    // recognised as meaningless (see organizer::meaningful_filesystem_date).
    organizer::mark_run_start();

    // Initialize statistics
    let mut stats = ProcessingStats {
        output_dir: config.output.clone(),
        dry_run: config.dry_run,
        organize_mode: Some(config.organize.as_str()),
        ..Default::default()
    };
    let start_time = Instant::now();
    // Rows for the end-of-run report; collected across every phase.
    let mut report_rows: Vec<ReportRow> = Vec::new();

    // Validate input path
    if !config.input.exists() {
        return Err(format!("Input path does not exist: {}", config.input.display()).into());
    }

    if !config.input.is_dir() {
        return Err(format!("Input path is not a directory: {}", config.input.display()).into());
    }

    info!("Input directory validated: {}", config.input.display());

    // Validate output path (create if it doesn't exist)
    if !config.output.exists() {
        fs::create_dir_all(&config.output)?;
        info!("Created output directory: {}", config.output.display());
    } else if !config.output.is_dir() {
        return Err(format!(
            "Output path exists but is not a directory: {}",
            config.output.display()
        )
        .into());
    }

    info!("Output directory validated: {}", config.output.display());

    if config.dry_run {
        crate::progress::println(format!(
            "DRY RUN: archives will be extracted to scratch space so the run can be planned, \
             but nothing will be written into {}.",
            config.output.display()
        ));
        if config.verify {
            warn!("--verify does nothing during a --dry-run: there is no manifest to verify");
        }
    }

    // The resume manifest. `--force` still loads it because throwing away the
    // resume record of the rest of the library would be a much bigger hammer
    // than "reprocess the files in this run." It is not consulted below.
    let mut manifest = if config.dry_run {
        Manifest::default()
    } else {
        manifest::load_manifest(&config.output)
    };
    if config.force && !manifest.is_empty() {
        info!(
            "--force: ignoring {} manifest entries; every file will be reprocessed",
            manifest.len()
        );
    }

    // Sizes are parsed before anything is touched so a typo fails immediately
    // rather than silently changing the limits.
    let max_file_size = parse_optional_size("--max-file-size", config.max_file_size.as_ref())?;
    let max_archive_size =
        parse_optional_size("--max-archive-size", config.max_archive_size.as_ref())?;
    // Archive processing phase
    info!("Starting archive processing phase");
    overall_pb.set_message("Discovering archives");

    let archive_files = match archive::find_archive_files(&config.input, config.recursive) {
        Ok(files) => files,
        Err(e) => {
            error!("Archive discovery failed: {}", e);
            vec![] // Continue with empty list
        }
    };
    stats.archives_found = archive_files.len();

    // Finding nothing at all is a *failure*, not a silent success.
    if archive_files.is_empty() {
        overall_pb.finish_and_clear();
        let message = no_archives_message(&config.input, config.recursive);
        error!("{}", message);
        crate::progress::eprintln(format!("\n{}\n", message));
        stats.total_processing_time = start_time.elapsed();
        stats::generate_summary(&stats);
        return Ok(RunOutcome::CompletedWithErrors(stats));
    }

    // Advisory: a numbered shard sequence with a hole in it means missing
    // photos, and the user needs to know *before* the run, not after.
    archive::report_split_archives(&archive::detect_split_archives(&archive_files));

    // Create a temporary directory for extraction
    // Use the custom temp directory if provided, otherwise create a default one in the output directory
    //
    // IMPORTANT: the directory the *user* names is never deleted. We always
    // create a uniquely named `gphotos-takeout-<random>` subdirectory inside it
    // and only that subdirectory is removed when the run ends.
    let temp_dir_base = match config.temp_dir {
        Some(ref temp_dir_path) => {
            if temp_dir_path.exists() && !temp_dir_path.is_dir() {
                return Err(format!(
                    "Custom temporary directory path exists but is not a directory: {}",
                    temp_dir_path.display()
                )
                .into());
            }
            temp_dir_path.clone()
        }
        None => {
            let default_temp_dir_path = config.output.join("temp");
            if default_temp_dir_path.exists() && !default_temp_dir_path.is_dir() {
                return Err(format!(
                    "Default temporary directory path exists but is not a directory: {}",
                    default_temp_dir_path.display()
                )
                .into());
            }
            default_temp_dir_path
        }
    };

    let temp_dir = archive::TempDir::create_inside(&temp_dir_base).map_err(|e| {
        format!(
            "Could not create a temporary extraction directory inside {}: {}",
            temp_dir_base.display(),
            e
        )
    })?;

    // Log the temp directory path on application startup
    info!("Using temporary directory: {}", temp_dir.path().display());

    set_processing_stage(
        &overall_pb,
        &format!(
            "Extracting {} archive{}",
            archive_files.len(),
            if archive_files.len() == 1 { "" } else { "s" }
        ),
    );
    let extraction_results = archive::extract_archives(
        archive_files,
        temp_dir.path(),
        max_file_size,
        max_archive_size,
        config.max_files,
    );

    // Honest accounting: only archives that actually extracted are counted, and
    // every failure is reported and reflected in the exit code.
    let mut archive_interrupted = false;
    for (archive_path, result) in &extraction_results {
        match result {
            Ok(summary) => {
                stats.archives_extracted += 1;
                stats.entries_skipped += summary.skipped_oversize + summary.skipped_unsafe;
                if summary.skipped_oversize > 0 || summary.skipped_unsafe > 0 {
                    report_rows.push(ReportRow {
                        phase: "archive-entries-skipped",
                        source: archive_path.display().to_string(),
                        destination: String::new(),
                        detail: format!(
                            "{} entries skipped as oversize, {} as unsafe paths",
                            summary.skipped_oversize, summary.skipped_unsafe
                        ),
                    });
                }
            }
            Err(e) => {
                stats.archives_failed += 1;
                if matches!(e, ArchiveError::Interrupted) {
                    archive_interrupted = true;
                }
                error!(
                    "Archive extraction failed for {}: {}",
                    archive_path.display(),
                    e
                );
                report_rows.push(ReportRow {
                    phase: "archive",
                    source: archive_path.display().to_string(),
                    destination: String::new(),
                    detail: e.to_string(),
                });
            }
        }
    }

    if stats.archives_failed > 0 {
        error!(
            "{} of {} archives failed to extract. Do not delete your originals",
            stats.archives_failed,
            extraction_results.len()
        );
    }

    if crate::is_shutdown() || archive_interrupted {
        return Ok(finish_interrupted(
            &mut stats,
            report_rows,
            start_time,
            &config,
            temp_dir,
            Some(&manifest),
            &overall_pb,
        ));
    }
    overall_pb.inc(1);
    info!("Archive processing phase completed");

    // Metadata processing phase
    info!("Starting metadata processing phase");
    set_processing_stage(&overall_pb, "Discovering media files");

    // Find all media files in the extracted content
    let media_files = match metadata::find_media_files_with_stats(temp_dir.path(), &mut stats) {
        Ok(files) => {
            stats.media_files_found = files.len();
            files
        }
        Err(e) => {
            error!("Media file discovery failed: {}", e);
            vec![] // Continue with empty list
        }
    };
    overall_pb.inc(1);

    // Pair media files with their JSON metadata
    set_processing_stage(&overall_pb, "Pairing media with sidecars");
    let media_metadata_pairs = match metadata::pair_media_with_metadata(media_files, &mut stats) {
        Ok(pairs) => pairs,
        Err(e) => {
            error!("Metadata pairing failed: {}", e);
            vec![] // Continue with empty list
        }
    };

    if crate::is_shutdown() {
        return Ok(finish_interrupted(
            &mut stats,
            report_rows,
            start_time,
            &config,
            temp_dir,
            Some(&manifest),
            &overall_pb,
        ));
    }
    overall_pb.inc(1);
    info!("Metadata processing phase completed");

    // Live Photo pre-processing phase
    set_processing_stage(&overall_pb, "Indexing Live Photos");
    info!("Starting Live Photo pre-processing phase");
    let live_photo_dates = build_live_photo_dates_map(&media_metadata_pairs);
    info!(
        "Live Photo pre-processing phase completed: {} Live Photo dates mapped",
        live_photo_dates.len()
    );
    overall_pb.inc(1);

    // EXIF processing phase
    info!("Starting EXIF processing phase");
    set_processing_stage(&overall_pb, "Writing EXIF metadata");

    // Write EXIF metadata to media files. The batch borrows the pairs, so no
    // clone is needed here.
    let mut exif_interrupted = false;
    // source -> failure message, so the report can name the *output* path once
    // the organizer has told us where each source landed.
    let mut exif_failures: Vec<(PathBuf, String)> = Vec::new();
    if config.dry_run {
        // A dry run must not modify the extracted files either: the EXIF phase
        // rewrites them in place, and `--keep-temp` would then hand the user a
        // scratch directory that had been quietly altered.
        info!("Skipping the EXIF phase: --dry-run");
        crate::progress::println(format!(
            "DRY RUN: metadata writes skipped ({} files paired)",
            media_metadata_pairs.len()
        ));
    } else {
        match exif::write_exif_metadata_batch_with_tz(&media_metadata_pairs, config.timezone) {
            Ok(mut exif_summary) => {
                stats.exif_written = exif_summary.exif_written;
                stats.exif_fresh_blocks = exif_summary.fresh_exif_blocks;
                stats.video_dates_written = exif_summary.video_dates_written;
                stats.exif_mtime_only = exif_summary.mtime_only;
                stats.exif_failures = exif_summary.failures.len();
                exif_failures = std::mem::take(&mut exif_summary.failures);
                if exif_summary.not_processed > 0 {
                    // Ctrl+C landed mid-phase.
                    exif_interrupted = true;
                    warn!(
                        "EXIF phase interrupted: {} files were not processed",
                        exif_summary.not_processed
                    );
                }
                info!(
                    "EXIF processing phase completed: {} written, {} mtime-only, {} without metadata, {} failed",
                    exif_summary.exif_written,
                    exif_summary.mtime_only,
                    exif_summary.skipped_no_metadata,
                    exif_summary.failures.len()
                );
            }
            Err(e) => {
                error!("EXIF processing failed: {}", e);
                report_rows.push(ReportRow {
                    phase: "exif",
                    source: String::new(),
                    destination: String::new(),
                    detail: format!("EXIF phase aborted: {}", e),
                });
                stats.exif_failures += 1;
            }
        }
    }

    if crate::is_shutdown() || exif_interrupted {
        for (source, detail) in exif_failures {
            report_rows.push(ReportRow {
                phase: "exif",
                source: source.display().to_string(),
                destination: String::new(),
                detail,
            });
        }
        return Ok(finish_interrupted(
            &mut stats,
            report_rows,
            start_time,
            &config,
            temp_dir,
            Some(&manifest),
            &overall_pb,
        ));
    }
    overall_pb.inc(1);

    // Chronological organization phase
    info!("Starting chronological organization phase");
    set_processing_stage(&overall_pb, "Organizing files");

    // source -> final output path, used to translate temp paths in the report.
    let mut destinations: HashMap<PathBuf, PathBuf> = HashMap::new();

    let options = organizer::OrganizeOptions {
        mode: config.organize,
        // Album names come from the folder each file sits in *under the
        // extraction root. Every archive is extracted into this one scratch
        // directory, so the `Takeout/Google Photos/<folder>` structure is
        // relative to it.
        extract_root: Some(temp_dir.path()),
        dry_run: config.dry_run,
        dedup: !config.no_dedup,
        copy_sidecars: config.copy_sidecars,
        skip_derivatives: config.skip_derivatives,
        resume: if config.force || config.dry_run {
            None
        } else {
            Some(&manifest)
        },
        record_manifest: !config.dry_run,
    };
    let organize_result = organizer::organize_media_files_with_options(
        media_metadata_pairs,
        &config.output,
        &live_photo_dates,
        &options,
    );
    // `options` is not used again, so its borrow of `manifest` ends here and the
    // records collected below can be merged into it.
    match organize_result {
        Ok(summary) => {
            stats.files_organized = summary.organized;
            stats.duplicates_skipped = summary.duplicates_skipped;
            stats.unknown_date = summary.unknown_date;
            stats.organize_failures = summary.failure_count();
            stats.resumed_skips = summary.resumed_skips;
            stats.derivatives_skipped = summary.derivatives_skipped;
            stats.album_copies = summary.album_copies;
            stats.sidecars_copied = summary.sidecars_copied;
            stats.planned_organize = summary.planned;
            stats.planned_duplicates = summary.planned_duplicates;
            info!(
                "Organization phase completed: {} organized, {} duplicates skipped, {} resumed, {} undated, {} errors",
                summary.organized,
                summary.duplicates_skipped,
                summary.resumed_skips,
                summary.unknown_date,
                summary.failure_count()
            );

            destinations = summary.destinations.iter().cloned().collect();

            // Everything the organizer placed goes into the resume manifest, so
            // an interrupted run can pick up where it stopped.
            for (hash, destination) in &summary.records {
                if let Err(error) =
                    manifest.record(&config.output, hash.clone(), destination.as_path())
                {
                    error!(
                        "Could not record {} in the resume manifest: {}",
                        destination.display(),
                        error
                    );
                    stats.organize_failures += 1;
                    report_rows.push(ReportRow {
                        phase: "organize",
                        source: String::new(),
                        destination: destination.display().to_string(),
                        detail: format!("Could not record output in the resume manifest: {error}"),
                    });
                }
            }

            for (source, detail) in &summary.failures {
                report_rows.push(ReportRow {
                    phase: "organize",
                    source: source.display().to_string(),
                    destination: String::new(),
                    detail: detail.clone(),
                });
            }

            // The photo itself is in place, but an album copy or a sidecar is
            // not. Worth the user's attention, not worth an exit code.
            for (source, detail) in &summary.warnings {
                report_rows.push(ReportRow {
                    phase: "organize-warning",
                    source: source.display().to_string(),
                    destination: destinations
                        .get(source)
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    detail: detail.clone(),
                });
            }

            for source in &summary.derivatives {
                report_rows.push(ReportRow {
                    phase: "derivative-skipped",
                    source: source.display().to_string(),
                    destination: String::new(),
                    detail: "Looks like a Google-generated derivative; skipped because \
                             --skip-derivatives was given"
                        .to_string(),
                });
            }

            // Undated files, reported by their FINAL location. The old CSV
            // listed temp paths that were deleted before the user could read
            // them.
            let unknown_dir = config.output.join(UNKNOWN_DATE_DIR);
            for (source, destination) in &summary.destinations {
                if destination.starts_with(&unknown_dir) {
                    report_rows.push(ReportRow {
                        phase: "unknown-date",
                        source: source.display().to_string(),
                        destination: destination.display().to_string(),
                        detail: "No trustworthy capture date; filed under unknown-date/"
                            .to_string(),
                    });
                }
            }
        }
        Err(e) => {
            error!("Chronological organization failed: {}", e);
            report_rows.push(ReportRow {
                phase: "organize",
                source: String::new(),
                destination: String::new(),
                detail: format!("Organization phase aborted: {}", e),
            });
            stats.organize_failures += 1;
        }
    }

    // EXIF failures are reported last so they can name the output path the
    // organizer chose for that source file.
    for (source, detail) in exif_failures {
        let destination = destinations
            .get(&source)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        report_rows.push(ReportRow {
            phase: "exif",
            source: source.display().to_string(),
            destination,
            detail,
        });
    }
    overall_pb.inc(1);

    let mut interrupted = crate::is_shutdown();

    // Verification: re-hash everything the manifest claims is in the library.
    // Runs last and never during a dry run because there is nothing on disk to
    // check.
    if config.verify && !config.dry_run && !interrupted {
        set_processing_stage(&overall_pb, "Verifying organized files");
        let verify_pb = crate::progress::add(ProgressBar::new(manifest.len() as u64));
        verify_pb.set_style(
            ProgressStyle::default_bar()
                .template(VERIFY_PROGRESS_TEMPLATE)?
                .progress_chars("#>-"),
        );
        verify_pb.enable_steady_tick(Duration::from_millis(120));
        let result =
            verify::verify_organized_files_with_progress(&config.output, &manifest, &verify_pb);
        verify_pb.finish_and_clear();
        stats.verify_ran = true;
        stats.verified = result.verified;
        stats.verify_missing = result.missing.len();
        stats.verify_mismatched = result.mismatched.len();

        for path in &result.missing {
            report_rows.push(ReportRow {
                phase: "verify-failed",
                source: String::new(),
                destination: path.display().to_string(),
                detail: "Recorded in the manifest but missing from the library".to_string(),
            });
        }
        for path in &result.mismatched {
            report_rows.push(ReportRow {
                phase: "verify-failed",
                source: String::new(),
                destination: path.display().to_string(),
                detail: "Content does not match the hash recorded when it was organized"
                    .to_string(),
            });
        }

        interrupted = crate::is_shutdown();
        if !interrupted {
            overall_pb.inc(1);
        }
    }

    finalize(
        &mut stats,
        report_rows,
        start_time,
        &config,
        temp_dir,
        Some(&manifest),
        FinalizeStatus {
            interrupted,
            overall_progress: &overall_pb,
        },
    );

    Ok(if interrupted {
        RunOutcome::Interrupted(stats)
    } else if stats.has_failures() {
        RunOutcome::CompletedWithErrors(stats)
    } else {
        RunOutcome::Success(stats)
    })
}

/// Common tail for an interrupted run.
fn finish_interrupted(
    stats: &mut ProcessingStats,
    report_rows: Vec<ReportRow>,
    start_time: Instant,
    config: &AppConfig,
    temp_dir: archive::TempDir,
    manifest: Option<&Manifest>,
    overall_pb: &ProgressBar,
) -> RunOutcome {
    finalize(
        stats,
        report_rows,
        start_time,
        config,
        temp_dir,
        manifest,
        FinalizeStatus {
            interrupted: true,
            overall_progress: overall_pb,
        },
    );
    RunOutcome::Interrupted(stats.clone())
}

/// Write the report and the manifest, dispose of the temp dir and print the
/// summary.
///
/// Every exit path, including success, failure, and Ctrl+C, goes through here so
/// the resume manifest is always saved and a second run never redoes work the
/// first one finished.
fn finalize(
    stats: &mut ProcessingStats,
    report_rows: Vec<ReportRow>,
    start_time: Instant,
    config: &AppConfig,
    temp_dir: archive::TempDir,
    manifest: Option<&Manifest>,
    status: FinalizeStatus<'_>,
) {
    let FinalizeStatus {
        interrupted,
        overall_progress: overall_pb,
    } = status;
    set_processing_stage(overall_pb, "Finalizing output");
    stats.interrupted = interrupted;
    stats.total_processing_time = start_time.elapsed();

    // A dry run writes nothing outside the scratch directory: no report, no
    // manifest.
    if !config.dry_run {
        match write_report(&config.output, &report_rows) {
            Ok(path) => stats.report_path = path,
            Err(e) => error!(
                "Could not write the report to {}: {}",
                config.output.join(REPORT_FILE_NAME).display(),
                e
            ),
        }

        if let Some(manifest) = manifest {
            match manifest::save_manifest(manifest, &config.output) {
                Ok(path) => stats.manifest_path = Some(path),
                Err(e) => error!(
                    "Could not save the resume manifest to {}: {}. The next run will \
                     reprocess everything.",
                    config.output.join(manifest::MANIFEST_FILE_NAME).display(),
                    e
                ),
            }
        }
    }

    if config.keep_temp {
        let kept = temp_dir.keep();
        crate::progress::println(format!(
            "--keep-temp: extracted files left in {} (delete it yourself when done)",
            kept.display()
        ));
    } else {
        drop(temp_dir);
    }

    overall_pb.inc(1);
    overall_pb.finish_and_clear();

    stats::generate_summary(stats);

    if interrupted {
        crate::progress::println(format!(
            "Interrupted before completion. Media extensions searched: {}",
            MEDIA_EXTENSIONS.join(", ")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{PhotoMetadata, Timestamp};

    fn pair_with(path: &str, ts: Option<&str>) -> MediaMetadataPair {
        match ts {
            Some(ts) => {
                let metadata = PhotoMetadata {
                    photo_taken_time: Some(Timestamp {
                        timestamp: Some(ts.to_string()),
                        formatted: None,
                    }),
                    ..Default::default()
                };
                MediaMetadataPair::WithMetadata(PathBuf::from(path), Box::new(metadata), None)
            }
            None => MediaMetadataPair::WithoutMetadata(PathBuf::from(path)),
        }
    }

    #[test]
    fn test_parse_size_string_units() {
        assert_eq!(parse_size_string("100").unwrap(), 100);
        assert_eq!(parse_size_string("1K").unwrap(), 1024);
        assert_eq!(parse_size_string("2mb").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size_string("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(
            parse_size_string("1T").unwrap(),
            1024u64 * 1024 * 1024 * 1024
        );
        assert!(parse_size_string("10X").is_err());
        assert!(parse_size_string("abc").is_err());
    }

    #[test]
    fn test_parse_size_string_accepts_whitespace() {
        // The form the README uses.
        assert_eq!(
            parse_size_string("200 GB").unwrap(),
            200 * 1024 * 1024 * 1024
        );
        assert_eq!(
            parse_size_string("  50 g  ").unwrap(),
            50 * 1024 * 1024 * 1024
        );
        assert!(parse_size_string("   ").is_err());
        assert!(parse_size_string("").is_err());
    }

    #[test]
    fn test_parse_optional_size_aborts_on_bad_input() {
        // A typo must be fatal, not a silent fallback to the default.
        assert!(parse_optional_size("--max-file-size", Some(&"10Q".to_string())).is_err());
        assert_eq!(
            parse_optional_size("--max-file-size", Some(&"10M".to_string())).unwrap(),
            Some(10 * 1024 * 1024)
        );
        assert_eq!(parse_optional_size("--max-file-size", None).unwrap(), None);
    }

    #[test]
    fn live_photo_map_is_keyed_per_directory() {
        // Same stem, two different album folders: both must survive.
        let pairs = vec![
            pair_with("/t/Photos from 2015/IMG_0001.jpg", Some("1420070400")),
            pair_with("/t/Photos from 2022/IMG_0001.JPG", Some("1640995200")),
        ];
        let map = build_live_photo_dates_map(&pairs);
        assert_eq!(map.len(), 2);
        assert_eq!(
            map[&live_photo_key(Path::new("/t/Photos from 2015/IMG_0001.mp4"))].timestamp(),
            1420070400
        );
        assert_eq!(
            map[&live_photo_key(Path::new("/t/Photos from 2022/IMG_0001.MOV"))].timestamp(),
            1640995200
        );
    }

    #[test]
    fn live_photo_map_only_takes_sidecar_dates() {
        // A still with no sidecar contributes nothing, preventing mtime
        // poisoning.
        let pairs = vec![pair_with("/t/a/IMG_0002.heic", None)];
        assert!(build_live_photo_dates_map(&pairs).is_empty());
    }

    #[test]
    fn video_prefers_its_own_sidecar_over_the_live_photo_map() {
        let mut map: LivePhotoDates = HashMap::new();
        map.insert(live_photo_key(Path::new("/t/a/IMG_0003.mp4")), {
            organizer::parse_unix_timestamp("1420070400").unwrap()
        });
        let video = pair_with("/t/a/IMG_0003.mp4", Some("1640995200"));
        let (_, date) = organizer::extract_photo_date(video, &map).unwrap();
        assert_eq!(date.known().unwrap().timestamp(), 1640995200);
    }

    #[test]
    fn video_without_sidecar_falls_back_to_the_live_photo_map() {
        let mut map: LivePhotoDates = HashMap::new();
        map.insert(
            live_photo_key(Path::new("/t/a/IMG_0004.mp4")),
            organizer::parse_unix_timestamp("1420070400").unwrap(),
        );
        let video = pair_with("/t/a/IMG_0004.mp4", None);
        let (_, date) = organizer::extract_photo_date(video, &map).unwrap();
        assert_eq!(date.known().unwrap().timestamp(), 1420070400);
    }

    #[test]
    fn exit_codes_follow_the_outcome() {
        let s = ProcessingStats::default();
        assert_eq!(RunOutcome::Success(s.clone()).exit_code(), 0);
        assert_eq!(RunOutcome::CompletedWithErrors(s.clone()).exit_code(), 1);
        assert_eq!(RunOutcome::Interrupted(s).exit_code(), 130);
    }

    #[test]
    fn report_rows_are_written_and_escaped() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_report(dir.path(), &[]).unwrap().is_none());

        let rows = vec![ReportRow {
            phase: "organize",
            source: "/tmp/a,b.jpg".to_string(),
            destination: "/out/2020/01/a.jpg".to_string(),
            detail: "=cmd".to_string(),
        }];
        let path = write_report(dir.path(), &rows).unwrap().unwrap();
        assert_eq!(path.file_name().unwrap(), REPORT_FILE_NAME);
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("phase,source,destination,detail\n"));
        assert!(body.contains("\"/tmp/a,b.jpg\""));
        assert!(body.contains("'=cmd"));
    }

    #[test]
    fn no_archives_message_names_extensions_and_path() {
        let msg = no_archives_message(Path::new("/some/where"), false);
        assert!(msg.contains("/some/where"));
        assert!(msg.contains(".zip"));
        assert!(msg.contains(".tgz"));
        assert!(msg.contains(".tar.gz"));
        assert!(msg.contains("--recursive"));
        assert!(!no_archives_message(Path::new("/x"), true).contains("--recursive"));
    }

    #[test]
    fn overall_progress_names_every_pipeline_stage() {
        assert!(OVERALL_PROGRESS_TEMPLATE.contains("{msg}"));
        assert!(!OVERALL_PROGRESS_TEMPLATE.contains("{bar"));
        assert!(!OVERALL_PROGRESS_TEMPLATE.contains("{wide_bar"));
        assert!(!OVERALL_PROGRESS_TEMPLATE.contains("{eta}"));
        assert!(!OVERALL_PROGRESS_TEMPLATE.contains("{percent"));
        assert!(VERIFY_PROGRESS_TEMPLATE.contains("Verify {pos}/{len}"));
        assert!(VERIFY_PROGRESS_TEMPLATE.contains("{percent"));
        assert!(!VERIFY_PROGRESS_TEMPLATE.contains("{bar"));
        assert!(!VERIFY_PROGRESS_TEMPLATE.contains("{wide_bar"));
        assert_eq!(pipeline_stages(true).len() - SETUP_STAGE_COUNT, 8);
        assert_eq!(pipeline_stages(false).len() - SETUP_STAGE_COUNT, 7);

        let progress = ProgressBar::hidden();
        progress.set_length(8);
        set_processing_stage(&progress, "Extracting archives");
        assert_eq!(
            progress.message(),
            "Step 1 of 8: Extracting archives".to_string()
        );
        progress.inc(1);
        set_processing_stage(&progress, "Discovering media files");
        assert_eq!(
            progress.message(),
            "Step 2 of 8: Discovering media files".to_string()
        );
        assert_eq!(
            pipeline_stages(true),
            vec![
                "Preparing run",
                "Discovering archives",
                "Extracting archives",
                "Discovering media files",
                "Pairing media with sidecars",
                "Indexing Live Photos",
                "Writing EXIF metadata",
                "Organizing files",
                "Verifying organized files",
                "Finalizing output",
            ]
        );

        let without_verify = pipeline_stages(false);
        assert!(!without_verify.contains(&"Verifying organized files"));
        assert_eq!(without_verify.last(), Some(&"Finalizing output"));
    }
}
