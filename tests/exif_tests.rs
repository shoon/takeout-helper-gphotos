// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use chrono_tz::Tz;
use little_exif::exif_tag::ExifTag;
use little_exif::metadata::Metadata;
use std::fs;
use std::path::Path;
use takeout_helper_gphotos::exif;
use takeout_helper_gphotos::metadata::{GeoDataExif, MediaMetadataPair, PhotoMetadata, Timestamp};

/// A genuine 160-byte 1x1 baseline JPEG.
const TINY_JPEG: &[u8] = include_bytes!("fixtures/tiny.jpg");

/// 2021-01-01 00:00:00 UTC
const TEST_TIMESTAMP: i64 = 1_609_459_200;

/// Seconds between the QuickTime epoch (1904-01-01) and the Unix epoch.
const QUICKTIME_EPOCH_OFFSET: i64 = 2_082_844_800;

fn metadata_with_timestamp() -> PhotoMetadata {
    PhotoMetadata {
        title: Some("Test Image".to_string()),
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

fn mtime_secs(path: &Path) -> i64 {
    let modified = fs::metadata(path).unwrap().modified().unwrap();
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[test]
fn test_unix_to_exif_datetime() {
    // Test valid timestamp
    let result = exif::unix_to_exif_datetime("1609459200");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "2021:01:01 00:00:00");

    // Test invalid timestamp
    let result = exif::unix_to_exif_datetime("invalid");
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().to_string(), "invalid timestamp");

    // Test negative timestamp
    let result = exif::unix_to_exif_datetime("-1000");
    assert!(result.is_ok());
    // This should be a valid date, just in the past
    let result_str = result.unwrap();
    assert!(!result_str.is_empty());

    // Test zero timestamp
    let result = exif::unix_to_exif_datetime("0");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "1970:01:01 00:00:00");
}

#[test]
fn test_is_supported_format() {
    // Everything little_exif 0.6.23 can write.
    for name in [
        "test.jpg",
        "test.jpeg",
        "test.JPG",
        "test.png",
        "test.heic",
        "test.heif",
        "test.hif",
        "test.avif",
        "test.jxl",
        "test.tiff",
        "test.tif",
        "test.webp",
    ] {
        assert!(
            exif::is_supported_format(Path::new(name)),
            "{name} should be EXIF-writable"
        );
    }

    // Videos and non-media stay mtime-only.
    for name in [
        "test.mp4",
        "test.mov",
        "test.txt",
        "test.pdf",
        "noextension",
    ] {
        assert!(
            !exif::is_supported_format(Path::new(name)),
            "{name} should not be EXIF-writable"
        );
    }
}

#[test]
fn test_check_exiftool_available() {
    // The pure-Rust implementation never shells out to exiftool, so this stub
    // must always report that exiftool is unavailable.
    assert!(!exif::check_exiftool_available());
}

/// After the EXIF write the file's mtime must
/// still be photoTakenTime, not the moment little_exif rewrote the file.
#[test]
fn test_final_mtime_equals_photo_taken_time() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let path = temp_dir.path().join("IMG_0001.jpg");
    fs::write(&path, TINY_JPEG).unwrap();

    exif::write_metadata_to_file(&path, &metadata_with_timestamp()).unwrap();

    assert_eq!(
        mtime_secs(&path),
        TEST_TIMESTAMP,
        "mtime must equal photoTakenTime after the EXIF write"
    );
}

