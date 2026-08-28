// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! EXIF metadata writing.
//!
//! Google Takeout ships the authoritative capture date, GPS position and
//! description in JSON sidecars. This module writes that information back into
//! the media files themselves (via [`little_exif`]) and, as a last resort,
//! into the filesystem modification time.
//!
//! Two invariants matter here:
//!
//! * **EXIF is written first, mtime last.** `little_exif` rewrites the whole
//!   file, which resets the mtime to "now"; setting the mtime before the EXIF
//!   write silently loses it.
//! * **Writes are atomic.** A failed or truncated `little_exif` write must
//!   never destroy the original, so we write to a sibling temp copy, verify it
//!   still looks like the format it claims to be, and only then rename it over
//!   the original. The QuickTime `mvhd` patcher below follows the
//!   same discipline.

use crate::metadata::{MediaMetadataPair, PhotoMetadata};
use chrono::Offset;
use chrono_tz::Tz;
use indicatif::{ProgressBar, ProgressStyle};
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use little_exif::rational::uR64;
use log::{debug, error, info, warn};
use rayon::prelude::*;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Offset written alongside the date tags when no timezone could be resolved.
/// We emit UTC in that case, so state that explicitly rather than leaving the
/// reference frame ambiguous.
const UTC_OFFSET: &str = "+00:00";

/// Extensions that `little_exif` 0.6.23 can write EXIF into.
const EXIF_WRITABLE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "heic", "heif", "hif", "avif", "jxl", "tiff", "tif", "webp",
];

/// Container formats whose capture date lives in a QuickTime `mvhd` header
/// rather than in EXIF.
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v"];

/// Seconds between the QuickTime epoch (1904-01-01 UTC) and the Unix epoch.
const QUICKTIME_EPOCH_OFFSET: i64 = 2_082_844_800;

/// Counter used to make temp filenames unique across threads.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `tzf-rs`' finder embeds the whole timezone boundary database and takes a
/// noticeable fraction of a second plus several MB to construct, so build it at
/// most once per process. The EXIF batch runs under rayon, hence the `Sync`
/// requirement that `DefaultFinder` satisfies. `LazyLock` also keeps the cost
/// off runs that never see a geotagged file.
static TZ_FINDER: LazyLock<tzf_rs::DefaultFinder> = LazyLock::new(tzf_rs::DefaultFinder::new);

// Define a simple error type for this module
#[derive(Debug, thiserror::Error)]
pub enum ExifError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("EXIF write error: {0}")]
    ExifWrite(String),
    #[error("invalid timestamp")]
    InvalidTimestamp,
}

/// What happened to a single file during the EXIF phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExifWriteOutcome {
    /// EXIF tags were embedded in the file (and the mtime was set).
    ExifWritten,
    /// A QuickTime/MP4 `mvhd` header was patched with the capture date (and the
    /// mtime was set). Tracked apart from [`Self::ExifWritten`] because it is a
    /// different container field, and apart from [`Self::MtimeOnly`] because the
    /// date really did make it inside the file.
    VideoDateWritten,
    /// The format cannot carry a date (or had nothing to write); only the
    /// filesystem modification time was corrected.
    MtimeOnly,
}

/// Honest per-run accounting for the EXIF phase.
#[derive(Debug, Default)]
pub struct ExifBatchSummary {
    /// Files that received embedded EXIF metadata.
    pub exif_written: usize,
    /// Successful EXIF writes that started with a fresh block because the
    /// file's pre-existing EXIF could not be parsed. Sidecar-derived tags were
    /// written, but unknown tags from the unreadable block may not survive.
    pub fresh_exif_blocks: usize,
    /// Videos whose QuickTime `mvhd` creation/modification times were patched.
    /// These also had their filesystem mtime corrected.
    pub video_dates_written: usize,
    /// Files that could only receive a corrected modification time.
    pub mtime_only: usize,
    /// Files that had no sidecar metadata at all.
    pub skipped_no_metadata: usize,
    /// Files that failed, with the reason. The originals are left untouched.
    pub failures: Vec<(PathBuf, String)>,
    /// Files not visited because a shutdown was requested part-way through.
    pub not_processed: usize,
}

impl ExifBatchSummary {
    /// Total number of files the batch actually accounted for.
    pub fn total_processed(&self) -> usize {
        self.exif_written
            + self.video_dates_written
            + self.mtime_only
            + self.skipped_no_metadata
            + self.failures.len()
    }
}

/// Per-item result, aggregated into [`ExifBatchSummary`] after the parallel pass.
enum ItemOutcome {
    ExifWritten { fresh_block: bool },
    VideoDateWritten,
    MtimeOnly,
    SkippedNoMetadata,
    Failed(PathBuf, String),
    NotProcessed,
}

/// Write EXIF metadata (and corrected modification times) for a batch of files.
///
/// Takes the pairs **by reference**. This used to take the `Vec` by value and
/// then clone it again internally, holding the set in triplicate.
pub fn write_exif_metadata_batch(
    media_metadata_pairs: &[MediaMetadataPair],
) -> Result<ExifBatchSummary, Box<dyn std::error::Error>> {
    write_exif_metadata_batch_with_tz(media_metadata_pairs, None)
}

/// As [`write_exif_metadata_batch`], but rendering the date tags in a timezone.
///
/// `tz_override` forces one zone for every file (the `--timezone` flag); with
/// `None` each file's zone is derived from its own GPS coordinates, falling back
/// to UTC when it has none. See [`resolve_timezone`].
pub fn write_exif_metadata_batch_with_tz(
    media_metadata_pairs: &[MediaMetadataPair],
    tz_override: Option<Tz>,
) -> Result<ExifBatchSummary, Box<dyn std::error::Error>> {
    info!(
        "Starting EXIF metadata writing phase for {} files",
        media_metadata_pairs.len()
    );

    // Create a progress bar for EXIF writing
    let exif_pb = crate::progress::add(ProgressBar::new(media_metadata_pairs.len() as u64));
    exif_pb.set_style(
        ProgressStyle::default_bar()
            .template("  {spinner:.green} {percent:>3}% EXIF {pos}/{len}")?
            .progress_chars("#>-"),
    );

    // Process each media file in parallel, borrowing the slice.
    let outcomes: Vec<ItemOutcome> = media_metadata_pairs
        .par_iter()
        .map(|pair| {
            // Sample the shutdown flag so Ctrl+C stops a multi-hour run early.
            if crate::is_shutdown() {
                return ItemOutcome::NotProcessed;
            }

            let outcome = match pair {
                MediaMetadataPair::WithMetadata(path, metadata, _) => {
                    let tz = resolve_timezone(metadata, tz_override);
                    match write_metadata_to_file_detailed(path, metadata, tz) {
                        Ok(result) => match result.outcome {
                            ExifWriteOutcome::ExifWritten => ItemOutcome::ExifWritten {
                                fresh_block: result.fresh_exif_block,
                            },
                            ExifWriteOutcome::VideoDateWritten => ItemOutcome::VideoDateWritten,
                            ExifWriteOutcome::MtimeOnly => ItemOutcome::MtimeOnly,
                        },
                        Err(e) => {
                            error!(
                                "Failed to write EXIF metadata for {}: {}",
                                path.display(),
                                e
                            );
                            ItemOutcome::Failed(path.clone(), e.to_string())
                        }
                    }
                }
                MediaMetadataPair::WithoutMetadata(path) => {
                    debug!("No metadata to write for {}", path.display());
                    ItemOutcome::SkippedNoMetadata
                }
            };

            exif_pb.inc(1);
            outcome
        })
        .collect();

    let mut summary = ExifBatchSummary::default();
    for outcome in outcomes {
        match outcome {
            ItemOutcome::ExifWritten { fresh_block } => {
                summary.exif_written += 1;
                summary.fresh_exif_blocks += usize::from(fresh_block);
            }
            ItemOutcome::VideoDateWritten => summary.video_dates_written += 1,
            ItemOutcome::MtimeOnly => summary.mtime_only += 1,
            ItemOutcome::SkippedNoMetadata => summary.skipped_no_metadata += 1,
            ItemOutcome::Failed(path, reason) => summary.failures.push((path, reason)),
            ItemOutcome::NotProcessed => summary.not_processed += 1,
        }
    }

    if summary.not_processed > 0 {
        warn!(
            "Shutdown requested: {} files were not processed in the EXIF phase",
            summary.not_processed
        );
        exif_pb.finish_and_clear();
        return Ok(summary);
    }

    exif_pb.finish_and_clear();

    info!(
        "EXIF phase: {} written ({} fresh blocks after unreadable existing EXIF), {} video dates written, {} mtime-only, {} without metadata, {} failed",
        summary.exif_written,
        summary.fresh_exif_blocks,
        summary.video_dates_written,
        summary.mtime_only,
        summary.skipped_no_metadata,
        summary.failures.len()
    );

    Ok(summary)
}

/// Failures reading pre-existing EXIF are expected for files that simply have
/// none. `little_exif` reports everything as `std::io::Error`, so classify by
/// kind first and only fall back to message matching for the `Other` bucket it
/// uses for "there was nothing to read".
fn is_critical_exif_error(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;

    match error.kind() {
        // "Unsupported file type", "not yet implemented for ...". We already
        // gate on the extension, so this is informational at most.
        ErrorKind::Unsupported => false,
        // A file with no EXIF block at all decodes to nothing; that is the
        // normal case for Takeout output, not an error worth shouting about.
        ErrorKind::InvalidData | ErrorKind::UnexpectedEof => false,
        ErrorKind::Other => {
            let error_str = error.to_string().to_lowercase();
            !(error_str.contains("no exif")
                || error_str.contains("no metadata")
                || (error_str.contains("unknown tag for combination undef vs string")
                    && error_str.contains("exifversion")))
        }
        _ => true,
    }
}

/// Write all available metadata for a single media file, dating it in UTC.
///
/// Order matters: EXIF first (it rewrites the file and resets the mtime),
/// filesystem modification time **last**.
pub fn write_metadata_to_file(
    media_path: &Path,
    metadata: &PhotoMetadata,
) -> Result<ExifWriteOutcome, ExifError> {
    write_metadata_to_file_with_tz(media_path, metadata, None)
}

/// As [`write_metadata_to_file`], but rendering the EXIF date tags in `tz`.
///
/// `tz` only affects the EXIF date tags. The filesystem mtime and the QuickTime
/// `mvhd` fields are absolute instants by definition and stay in UTC.
pub fn write_metadata_to_file_with_tz(
    media_path: &Path,
    metadata: &PhotoMetadata,
    tz: Option<Tz>,
) -> Result<ExifWriteOutcome, ExifError> {
    write_metadata_to_file_detailed(media_path, metadata, tz).map(|result| result.outcome)
}

/// Internal result that keeps successful metadata writes compatible with the
/// public outcome while recording whether unreadable pre-existing EXIF had to
/// be replaced with a fresh block.
struct DetailedWriteOutcome {
    outcome: ExifWriteOutcome,
    fresh_exif_block: bool,
}

fn write_metadata_to_file_detailed(
    media_path: &Path,
    metadata: &PhotoMetadata,
    tz: Option<Tz>,
) -> Result<DetailedWriteOutcome, ExifError> {
    // Check if the file exists
    if !media_path.exists() {
        return Err(ExifError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "File not found",
        )));
    }

    let mut outcome = ExifWriteOutcome::MtimeOnly;
    let mut fresh_exif_block = false;
    let mut exif_error: Option<ExifError> = None;

    if is_supported_format(media_path) {
        match build_exif_metadata(media_path, metadata, tz) {
            Some(built) => match write_exif_atomically(media_path, &built.metadata) {
                Ok(()) => {
                    debug!("Successfully wrote EXIF data for {}", media_path.display());
                    outcome = ExifWriteOutcome::ExifWritten;
                    if let Some(load_error) = built.unreadable_existing_exif {
                        fresh_exif_block = true;
                        warn!(
                            "Embedded sidecar-derived EXIF in {} using a fresh EXIF block because its existing EXIF could not be read: {}. Existing unparsed tags may not have been preserved",
                            media_path.display(),
                            load_error
                        );
                    }
                }
                Err(e) => exif_error = Some(e),
            },
            None => {
                debug!(
                    "No EXIF-worthy metadata for {}, mtime only",
                    media_path.display()
                );
            }
        }
    } else if is_video_format(media_path) {
        // The QuickTime spec defines mvhd's creation/modification times as UTC,
        // so the display timezone does not apply here.
        if let Some(timestamp) = photo_taken_unix(metadata) {
            match write_video_date(media_path, timestamp) {
                Ok(()) => outcome = ExifWriteOutcome::VideoDateWritten,
                // A video with no moov/mvhd is ordinary, not a failure: it still
                // gets a corrected mtime below, which is what it would have got
                // before this feature existed.
                Err(e) => debug!(
                    "Could not patch video date for {}, falling back to mtime only: {}",
                    media_path.display(),
                    e
                ),
            }
        }
    }

    // Set the modification time last because anything that rewrites the file above
    // would otherwise reset it to "now".
    set_file_modification_time(media_path, metadata)?;

    match exif_error {
        Some(e) => Err(e),
        None => Ok(DetailedWriteOutcome {
            outcome,
            fresh_exif_block,
        }),
    }
}

/// The sidecar's `photoTakenTime` as a Unix timestamp, if it parses.
fn photo_taken_unix(metadata: &PhotoMetadata) -> Option<i64> {
    metadata
        .photo_taken_time
        .as_ref()
        .and_then(|t| t.timestamp.as_ref())
        .and_then(|ts| ts.parse::<i64>().ok())
}

/// Build the in-memory EXIF metadata for a file, merging over whatever the file
/// already carries. Returns `None` when there is nothing worth writing.
struct BuiltExifMetadata {
    metadata: Metadata,
    unreadable_existing_exif: Option<String>,
}