/// The batch reports accurate per-file accounting instead of `()`.
#[test]
fn test_batch_returns_summary() {
    let work_dir = tempfile::TempDir::new().unwrap();

    let photo = work_dir.path().join("photo.jpg");
    fs::write(&photo, TINY_JPEG).unwrap();
    let video = work_dir.path().join("clip.mov");
    fs::write(&video, b"pretend video").unwrap();
    let orphan = work_dir.path().join("orphan.jpg");
    fs::write(&orphan, TINY_JPEG).unwrap();

    let pairs = vec![
        MediaMetadataPair::WithMetadata(photo.clone(), Box::new(metadata_with_timestamp()), None),
        MediaMetadataPair::WithMetadata(video.clone(), Box::new(metadata_with_timestamp()), None),
        MediaMetadataPair::WithoutMetadata(orphan),
    ];

    let summary = exif::write_exif_metadata_batch(&pairs).unwrap();

    assert_eq!(summary.exif_written, 1);
    assert_eq!(summary.mtime_only, 1);
    assert_eq!(summary.skipped_no_metadata, 1);
    assert!(summary.failures.is_empty(), "{:?}", summary.failures);

    // Both the photo and the video carry the corrected date.
    assert_eq!(mtime_secs(&photo), TEST_TIMESTAMP);
    assert_eq!(mtime_secs(&video), TEST_TIMESTAMP);
}