fn build_exif_metadata(
    media_path: &Path,
    metadata: &PhotoMetadata,
    tz: Option<Tz>,
) -> Option<BuiltExifMetadata> {
    let datetime = metadata
        .photo_taken_time
        .as_ref()
        .and_then(|t| t.timestamp.as_ref())
        .and_then(|ts| unix_to_exif_datetime_tz(ts, tz).ok());

    let description = metadata
        .description
        .as_ref()
        .map(|d| d.trim())
        .filter(|d| !d.is_empty());

    let coords = gps_coordinates(metadata);

    if datetime.is_none() && description.is_none() && coords.is_none() {
        return None;
    }

    // Start from the file's existing EXIF so unrelated tags survive.
    let (mut exif_metadata, unreadable_existing_exif) = match Metadata::new_from_path(media_path) {
        Ok(existing) => (existing, None),
        Err(e) => {
            if is_critical_exif_error(&e) {
                // Do not warn yet. The atomic write below may fail, in which
                // case the original is untouched and this is only context for
                // the real write error. A preservation warning is emitted only
                // after fresh sidecar-derived EXIF was successfully embedded.
                (Metadata::new(), Some(e.to_string()))
            } else {
                debug!(
                    "No pre-existing EXIF metadata for {}: {}",
                    media_path.display(),
                    e
                );
                (Metadata::new(), None)
            }
        }
    };

    if let Some((datetime, offset)) = datetime {
        // DateTimeOriginal (0x9003), CreateDate/DateTimeDigitized (0x9004) and
        // ModifyDate (0x0132). #27 previously only wrote the first and last.
        exif_metadata.set_tag(ExifTag::DateTimeOriginal(datetime.clone()));
        exif_metadata.set_tag(ExifTag::CreateDate(datetime.clone()));
        exif_metadata.set_tag(ExifTag::ModifyDate(datetime));

        // The date tags above have no timezone of their own, so record the frame
        // they are expressed in instead of leaving the reader guessing (§10.5).
        exif_metadata.set_tag(ExifTag::OffsetTimeOriginal(offset.clone()));
        exif_metadata.set_tag(ExifTag::OffsetTimeDigitized(offset));
    }

    if let Some(description) = description {
        exif_metadata.set_tag(ExifTag::ImageDescription(description.to_string()));
    }

    if let Some((latitude, longitude, altitude)) = coords {
        exif_metadata.set_tag(ExifTag::GPSLatitudeRef(
            if latitude >= 0.0 { "N" } else { "S" }.to_string(),
        ));
        exif_metadata.set_tag(ExifTag::GPSLatitude(degrees_to_dms(latitude)));
        exif_metadata.set_tag(ExifTag::GPSLongitudeRef(
            if longitude >= 0.0 { "E" } else { "W" }.to_string(),
        ));
        exif_metadata.set_tag(ExifTag::GPSLongitude(degrees_to_dms(longitude)));

        if let Some(altitude) = altitude {
            // 0 = above sea level, 1 = below.
            exif_metadata.set_tag(ExifTag::GPSAltitudeRef(vec![u8::from(altitude < 0.0)]));
            exif_metadata.set_tag(ExifTag::GPSAltitude(vec![altitude_rational(altitude)]));
        }
    }

    Some(BuiltExifMetadata {
        metadata: exif_metadata,
        unreadable_existing_exif,
    })
}

/// Extract usable GPS coordinates, preferring `geoDataExif` over `geoData`.
///
/// Google writes `0.0 / 0.0` to mean "no location", so those are rejected.
fn gps_coordinates(metadata: &PhotoMetadata) -> Option<(f64, f64, Option<f64>)> {
    let from_exif = metadata
        .geo_data_exif
        .as_ref()
        .and_then(|g| Some((g.latitude?, g.longitude?, g.altitude)));

    let from_plain = metadata
        .geo_data
        .as_ref()
        .and_then(|g| Some((g.latitude?, g.longitude?, g.altitude)));

    [from_exif, from_plain]
        .into_iter()
        .flatten()
        .find(|(lat, lon, _)| is_usable_coordinate(*lat, *lon))
}

/// `0/0` is Google's "unknown location" sentinel; anything out of range is junk.
fn is_usable_coordinate(latitude: f64, longitude: f64) -> bool {
    if !latitude.is_finite() || !longitude.is_finite() {
        return false;
    }
    if latitude == 0.0 && longitude == 0.0 {
        return false;
    }
    (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude)
}

/// Convert signed decimal degrees into the degrees/minutes/seconds rational
/// triple that the EXIF GPS tags require. The sign is carried by the `*Ref` tag,
/// so only the magnitude is encoded here.
fn degrees_to_dms(degrees: f64) -> Vec<uR64> {
    const SEC_SCALE: u64 = 10_000;

    // Work in ten-thousandths of an arcsecond so rounding cannot produce a
    // "60 seconds" component.
    let total = (degrees.abs() * 3600.0 * SEC_SCALE as f64).round() as u64;

    let deg = total / (3600 * SEC_SCALE);
    let rem = total % (3600 * SEC_SCALE);
    let min = rem / (60 * SEC_SCALE);
    let sec_scaled = rem % (60 * SEC_SCALE);

    vec![
        uR64 {
            nominator: deg as u32,
            denominator: 1,
        },
        uR64 {
            nominator: min as u32,
            denominator: 1,
        },
        uR64 {
            nominator: sec_scaled as u32,
            denominator: SEC_SCALE as u32,
        },
    ]
}

/// GPSAltitude is an unsigned rational in metres; the sign lives in GPSAltitudeRef.
fn altitude_rational(altitude: f64) -> uR64 {
    let scaled = (altitude.abs() * 1000.0).round();
    if !scaled.is_finite() || scaled > u32::MAX as f64 {
        return uR64 {
            nominator: 0,
            denominator: 1,
        };
    }
    uR64 {
        nominator: scaled as u32,
        denominator: 1000,
    }
}

/// Write EXIF into a sibling temp copy, verify it, then rename over the original.
///
/// On any failure the original file is left byte-for-byte untouched.
fn write_exif_atomically(media_path: &Path, exif_metadata: &Metadata) -> Result<(), ExifError> {
    let temp_path = temp_sibling_path(media_path)?;

    // Copy preserves permissions, so the renamed file keeps the original mode.
    if let Err(e) = fs::copy(media_path, &temp_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(ExifError::Io(e));
    }

    let result = exif_metadata
        .write_to_file(&temp_path)
        .map_err(|e| ExifError::ExifWrite(e.to_string()))
        .and_then(|()| verify_media_file(&temp_path));

    match result {
        Ok(()) => match fs::rename(&temp_path, media_path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&temp_path);
                Err(ExifError::Io(e))
            }
        },
        Err(e) => {
            // Original untouched; drop the damaged candidate.
            let _ = fs::remove_file(&temp_path);
            Err(e)
        }
    }
}

/// A unique hidden sibling path in the same directory (so `rename` stays atomic).
///
/// The temp name **must keep the original extension**: `little_exif` picks its
/// codec from `Path::extension()`, so `photo.jpg.tmp7` is rejected as an
/// "unknown file type".
fn temp_sibling_path(media_path: &Path) -> Result<PathBuf, ExifError> {
    let parent = media_path.parent().ok_or_else(|| {
        ExifError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "media path has no parent directory",
        ))
    })?;

    let stem = media_path
        .file_stem()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "media".to_string());
    let extension = media_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();

    let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        ".{}.exiftmp.{}.{}.{}",
        stem,
        std::process::id(),
        unique,
        extension
    );
    Ok(parent.join(name))
}

/// Cheap sanity check that a written file is still a readable image: non-zero
/// size plus the magic bytes the extension implies.
fn verify_media_file(path: &Path) -> Result<(), ExifError> {
    let len = fs::metadata(path).map_err(ExifError::Io)?.len();
    if len == 0 {
        return Err(ExifError::ExifWrite(
            "EXIF write produced an empty file".to_string(),
        ));
    }

    let header = read_header(path, 16)?;
    if !header_matches_extension(path, &header) {
        return Err(ExifError::ExifWrite(format!(
            "EXIF write produced a file whose header no longer matches its type ({} bytes)",
            len
        )));
    }

    Ok(())
}

fn read_header(path: &Path, len: usize) -> Result<Vec<u8>, ExifError> {
    let mut file = fs::File::open(path).map_err(ExifError::Io)?;
    let mut buffer = vec![0u8; len];
    let read = file.read(&mut buffer).map_err(ExifError::Io)?;
    buffer.truncate(read);
    Ok(buffer)
}