#[test]
fn test_escape_csv_field_quotes_and_neutralises() {
    assert_eq!(exif::escape_csv_field("plain"), "plain");
    assert_eq!(exif::escape_csv_field("a,b"), "\"a,b\"");
    assert_eq!(exif::escape_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    assert_eq!(exif::escape_csv_field("line1\nline2"), "\"line1\nline2\"");
    // #51: carriage return must trigger quoting too.
    assert_eq!(
        exif::escape_csv_field("line1\r\nline2"),
        "\"line1\r\nline2\""
    );
    assert_eq!(exif::escape_csv_field("bare\rcr"), "\"bare\rcr\"");
    // Excel/Sheets formula injection.
    assert_eq!(exif::escape_csv_field("=1+1"), "'=1+1");
    assert_eq!(exif::escape_csv_field("+41 555"), "'+41 555");
    assert_eq!(exif::escape_csv_field("-1"), "'-1");
    assert_eq!(exif::escape_csv_field("@import"), "'@import");
    assert_eq!(
        exif::escape_csv_field("=HYPERLINK(\"a\",\"b\")"),
        "\"'=HYPERLINK(\"\"a\"\",\"\"b\"\")\""
    );
}

// ---------------------------------------------------------------------------
// Timezone-aware EXIF dates
// ---------------------------------------------------------------------------

/// The base metadata, geotagged at the given coordinates via `geoDataExif`.
fn metadata_at(latitude: f64, longitude: f64) -> PhotoMetadata {
    let mut metadata = metadata_with_timestamp();
    metadata.geo_data_exif = Some(GeoDataExif {
        latitude: Some(latitude),
        longitude: Some(longitude),
        altitude: None,
        latitude_span: None,
        longitude_span: None,
    });
    metadata
}

/// Read a STRING-valued EXIF tag back out of a file.
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

#[test]
fn test_resolve_timezone_from_gps() {
    // Central Tokyo.
    assert_eq!(
        exif::resolve_timezone(&metadata_at(35.68, 139.77), None),
        Some(Tz::Asia__Tokyo)
    );
    // Manhattan, to prove the lookup is not returning one hard-coded answer.
    assert_eq!(
        exif::resolve_timezone(&metadata_at(40.7128, -74.0060), None),
        Some(Tz::America__New_York)
    );
}

#[test]
fn test_resolve_timezone_override_wins() {
    // An explicit --timezone beats the file's own coordinates...
    assert_eq!(
        exif::resolve_timezone(&metadata_at(35.68, 139.77), Some(Tz::Asia__Kolkata)),
        Some(Tz::Asia__Kolkata)
    );
    // ...and applies to a file that has no coordinates at all.
    assert_eq!(
        exif::resolve_timezone(&metadata_with_timestamp(), Some(Tz::Asia__Kolkata)),
        Some(Tz::Asia__Kolkata)
    );
}

#[test]
fn test_resolve_timezone_without_usable_gps() {
    // Google's 0/0 "unknown location" sentinel must not resolve to Africa/Accra.
    assert_eq!(exif::resolve_timezone(&metadata_at(0.0, 0.0), None), None);
    // Neither must out-of-range junk.
    assert_eq!(exif::resolve_timezone(&metadata_at(91.0, 0.5), None), None);
    // Nor a file with no geo block whatsoever.
    assert_eq!(
        exif::resolve_timezone(&metadata_with_timestamp(), None),
        None
    );
}

#[test]
fn test_exif_datetime_offset_formatting() {
    let stamp = TEST_TIMESTAMP.to_string();

    // No zone: UTC, exactly as before timezone support existed.
    assert_eq!(
        exif::unix_to_exif_datetime_tz(&stamp, None).unwrap(),
        ("2021:01:01 00:00:00".to_string(), "+00:00".to_string())
    );

    // Positive whole-hour offset.
    assert_eq!(
        exif::unix_to_exif_datetime_tz(&stamp, Some(Tz::Asia__Tokyo)).unwrap(),
        ("2021:01:01 09:00:00".to_string(), "+09:00".to_string())
    );

    // A negative offset rolls the local date back into the previous year.
    assert_eq!(
        exif::unix_to_exif_datetime_tz(&stamp, Some(Tz::America__New_York)).unwrap(),
        ("2020:12:31 19:00:00".to_string(), "-05:00".to_string())
    );

    // Half-hour zone.
    assert_eq!(
        exif::unix_to_exif_datetime_tz(&stamp, Some(Tz::Asia__Kolkata)).unwrap(),
        ("2021:01:01 05:30:00".to_string(), "+05:30".to_string())
    );

    // Quarter-hour zone, for the genuinely awkward case.
    assert_eq!(
        exif::unix_to_exif_datetime_tz(&stamp, Some(Tz::Asia__Kathmandu)).unwrap(),
        ("2021:01:01 05:45:00".to_string(), "+05:45".to_string())
    );

    // The UTC-only helper still agrees with the pair-returning one.
    assert_eq!(
        exif::unix_to_exif_datetime(&stamp).unwrap(),
        "2021:01:01 00:00:00"
    );
}

#[test]
fn test_offset_is_dst_correct_at_the_transition() {
    // US DST 2021 began at 2021-03-14 07:00:00 UTC: 01:59:59 EST becomes
    // 03:00:00 EDT. The offset must come from the instant, not from a fixed
    // per-zone table.
    let before = 1_615_705_199i64; // 07:00:00 UTC minus one second
    let after = 1_615_705_200i64;

    assert_eq!(
        exif::unix_to_exif_datetime_tz(&before.to_string(), Some(Tz::America__New_York)).unwrap(),
        ("2021:03:14 01:59:59".to_string(), "-05:00".to_string())
    );
    assert_eq!(
        exif::unix_to_exif_datetime_tz(&after.to_string(), Some(Tz::America__New_York)).unwrap(),
        ("2021:03:14 03:00:00".to_string(), "-04:00".to_string())
    );

    // Southern-hemisphere DST runs the other way round.
    assert_eq!(
        exif::unix_to_exif_datetime_tz(&TEST_TIMESTAMP.to_string(), Some(Tz::Australia__Sydney))
            .unwrap(),
        ("2021:01:01 11:00:00".to_string(), "+11:00".to_string())
    );
}

#[test]
fn test_batch_writes_local_time_resolved_from_gps() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("tokyo.jpg");
    fs::write(&path, TINY_JPEG).unwrap();

    let pairs = vec![MediaMetadataPair::WithMetadata(
        path.clone(),
        Box::new(metadata_at(35.68, 139.77)),
        None,
    )];

    let summary = exif::write_exif_metadata_batch_with_tz(&pairs, None).unwrap();
    assert_eq!(summary.exif_written, 1);
    assert!(summary.failures.is_empty(), "{:?}", summary.failures);

    // Tokyo is +09:00 year-round, so the wall-clock time is 09:00 local.
    assert_eq!(
        read_string_tag(&path, &ExifTag::DateTimeOriginal(String::new())).as_deref(),
        Some("2021:01:01 09:00:00")
    );
    assert_eq!(
        read_string_tag(&path, &ExifTag::CreateDate(String::new())).as_deref(),
        Some("2021:01:01 09:00:00")
    );
    assert_eq!(
        read_string_tag(&path, &ExifTag::OffsetTimeOriginal(String::new())).as_deref(),
        Some("+09:00")
    );
    assert_eq!(
        read_string_tag(&path, &ExifTag::OffsetTimeDigitized(String::new())).as_deref(),
        Some("+09:00")
    );

    // The mtime is an absolute instant and stays in UTC.
    assert_eq!(mtime_secs(&path), TEST_TIMESTAMP);
}