/// Does `header` look like the format `path`'s extension claims?
fn header_matches_extension(path: &Path, header: &[u8]) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "jpg" | "jpeg" => header.starts_with(&[0xFF, 0xD8]),
        "png" => header.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
        "tiff" | "tif" => {
            header.starts_with(&[0x49, 0x49, 0x2A, 0x00])
                || header.starts_with(&[0x4D, 0x4D, 0x00, 0x2A])
        }
        "webp" => header.len() >= 12 && header.starts_with(b"RIFF") && &header[8..12] == b"WEBP",
        // ISO-BMFF family: `....ftyp` at offset 4.
        "heic" | "heif" | "hif" | "avif" => header.len() >= 12 && &header[4..8] == b"ftyp",
        "jxl" => {
            header.starts_with(&[0xFF, 0x0A])
                || header.starts_with(&[0x00, 0x00, 0x00, 0x0C, 0x4A, 0x58, 0x4C, 0x20])
        }
        // Unknown extension: we never claim to have verified it.
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// QuickTime / MP4 `mvhd` date patching
// ---------------------------------------------------------------------------

/// A parsed ISO-BMFF / QuickTime atom header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AtomHeader {
    /// Offset of the atom's first byte, i.e. of its size field.
    start: u64,
    /// Bytes of header before the payload: 8 normally, 16 for a 64-bit size.
    header_len: u64,
    /// Total size of the atom, header included.
    total_len: u64,
    /// The four-character type code.
    kind: [u8; 4],
}

impl AtomHeader {
    /// Offset one past the atom's last byte.
    fn end(&self) -> u64 {
        self.start + self.total_len
    }

    /// Offset of the atom's first payload byte.
    fn body(&self) -> u64 {
        self.start + self.header_len
    }
}

/// Read one atom header at `offset`, refusing anything that does not fit inside
/// `limit`.
///
/// Returning `Ok(None)` means "no valid atom here": a truncated header, a size
/// smaller than the header it describes, or a size that runs past the end of the
/// enclosing container. Treating those as absence rather than as an error is
/// what keeps the walk from wandering into media payload.
fn read_atom_header(
    file: &mut fs::File,
    offset: u64,
    limit: u64,
) -> Result<Option<AtomHeader>, ExifError> {
    if offset.saturating_add(8) > limit {
        return Ok(None);
    }

    file.seek(SeekFrom::Start(offset))?;
    let mut buf = [0u8; 8];
    if file.read_exact(&mut buf).is_err() {
        return Ok(None);
    }

    let mut kind = [0u8; 4];
    kind.copy_from_slice(&buf[4..8]);

    let (header_len, total_len) = match u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) {
        // 0 means "this atom runs to the end of its container".
        0 => (8u64, limit - offset),
        // 1 means the real size is the 64-bit value following the type code.
        1 => {
            if offset.saturating_add(16) > limit {
                return Ok(None);
            }
            let mut large = [0u8; 8];
            if file.read_exact(&mut large).is_err() {
                return Ok(None);
            }
            (16u64, u64::from_be_bytes(large))
        }
        size => (8u64, u64::from(size)),
    };

    if total_len < header_len || offset.saturating_add(total_len) > limit {
        return Ok(None);
    }

    Ok(Some(AtomHeader {
        start: offset,
        header_len,
        total_len,
        kind,
    }))
}

/// Locate the `mvhd` atom as a direct child of the top-level `moov` atom.
///
/// Walking the structure avoids mistaking `mvhd` bytes in an `mdat` payload for
/// an atom header. Patching such a match would corrupt the file. Since
/// `total_len >= header_len >= 8` is enforced above, every iteration advances and
/// the walk terminates.
fn find_mvhd(file: &mut fs::File, file_len: u64) -> Result<Option<AtomHeader>, ExifError> {
    let mut offset = 0u64;

    while let Some(atom) = read_atom_header(file, offset, file_len)? {
        if &atom.kind == b"moov" {
            let mut child_offset = atom.body();
            while let Some(child) = read_atom_header(file, child_offset, atom.end())? {
                if &child.kind == b"mvhd" {
                    return Ok(Some(child));
                }
                child_offset = child.end();
            }
            // A `moov` without an `mvhd` is malformed, and a file only ever has
            // one `moov`, so there is nothing further to look at.
            return Ok(None);
        }
        offset = atom.end();
    }

    Ok(None)
}

/// Where `mvhd` keeps its creation and modification times, and how wide they are.
///
/// Both fields follow the 1-byte version and 3 flag bytes; version 0 stores them
/// as 32-bit seconds, version 1 as 64-bit.
fn mvhd_time_fields(mvhd: &AtomHeader, version: u8) -> Result<(u64, u64, usize), ExifError> {
    let body = mvhd.body();
    let (creation, modification, width) = match version {
        0 => (body + 4, body + 8, 4usize),
        1 => (body + 4, body + 12, 8usize),
        other => {
            return Err(ExifError::ExifWrite(format!(
                "unsupported mvhd version {other}"
            )));
        }
    };

    if modification + width as u64 > mvhd.end() {
        return Err(ExifError::ExifWrite(
            "mvhd atom is too short for its version".to_string(),
        ));
    }

    Ok((creation, modification, width))
}

/// Read the `mvhd` version byte.
fn read_mvhd_version(file: &mut fs::File, mvhd: &AtomHeader) -> Result<u8, ExifError> {
    if mvhd.body() >= mvhd.end() {
        return Err(ExifError::ExifWrite("mvhd atom has no body".to_string()));
    }
    file.seek(SeekFrom::Start(mvhd.body()))?;
    let mut version = [0u8; 1];
    file.read_exact(&mut version)?;
    Ok(version[0])
}

/// Stamp a capture date into an MP4/MOV/M4V file's `moov/mvhd` header.
///
/// The two `mvhd` time fields are seconds since 1904-01-01 **UTC** by
/// specification, so no timezone is applied to them.
///
/// The write follows the same discipline as the EXIF path: the original is only
/// replaced by an `fs::rename` of a fully patched and re-verified sibling copy,
/// so a failure at any point leaves it byte-for-byte intact.
///
/// Returns an error when the file has no `moov/mvhd`, when the atom tree is
/// malformed, or when the date cannot be represented. Callers are
/// expected to treat as "fall back to setting the mtime", not as data loss.
pub fn write_video_date(path: &Path, unix_timestamp: i64) -> Result<(), ExifError> {
    // QuickTime counts from 1904-01-01 and cannot express anything earlier.
    let qt_timestamp = unix_timestamp
        .checked_add(QUICKTIME_EPOCH_OFFSET)
        .filter(|seconds| *seconds >= 0)
        .ok_or(ExifError::InvalidTimestamp)?;

    let original_len = fs::metadata(path)?.len();

    // Parse the *original* first. Videos without a usable `moov/mvhd` are common
    // enough that copying gigabytes only to discover it would be pure waste.
    let (mvhd, creation_offset, modification_offset, width) = {
        let mut file = fs::File::open(path)?;
        let Some(mvhd) = find_mvhd(&mut file, original_len)? else {
            return Err(ExifError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no moov/mvhd atom",
            )));
        };
        let version = read_mvhd_version(&mut file, &mvhd)?;
        let (creation, modification, width) = mvhd_time_fields(&mvhd, version)?;
        (mvhd, creation, modification, width)
    };

    // Encode before copying so an unrepresentable date costs nothing. A version-0
    // header runs out of room in 2040; refuse rather than wrap silently to 1904.
    let encoded: Vec<u8> = if width == 4 {
        u32::try_from(qt_timestamp)
            .map_err(|_| ExifError::InvalidTimestamp)?
            .to_be_bytes()
            .to_vec()
    } else {
        (qt_timestamp as u64).to_be_bytes().to_vec()
    };

    let temp_path = temp_sibling_path(path)?;
    if let Err(e) = fs::copy(path, &temp_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(ExifError::Io(e));
    }

    let result = patch_mvhd_times(&temp_path, creation_offset, modification_offset, &encoded)
        .and_then(|()| {
            verify_patched_video(
                &temp_path,
                original_len,
                &mvhd,
                creation_offset,
                modification_offset,
                &encoded,
            )
        });

    match result {
        Ok(()) => match fs::rename(&temp_path, path) {
            Ok(()) => {
                debug!(
                    "Patched mvhd creation/modification time in {}",
                    path.display()
                );
                Ok(())
            }
            Err(e) => {
                let _ = fs::remove_file(&temp_path);
                Err(ExifError::Io(e))
            }
        },
        Err(e) => {
            // Original untouched; drop the damaged candidate.
            let _ = fs::remove_file(&temp_path);
            Err(e)
        }
    }
}