#[test]
fn test_batch_timezone_override_beats_gps() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("tokyo.jpg");
    fs::write(&path, TINY_JPEG).unwrap();

    let pairs = vec![MediaMetadataPair::WithMetadata(
        path.clone(),
        Box::new(metadata_at(35.68, 139.77)),
        None,
    )];

    let summary =
        exif::write_exif_metadata_batch_with_tz(&pairs, Some(Tz::America__New_York)).unwrap();
    assert_eq!(summary.exif_written, 1);

    assert_eq!(
        read_string_tag(&path, &ExifTag::DateTimeOriginal(String::new())).as_deref(),
        Some("2020:12:31 19:00:00")
    );
    assert_eq!(
        read_string_tag(&path, &ExifTag::OffsetTimeOriginal(String::new())).as_deref(),
        Some("-05:00")
    );
}

#[test]
fn test_batch_without_gps_still_writes_utc() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("nowhere.jpg");
    fs::write(&path, TINY_JPEG).unwrap();

    let pairs = vec![MediaMetadataPair::WithMetadata(
        path.clone(),
        Box::new(metadata_with_timestamp()),
        None,
    )];

    // The plain entry point must behave exactly as it did before this feature.
    let summary = exif::write_exif_metadata_batch(&pairs).unwrap();
    assert_eq!(summary.exif_written, 1);

    assert_eq!(
        read_string_tag(&path, &ExifTag::DateTimeOriginal(String::new())).as_deref(),
        Some("2021:01:01 00:00:00")
    );
    assert_eq!(
        read_string_tag(&path, &ExifTag::OffsetTimeOriginal(String::new())).as_deref(),
        Some("+00:00")
    );
}

// ---------------------------------------------------------------------------
// Video (MP4/MOV/M4V) date patching
// ---------------------------------------------------------------------------

/// Build an ISO-BMFF atom: 4-byte big-endian size, 4-byte type, then payload.
fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&u32::try_from(8 + payload.len()).unwrap().to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A version-0 `mvhd` payload: version + flags, creation_time, modification_time,
/// timescale, duration, then the fixed 80-byte tail (rate .. next_track_ID).
fn mvhd_v0_payload() -> Vec<u8> {
    let mut payload = vec![0u8, 0, 0, 0];
    payload.extend_from_slice(&0u32.to_be_bytes()); // creation_time
    payload.extend_from_slice(&0u32.to_be_bytes()); // modification_time
    payload.extend_from_slice(&600u32.to_be_bytes()); // timescale
    payload.extend_from_slice(&1200u32.to_be_bytes()); // duration
    payload.extend_from_slice(&[0u8; 80]);
    payload
}

/// The same, with 64-bit times and duration (version 1).
fn mvhd_v1_payload() -> Vec<u8> {
    let mut payload = vec![1u8, 0, 0, 0];
    payload.extend_from_slice(&0u64.to_be_bytes()); // creation_time
    payload.extend_from_slice(&0u64.to_be_bytes()); // modification_time
    payload.extend_from_slice(&600u32.to_be_bytes()); // timescale
    payload.extend_from_slice(&1200u64.to_be_bytes()); // duration
    payload.extend_from_slice(&[0u8; 80]);
    payload
}