/// Overwrite the two `mvhd` time fields in place. Only those bytes change, so
/// the file's length and every other atom are unaffected.
fn patch_mvhd_times(
    temp_path: &Path,
    creation_offset: u64,
    modification_offset: u64,
    encoded: &[u8],
) -> Result<(), ExifError> {
    let mut file = fs::OpenOptions::new().write(true).open(temp_path)?;

    file.seek(SeekFrom::Start(creation_offset))?;
    file.write_all(encoded)?;
    file.seek(SeekFrom::Start(modification_offset))?;
    file.write_all(encoded)?;
    file.sync_all()?;

    Ok(())
}

/// Re-parse the patched copy before it is allowed to replace the original: the
/// length must be unchanged, the atom tree must still resolve to the very same
/// `mvhd`, and both fields must read back as exactly what we wrote.
fn verify_patched_video(
    temp_path: &Path,
    original_len: u64,
    expected_mvhd: &AtomHeader,
    creation_offset: u64,
    modification_offset: u64,
    encoded: &[u8],
) -> Result<(), ExifError> {
    let patched_len = fs::metadata(temp_path)?.len();
    if patched_len != original_len {
        return Err(ExifError::ExifWrite(format!(
            "mvhd patch changed the file size ({original_len} -> {patched_len} bytes)"
        )));
    }

    let mut file = fs::File::open(temp_path)?;

    match find_mvhd(&mut file, patched_len)? {
        Some(found) if found == *expected_mvhd => {}
        _ => {
            return Err(ExifError::ExifWrite(
                "mvhd atom no longer parses after the patch".to_string(),
            ));
        }
    }

    for offset in [creation_offset, modification_offset] {
        let mut actual = vec![0u8; encoded.len()];
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut actual)?;
        if actual != encoded {
            return Err(ExifError::ExifWrite(
                "mvhd timestamp did not read back as written".to_string(),
            ));
        }
    }

    Ok(())
}

/// Set file modification time based on photo_taken_time from metadata
pub fn set_file_modification_time(
    media_path: &Path,
    metadata: &PhotoMetadata,
) -> Result<(), ExifError> {
    // Check if the file exists
    if !media_path.exists() {
        return Err(ExifError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "File not found",
        )));
    }

    // Try to set file system modification time based on photo_taken_time
    if let Some(photo_taken_time) = &metadata.photo_taken_time
        && let Some(timestamp_str) = &photo_taken_time.timestamp
    {
        // Parse the timestamp as UTC DateTime
        let timestamp = timestamp_str
            .parse::<i64>()
            .map_err(|_| ExifError::InvalidTimestamp)?;

        // Convert to DateTime<Utc> first to make timezone handling explicit
        let datetime_utc = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
            .ok_or(ExifError::InvalidTimestamp)?;

        // Convert to SystemTime (this correctly represents the UTC time)
        let system_time = datetime_utc.into();

        // Set file modification time
        filetime::set_file_mtime(
            media_path,
            filetime::FileTime::from_system_time(system_time),
        )
        .map_err(ExifError::Io)?;

        debug!(
            "Successfully set modification time for {}",
            media_path.display()
        );
    }

    Ok(())
}

/// Check if exiftool is available on the system
pub fn check_exiftool_available() -> bool {
    // This function is kept for compatibility with tests, but it's not used in the main application
    // since we're using a pure-Rust implementation now
    false
}

/// Check if file format supports EXIF metadata writing.
///
/// This is the single source of truth: [`write_metadata_to_file`] gates on this
/// exact function, so the two cannot contradict each other.
pub fn is_supported_format(path: &Path) -> bool {
    if let Some(extension) = path.extension() {
        let ext_str = extension.to_string_lossy().to_lowercase();
        EXIF_WRITABLE_EXTENSIONS.contains(&ext_str.as_str())
    } else {
        false
    }
}

/// Check whether a file is one of the QuickTime/MP4 containers whose capture
/// date lives in an `mvhd` header.
pub fn is_video_format(path: &Path) -> bool {
    if let Some(extension) = path.extension() {
        let ext_str = extension.to_string_lossy().to_lowercase();
        VIDEO_EXTENSIONS.contains(&ext_str.as_str())
    } else {
        false
    }
}

/// Convert a Unix timestamp to EXIF datetime format, in UTC.
pub fn unix_to_exif_datetime(timestamp_str: &str) -> Result<String, ExifError> {
    unix_to_exif_datetime_tz(timestamp_str, None).map(|(datetime, _offset)| datetime)
}

/// Render a Unix timestamp as an EXIF date string plus the UTC offset it is
/// expressed in, as `("YYYY:MM:DD HH:MM:SS", "+HH:MM")`.
///
/// With `tz = None` the date is UTC and the offset is `+00:00`, byte-for-byte
/// the behaviour from before timezone support existed. With a zone, the date is
/// local wall-clock time there and the offset is that zone's real offset *for
/// that instant*, so DST is handled by construction rather than by a table of
/// standard offsets.
pub fn unix_to_exif_datetime_tz(
    timestamp_str: &str,
    tz: Option<Tz>,
) -> Result<(String, String), ExifError> {
    // Parse the timestamp string to i64
    let timestamp = timestamp_str
        .parse::<i64>()
        .map_err(|_| ExifError::InvalidTimestamp)?;

    // Convert to DateTime<Utc>
    let datetime_utc = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
        .ok_or(ExifError::InvalidTimestamp)?;

    // Format as EXIF datetime string (YYYY:MM:DD HH:MM:SS)
    match tz {
        None => Ok((
            datetime_utc.format("%Y:%m:%d %H:%M:%S").to_string(),
            UTC_OFFSET.to_string(),
        )),
        Some(tz) => {
            let local = datetime_utc.with_timezone(&tz);
            let offset_seconds = local.offset().fix().local_minus_utc();
            Ok((
                local.format("%Y:%m:%d %H:%M:%S").to_string(),
                format_utc_offset(offset_seconds),
            ))
        }
    }
}

/// Format a UTC offset in seconds as EXIF's `±HH:MM`.
///
/// EXIF 2.31's OffsetTime tags have no room for a seconds component, so the
/// handful of pre-standardisation zones that carry one are truncated to whole
/// minutes rather than emitted in a shape readers cannot parse.
fn format_utc_offset(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let total_minutes = offset_seconds.unsigned_abs() / 60;
    format!(
        "{}{:02}:{:02}",
        sign,
        total_minutes / 60,
        total_minutes % 60
    )
}