struct Mp4Fixture {
    bytes: Vec<u8>,
    creation_offset: usize,
    modification_offset: usize,
    /// 4 bytes per timestamp for a version-0 `mvhd`, 8 for version 1.
    width: usize,
}

/// A minimal but structurally valid MP4: `ftyp`, then an `mdat` whose payload
/// contains the literal bytes `mvhd`, then `moov` holding the real `mvhd`
/// alongside a `trak` sibling.
///
/// The ordering is the point: a forward byte-scan for "mvhd" hits the decoy in
/// the media payload first and would corrupt the picture, so this fixture fails
/// loudly if the atom walk is ever replaced by one.
fn minimal_mp4(mvhd_payload: &[u8]) -> Mp4Fixture {
    let ftyp = atom(b"ftyp", b"isom\x00\x00\x02\x00isomiso2mp41");
    let mdat = atom(b"mdat", b"\x00\x01mvhd\xff\xfe payload payload payload");

    let mvhd = atom(b"mvhd", mvhd_payload);
    let trak = atom(b"trak", &atom(b"tkhd", &[0u8; 84]));

    let mut moov_payload = Vec::new();
    moov_payload.extend_from_slice(&mvhd);
    moov_payload.extend_from_slice(&trak);
    let moov = atom(b"moov", &moov_payload);

    // The mvhd body starts after ftyp, mdat, the moov header and the mvhd header.
    let body = ftyp.len() + mdat.len() + 8 + 8;
    let width = if mvhd_payload[0] == 0 { 4 } else { 8 };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&ftyp);
    bytes.extend_from_slice(&mdat);
    bytes.extend_from_slice(&moov);

    Mp4Fixture {
        bytes,
        creation_offset: body + 4,
        modification_offset: body + 4 + width,
        width,
    }
}

/// Read a big-endian unsigned integer of `width` bytes.
fn read_be(bytes: &[u8], offset: usize, width: usize) -> u64 {
    bytes[offset..offset + width]
        .iter()
        .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte))
}

/// Assert that the patch touched the two timestamp fields and nothing else.
fn assert_only_timestamps_changed(fixture: &Mp4Fixture, patched: &[u8], expected: u64) {
    assert_eq!(
        patched.len(),
        fixture.bytes.len(),
        "patching must not resize the file"
    );

    let encoded = &expected.to_be_bytes()[8 - fixture.width..];
    let mut want = fixture.bytes.clone();
    want[fixture.creation_offset..fixture.creation_offset + fixture.width].copy_from_slice(encoded);
    want[fixture.modification_offset..fixture.modification_offset + fixture.width]
        .copy_from_slice(encoded);

    assert_eq!(
        patched,
        &want[..],
        "only the mvhd creation/modification fields may change"
    );
}

fn assert_no_temp_files(dir: &Path) {
    let leftovers: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains("exiftmp"))
        .map(|entry| entry.path())
        .collect();
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

#[test]
fn test_is_video_format() {
    for name in ["clip.mp4", "clip.MOV", "clip.m4v"] {
        assert!(exif::is_video_format(Path::new(name)), "{name}");
    }
    for name in ["photo.jpg", "clip.avi", "clip.webm", "noextension"] {
        assert!(!exif::is_video_format(Path::new(name)), "{name}");
    }
}

#[test]
fn test_write_video_date_patches_mvhd_v0() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("clip.mp4");
    let fixture = minimal_mp4(&mvhd_v0_payload());
    fs::write(&path, &fixture.bytes).unwrap();

    exif::write_video_date(&path, TEST_TIMESTAMP).unwrap();

    let patched = fs::read(&path).unwrap();
    let expected = (TEST_TIMESTAMP + QUICKTIME_EPOCH_OFFSET) as u64;
    assert_eq!(read_be(&patched, fixture.creation_offset, 4), expected);
    assert_eq!(read_be(&patched, fixture.modification_offset, 4), expected);
    assert_only_timestamps_changed(&fixture, &patched, expected);
    assert_no_temp_files(dir.path());
}