/// Resolve the timezone the EXIF date tags for a file should be expressed in.
///
/// `override_tz` (the `--timezone` flag) always wins. Otherwise the zone is
/// looked up offline from the file's own GPS coordinates, which go through the
/// same [`gps_coordinates`] validity gate the GPS tags use, so Google's `0/0`
/// "unknown location" sentinel and out-of-range junk resolve to `None` rather
/// than to Africa/Accra. `None` means "write UTC".
pub fn resolve_timezone(metadata: &PhotoMetadata, override_tz: Option<Tz>) -> Option<Tz> {
    if let Some(tz) = override_tz {
        return Some(tz);
    }

    let (latitude, longitude, _altitude) = gps_coordinates(metadata)?;

    // Note the argument order: tzf-rs takes (lng, lat).
    let name = TZ_FINDER.get_tz_name(longitude, latitude);
    if name.is_empty() {
        debug!("No timezone covers coordinates ({latitude}, {longitude})");
        return None;
    }

    match name.parse::<Tz>() {
        Ok(tz) => {
            debug!("Resolved timezone {name} for coordinates ({latitude}, {longitude})");
            Some(tz)
        }
        Err(_) => {
            // tzf-rs and chrono-tz ship independent copies of the IANA database,
            // so a zone added in one but not the other lands here.
            debug!("chrono-tz does not know the zone '{name}'; writing UTC instead");
            None
        }
    }
}