#[test]
fn test_write_video_date_patches_mvhd_v1() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("clip.mov");
    let fixture = minimal_mp4(&mvhd_v1_payload());
    fs::write(&path, &fixture.bytes).unwrap();

    exif::write_video_date(&path, TEST_TIMESTAMP).unwrap();

    let patched = fs::read(&path).unwrap();
    let expected = (TEST_TIMESTAMP + QUICKTIME_EPOCH_OFFSET) as u64;
    assert_eq!(read_be(&patched, fixture.creation_offset, 8), expected);
    assert_eq!(read_be(&patched, fixture.modification_offset, 8), expected);
    assert_only_timestamps_changed(&fixture, &patched, expected);
    assert_no_temp_files(dir.path());
}

#[test]
fn test_write_video_date_without_moov_leaves_file_untouched() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("clip.mp4");

    // ftyp + an mdat carrying the "mvhd" decoy, and no moov anywhere.
    let mut bytes = atom(b"ftyp", b"isom\x00\x00\x02\x00isomiso2mp41");
    bytes.extend_from_slice(&atom(b"mdat", b"\x00\x01mvhd\xff\xfe payload"));

    fs::write(&path, &bytes).unwrap();

    assert!(
        exif::write_video_date(&path, TEST_TIMESTAMP).is_err(),
        "a file with no moov/mvhd has nothing to patch"
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "the original must be byte-for-byte unchanged"
    );
    assert_no_temp_files(dir.path());
}

#[test]
fn test_write_video_date_on_garbage_leaves_file_untouched() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("clip.mp4");
    let original = b"not really an mp4 at all".to_vec();
    fs::write(&path, &original).unwrap();

    assert!(exif::write_video_date(&path, TEST_TIMESTAMP).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_no_temp_files(dir.path());
}

#[test]
fn test_write_video_date_rejects_dates_before_1904() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("clip.mp4");
    let fixture = minimal_mp4(&mvhd_v0_payload());
    fs::write(&path, &fixture.bytes).unwrap();

    // 1900-01-01 is four years before the QuickTime epoch.
    assert!(exif::write_video_date(&path, -2_208_988_800).is_err());
    assert_eq!(fs::read(&path).unwrap(), fixture.bytes);
    assert_no_temp_files(dir.path());
}

#[test]
fn test_batch_counts_video_dates_and_falls_back_cleanly() {
    let dir = tempfile::TempDir::new().unwrap();

    let good = dir.path().join("good.mp4");
    let fixture = minimal_mp4(&mvhd_v0_payload());
    fs::write(&good, &fixture.bytes).unwrap();

    // No moov: must degrade to mtime-only rather than being reported as failed.
    let plain = dir.path().join("plain.mov");
    fs::write(&plain, b"pretend video").unwrap();

    let pairs = vec![
        MediaMetadataPair::WithMetadata(good.clone(), Box::new(metadata_with_timestamp()), None),
        MediaMetadataPair::WithMetadata(plain.clone(), Box::new(metadata_with_timestamp()), None),
    ];

    let summary = exif::write_exif_metadata_batch(&pairs).unwrap();
    assert_eq!(summary.video_dates_written, 1);
    assert_eq!(summary.mtime_only, 1);
    assert!(summary.failures.is_empty(), "{:?}", summary.failures);
    assert_eq!(summary.total_processed(), 2);

    // The patched video carries the date in both places...
    let patched = fs::read(&good).unwrap();
    assert_eq!(
        read_be(&patched, fixture.creation_offset, 4),
        (TEST_TIMESTAMP + QUICKTIME_EPOCH_OFFSET) as u64
    );
    assert_eq!(mtime_secs(&good), TEST_TIMESTAMP);

    // ...and the unpatchable one still got its mtime corrected, unmodified.
    assert_eq!(mtime_secs(&plain), TEST_TIMESTAMP);
    assert_eq!(fs::read(&plain).unwrap(), b"pretend video");
    assert_no_temp_files(dir.path());
}