/// Escape a string for CSV output.
///
/// Quotes any field containing a delimiter, quote, `\n` **or `\r`**,
/// and neutralises spreadsheet formula injection by prefixing a `'` to fields
/// that start with `=`, `+`, `-` or `@`.
pub fn escape_csv_field(field: &str) -> String {
    let sanitized = if field.starts_with(['=', '+', '-', '@']) {
        format!("'{}", field)
    } else {
        field.to_string()
    };

    if sanitized.contains(',')
        || sanitized.contains('"')
        || sanitized.contains('\n')
        || sanitized.contains('\r')
    {
        format!("\"{}\"", sanitized.replace('"', "\"\""))
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{GeoDataExif, PhotoMetadata, Timestamp};
    use std::fs;
    use tempfile::TempDir;

    /// A genuine 160-byte 1x1 baseline JPEG (verified decodable).
    const TINY_JPEG: &[u8] = include_bytes!("../tests/fixtures/tiny.jpg");
    /// A genuine 70-byte 1x1 RGBA PNG.
    const TINY_PNG: &[u8] = include_bytes!("../tests/fixtures/tiny.png");
    /// A genuine 1x1 HEIC (HEVC still), produced by macOS `sips`.
    const TINY_HEIC: &[u8] = include_bytes!("../tests/fixtures/tiny.heic");

    /// 2021-01-01 00:00:00 UTC
    const TEST_TIMESTAMP: &str = "1609459200";

    fn base_metadata() -> PhotoMetadata {
        PhotoMetadata {
            title: None,
            description: None,
            photo_taken_time: Some(Timestamp {
                timestamp: Some(TEST_TIMESTAMP.to_string()),
                formatted: Some("2021-01-01 00:00:00 UTC".to_string()),
            }),
            geo_data: None,
            geo_data_exif: None,
            image_views: None,
            creation_time: None,
            modification_time: None,
            favorited: None,
            archive: None,
            mime_type: None,
            is_google_photos_media: None,
            is_shared: None,
            migrated: None,
        }
    }

    /// Read a STRING-valued tag back out of a file.
    fn read_string_tag(path: &Path, tag: &ExifTag) -> Option<String> {
        let metadata = Metadata::new_from_path(path).ok()?;
        let endian = metadata.get_endian();
        let value = metadata.get_tag(tag).next()?.value_as_u8_vec(&endian);
        Some(
            String::from_utf8_lossy(&value)
                .trim_end_matches('\0')
                .to_string(),
        )
    }

    fn read_rationals(path: &Path, tag: &ExifTag) -> Option<Vec<uR64>> {
        let metadata = Metadata::new_from_path(path).ok()?;
        match metadata.get_tag(tag).next()? {
            ExifTag::GPSLatitude(v) | ExifTag::GPSLongitude(v) | ExifTag::GPSAltitude(v) => {
                Some(v.clone())
            }
            _ => None,
        }
    }

    fn mtime_secs(path: &Path) -> i64 {
        filetime::FileTime::from_last_modification_time(&fs::metadata(path).unwrap()).unix_seconds()
    }

    /// The regression test for #20: `little_exif` rewrites the file, so the
    /// mtime must be applied *after* the EXIF write. Before the fix this
    /// observed "now" instead of photoTakenTime.
    #[test]
    fn test_mtime_survives_exif_write() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("photo.jpg");
        fs::write(&path, TINY_JPEG).unwrap();

        let outcome = write_metadata_to_file(&path, &base_metadata()).unwrap();
        assert_eq!(outcome, ExifWriteOutcome::ExifWritten);

        assert_eq!(
            mtime_secs(&path),
            TEST_TIMESTAMP.parse::<i64>().unwrap(),
            "final mtime must equal photoTakenTime, not the EXIF write time"
        );
    }

    /// Writing EXIF must not disturb an mtime-only (video) file either.
    #[test]
    fn test_video_gets_mtime_only() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("clip.mp4");
        fs::write(&path, b"not really an mp4").unwrap();

        let outcome = write_metadata_to_file(&path, &base_metadata()).unwrap();
        assert_eq!(outcome, ExifWriteOutcome::MtimeOnly);
        assert_eq!(mtime_secs(&path), TEST_TIMESTAMP.parse::<i64>().unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"not really an mp4");
    }

    /// Write date, description and GPS, then read every one of them back.
    #[test]
    fn test_exif_modification_and_preservation() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("test_image.jpg");
        fs::write(&path, TINY_JPEG).unwrap();

        // Seed the file with an unrelated tag that must survive our write.
        let mut seed = Metadata::new();
        seed.set_tag(ExifTag::Make("ACME Camera".to_string()));
        seed.write_to_file(&path).unwrap();

        let mut metadata = base_metadata();
        metadata.description = Some("A test image".to_string());
        metadata.geo_data_exif = Some(GeoDataExif {
            latitude: Some(51.5074),
            longitude: Some(-0.1278),
            altitude: Some(35.0),
            latitude_span: None,
            longitude_span: None,
        });

        let outcome = write_metadata_to_file(&path, &metadata).unwrap();
        assert_eq!(outcome, ExifWriteOutcome::ExifWritten);

        // Dates.
        assert_eq!(
            read_string_tag(&path, &ExifTag::DateTimeOriginal(String::new())).as_deref(),
            Some("2021:01:01 00:00:00")
        );
        assert_eq!(
            read_string_tag(&path, &ExifTag::CreateDate(String::new())).as_deref(),
            Some("2021:01:01 00:00:00")
        );
        assert_eq!(
            read_string_tag(&path, &ExifTag::ModifyDate(String::new())).as_deref(),
            Some("2021:01:01 00:00:00")
        );
        assert_eq!(
            read_string_tag(&path, &ExifTag::OffsetTimeOriginal(String::new())).as_deref(),
            Some(UTC_OFFSET)
        );
        assert_eq!(
            read_string_tag(&path, &ExifTag::OffsetTimeDigitized(String::new())).as_deref(),
            Some(UTC_OFFSET)
        );

        // Description.
        assert_eq!(
            read_string_tag(&path, &ExifTag::ImageDescription(String::new())).as_deref(),
            Some("A test image")
        );

        // Pre-existing, unrelated tag preserved.
        assert_eq!(
            read_string_tag(&path, &ExifTag::Make(String::new())).as_deref(),
            Some("ACME Camera")
        );

        // GPS.
        assert_eq!(
            read_string_tag(&path, &ExifTag::GPSLatitudeRef(String::new())).as_deref(),
            Some("N")
        );
        assert_eq!(
            read_string_tag(&path, &ExifTag::GPSLongitudeRef(String::new())).as_deref(),
            Some("W")
        );

        let lat = read_rationals(&path, &ExifTag::GPSLatitude(Vec::new())).unwrap();
        assert_eq!(lat.len(), 3);
        assert_eq!(lat[0].nominator, 51);
        assert_eq!(lat[1].nominator, 30);
        // 51.5074 -> 51° 30' 26.64"
        let seconds = lat[2].nominator as f64 / lat[2].denominator as f64;
        assert!(
            (seconds - 26.64).abs() < 0.01,
            "unexpected latitude seconds: {seconds}"
        );

        let lon = read_rationals(&path, &ExifTag::GPSLongitude(Vec::new())).unwrap();
        assert_eq!(lon[0].nominator, 0);
        assert_eq!(lon[1].nominator, 7);

        let alt = read_rationals(&path, &ExifTag::GPSAltitude(Vec::new())).unwrap();
        assert!((alt[0].nominator as f64 / alt[0].denominator as f64 - 35.0).abs() < 0.001);

        // And the mtime is still the capture time.
        assert_eq!(mtime_secs(&path), TEST_TIMESTAMP.parse::<i64>().unwrap());
    }

    /// PNG is now a write target (little_exif 0.6.23), and the file must stay
    /// a valid PNG afterwards.
    #[test]
    fn test_png_receives_exif() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("shot.png");
        fs::write(&path, TINY_PNG).unwrap();

        let outcome = write_metadata_to_file(&path, &base_metadata()).unwrap();
        assert_eq!(outcome, ExifWriteOutcome::ExifWritten);

        let bytes = fs::read(&path).unwrap();
        assert!(bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]));
        assert_eq!(
            read_string_tag(&path, &ExifTag::DateTimeOriginal(String::new())).as_deref(),
            Some("2021:01:01 00:00:00")
        );
        assert_eq!(mtime_secs(&path), TEST_TIMESTAMP.parse::<i64>().unwrap());
    }

    /// HEIC support ensures an iPhone library receives embedded metadata too.
    #[test]
    fn test_heic_receives_exif() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("IMG_0001.heic");
        fs::write(&path, TINY_HEIC).unwrap();

        let mut metadata = base_metadata();
        metadata.description = Some("On the beach".to_string());
        metadata.geo_data_exif = Some(GeoDataExif {
            latitude: Some(-33.8688),
            longitude: Some(151.2093),
            altitude: Some(-5.5),
            latitude_span: None,
            longitude_span: None,
        });

        let outcome = write_metadata_to_file(&path, &metadata).unwrap();
        assert_eq!(outcome, ExifWriteOutcome::ExifWritten);

        // Still a structurally valid ISO-BMFF file.
        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[4..8], b"ftyp");

        assert_eq!(
            read_string_tag(&path, &ExifTag::DateTimeOriginal(String::new())).as_deref(),
            Some("2021:01:01 00:00:00")
        );
        assert_eq!(
            read_string_tag(&path, &ExifTag::ImageDescription(String::new())).as_deref(),
            Some("On the beach")
        );
        // Southern / eastern hemisphere, below sea level.
        assert_eq!(
            read_string_tag(&path, &ExifTag::GPSLatitudeRef(String::new())).as_deref(),
            Some("S")
        );
        assert_eq!(
            read_string_tag(&path, &ExifTag::GPSLongitudeRef(String::new())).as_deref(),
            Some("E")
        );
        assert_eq!(mtime_secs(&path), TEST_TIMESTAMP.parse::<i64>().unwrap());
    }

    /// Unreadable old EXIF must not turn a successful fresh write into a
    /// failure. The batch reports the preservation caveat separately while
    /// still counting the sidecar-derived EXIF as embedded.
    #[test]
    fn test_unreadable_existing_exif_is_counted_as_fresh_success() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("bad-existing-exif.jpg");

        // Insert an EXIF APP1 segment with invalid TIFF endian bytes (`PI`)
        // after the JPEG start marker. The underlying image remains valid.
        let mut bytes = TINY_JPEG[..2].to_vec();
        bytes.extend_from_slice(&[
            0xff, 0xe1, 0x00, 0x10, b'E', b'x', b'i', b'f', 0, 0, b'P', b'I', 0, 0x2a, 0, 0, 0, 8,
        ]);
        bytes.extend_from_slice(&TINY_JPEG[2..]);
        fs::write(&path, bytes).unwrap();

        let load_error = Metadata::new_from_path(&path).unwrap_err();
        assert!(is_critical_exif_error(&load_error));

        let pairs = vec![MediaMetadataPair::WithMetadata(
            path.clone(),
            Box::new(base_metadata()),
            None,
        )];
        let summary = write_exif_metadata_batch(&pairs).unwrap();

        assert_eq!(summary.exif_written, 1);
        assert_eq!(summary.fresh_exif_blocks, 1);
        assert!(summary.failures.is_empty());
        assert_eq!(summary.total_processed(), 1);
        assert_eq!(
            read_string_tag(&path, &ExifTag::DateTimeOriginal(String::new())).as_deref(),
            Some("2021:01:01 00:00:00")
        );
    }

    /// A corrupt file with an image extension must survive untouched. The
    /// atomic-write path never lets a failed write reach the original.
    #[test]
    fn test_corrupt_file_is_left_untouched() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("broken.png");
        let original = b"fake png content";
        fs::write(&path, original).unwrap();

        let result = write_metadata_to_file(&path, &base_metadata());
        assert!(result.is_err(), "a corrupt PNG must be reported as failed");

        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "the original file must be byte-for-byte unchanged"
        );

        // No temp leftovers in the directory.
        let leftovers: Vec<_> = fs::read_dir(temp_dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("exiftmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files must be cleaned up");
    }

    /// Unsupported extensions still get their mtime and nothing else.
    #[test]
    fn test_graceful_failure_on_unsupported_files() {
        let temp_dir = TempDir::new().unwrap();
        let txt_path = temp_dir.path().join("test_file.txt");
        let original = b"fake text content";
        fs::write(&txt_path, original).unwrap();

        let outcome = write_metadata_to_file(&txt_path, &base_metadata()).unwrap();
        assert_eq!(outcome, ExifWriteOutcome::MtimeOnly);
        assert_eq!(fs::read(&txt_path).unwrap(), original);
    }

    #[test]
    fn test_missing_file_is_an_error() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("nope.jpg");
        assert!(write_metadata_to_file(&path, &base_metadata()).is_err());
    }

    #[test]
    fn test_zero_zero_gps_is_skipped() {
        let mut metadata = base_metadata();
        metadata.geo_data_exif = Some(GeoDataExif {
            latitude: Some(0.0),
            longitude: Some(0.0),
            altitude: Some(0.0),
            latitude_span: None,
            longitude_span: None,
        });
        assert!(gps_coordinates(&metadata).is_none());
    }

    #[test]
    fn test_geo_data_exif_preferred_over_geo_data() {
        use crate::metadata::GeoData;

        let mut metadata = base_metadata();
        metadata.geo_data = Some(GeoData {
            latitude: Some(1.0),
            longitude: Some(2.0),
            altitude: None,
            latitude_span: None,
            longitude_span: None,
        });
        metadata.geo_data_exif = Some(GeoDataExif {
            latitude: Some(10.0),
            longitude: Some(20.0),
            altitude: None,
            latitude_span: None,
            longitude_span: None,
        });

        let (lat, lon, _) = gps_coordinates(&metadata).unwrap();
        assert_eq!((lat, lon), (10.0, 20.0));

        // With a 0/0 geoDataExif we fall back to geoData.
        metadata.geo_data_exif = Some(GeoDataExif {
            latitude: Some(0.0),
            longitude: Some(0.0),
            altitude: None,
            latitude_span: None,
            longitude_span: None,
        });
        let (lat, lon, _) = gps_coordinates(&metadata).unwrap();
        assert_eq!((lat, lon), (1.0, 2.0));
    }

    #[test]
    fn test_degrees_to_dms() {
        let dms = degrees_to_dms(-0.1278);
        assert_eq!(dms[0].nominator, 0);
        assert_eq!(dms[1].nominator, 7);
        let secs = dms[2].nominator as f64 / dms[2].denominator as f64;
        assert!((secs - 40.08).abs() < 0.01, "got {secs}");

        // A value that would round the seconds up to 60 must carry into minutes.
        let dms = degrees_to_dms(1.9999999999);
        assert_eq!(dms[0].nominator, 2);
        assert_eq!(dms[1].nominator, 0);
        assert_eq!(dms[2].nominator, 0);
    }

    #[test]
    fn test_escape_csv_field() {
        assert_eq!(escape_csv_field("plain"), "plain");
        assert_eq!(escape_csv_field("a,b"), "\"a,b\"");
        assert_eq!(escape_csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(escape_csv_field("a\nb"), "\"a\nb\"");
        // #51: carriage returns must be quoted too.
        assert_eq!(escape_csv_field("a\rb"), "\"a\rb\"");
        // Formula injection is neutralised.
        assert_eq!(escape_csv_field("=1+1"), "'=1+1");
        assert_eq!(escape_csv_field("+cmd"), "'+cmd");
        assert_eq!(escape_csv_field("-cmd"), "'-cmd");
        assert_eq!(escape_csv_field("@SUM(A1)"), "'@SUM(A1)");
        // Prefixed *and* quoted when it also contains a delimiter.
        assert_eq!(escape_csv_field("=SUM(A1,B1)"), "\"'=SUM(A1,B1)\"");
    }

    #[test]
    fn test_format_utc_offset() {
        assert_eq!(format_utc_offset(0), "+00:00");
        assert_eq!(format_utc_offset(9 * 3600), "+09:00");
        assert_eq!(format_utc_offset(-5 * 3600), "-05:00");
        assert_eq!(format_utc_offset(5 * 3600 + 30 * 60), "+05:30");
        assert_eq!(format_utc_offset(-(3 * 3600 + 30 * 60)), "-03:30");
        assert_eq!(format_utc_offset(14 * 3600), "+14:00");
        // Sub-minute components (pre-standardisation LMT zones) truncate rather
        // than producing a shape EXIF readers cannot parse.
        assert_eq!(format_utc_offset(9 * 3600 + 59), "+09:00");
        assert_eq!(format_utc_offset(-(9 * 3600 + 59)), "-09:00");
        // i32::MIN would overflow a naive `-x`; `unsigned_abs` must not panic.
        let _ = format_utc_offset(i32::MIN);
    }

    /// The atom reader must reject sizes that overrun their container rather
    /// than letting the walk wander into media payload.
    #[test]
    fn test_read_atom_header_rejects_overruns() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("clip.mp4");

        // An atom that claims 4 KiB inside a 16-byte file.
        let mut bytes = 4096u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"moov");
        bytes.extend_from_slice(&[0u8; 8]);
        fs::write(&path, &bytes).unwrap();

        let len = bytes.len() as u64;
        let mut file = fs::File::open(&path).unwrap();
        assert!(read_atom_header(&mut file, 0, len).unwrap().is_none());
        assert!(find_mvhd(&mut file, len).unwrap().is_none());

        // A size smaller than the header it describes is equally bogus.
        let mut bytes = 3u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"moov");
        fs::write(&path, &bytes).unwrap();
        let mut file = fs::File::open(&path).unwrap();
        assert!(
            read_atom_header(&mut file, 0, bytes.len() as u64)
                .unwrap()
                .is_none()
        );
    }

    /// A version-0 `mvhd` cannot represent a date past early 2040, and must say
    /// so rather than wrapping silently back round to 1904.
    #[test]
    fn test_mvhd_v0_rejects_post_2040_dates() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("clip.mp4");

        let mut mvhd = 108u32.to_be_bytes().to_vec();
        mvhd.extend_from_slice(b"mvhd");
        mvhd.extend_from_slice(&[0u8; 100]); // version 0 + zeroed body
        let mut moov = ((8 + mvhd.len()) as u32).to_be_bytes().to_vec();
        moov.extend_from_slice(b"moov");
        moov.extend_from_slice(&mvhd);
        fs::write(&path, &moov).unwrap();

        // 2100-01-01: 2082844800 + 4102444800 overflows a u32.
        assert!(write_video_date(&path, 4_102_444_800).is_err());
        assert_eq!(fs::read(&path).unwrap(), moov, "original must survive");
    }

    #[test]
    fn test_mvhd_time_fields_rejects_short_atoms() {
        // A 20-byte mvhd has room for version-0 fields but not version-1 ones.
        let mvhd = AtomHeader {
            start: 0,
            header_len: 8,
            total_len: 20,
            kind: *b"mvhd",
        };
        assert_eq!(mvhd_time_fields(&mvhd, 0).unwrap(), (12, 16, 4));
        assert!(mvhd_time_fields(&mvhd, 1).is_err());
        // Only versions 0 and 1 exist.
        assert!(mvhd_time_fields(&mvhd, 2).is_err());
    }

    #[test]
    fn test_batch_summary_accounting() {
        let temp_dir = TempDir::new().unwrap();

        let jpg = temp_dir.path().join("a.jpg");
        fs::write(&jpg, TINY_JPEG).unwrap();
        let mp4 = temp_dir.path().join("b.mp4");
        fs::write(&mp4, b"video").unwrap();
        let broken = temp_dir.path().join("c.png");
        fs::write(&broken, b"not a png").unwrap();
        let orphan = temp_dir.path().join("d.jpg");
        fs::write(&orphan, TINY_JPEG).unwrap();

        let pairs = vec![
            MediaMetadataPair::WithMetadata(jpg, Box::new(base_metadata()), None),
            MediaMetadataPair::WithMetadata(mp4, Box::new(base_metadata()), None),
            MediaMetadataPair::WithMetadata(broken, Box::new(base_metadata()), None),
            MediaMetadataPair::WithoutMetadata(orphan),
        ];

        let summary = write_exif_metadata_batch(&pairs).unwrap();
        assert_eq!(summary.exif_written, 1);
        assert_eq!(summary.mtime_only, 1);
        assert_eq!(summary.skipped_no_metadata, 1);
        assert_eq!(summary.failures.len(), 1);
        assert_eq!(summary.total_processed(), 4);
    }
}
