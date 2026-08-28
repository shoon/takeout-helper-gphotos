// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use std::collections::HashMap;
use std::path::Path;
use takeout_helper_gphotos::manifest::Manifest;
use takeout_helper_gphotos::metadata::MediaMetadataPair;
use takeout_helper_gphotos::organizer::*;

#[test]
fn test_format_date_path() {
    // Create a specific date for testing
    let date = chrono::DateTime::from_timestamp(1610668800, 0).unwrap(); // 2021-01-15 00:00:00 UTC

    // Call format_date_path
    let result = format_date_path(&date);

    // Verify the result is formatted correctly as YYYY/MM
    assert_eq!(result, "2021/01");
}

#[test]
fn test_generate_unique_filename_no_conflict() {
    // Create a temporary directory for our test
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a test file path (but don't actually create the file)
    let file_path = temp_dir.path().join("test.jpg");
    let file_name = "test.jpg".to_string();

    // Call generate_unique_filename
    let result = generate_unique_filename(&file_path, temp_dir.path()).unwrap();

    // Verify the result is the same as the original file path since there's no conflict
    assert_eq!(result.file_name().unwrap().to_string_lossy(), file_name);
}

#[test]
fn test_generate_unique_filename_with_conflict() {
    // Create a temporary directory for our test
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a test file path
    let file_path = temp_dir.path().join("test.jpg");
    // Create a conflict file (same name)
    let conflict_file = temp_dir.path().join("test.jpg");
    std::fs::write(&conflict_file, "conflicting content").unwrap();

    // Call generate_unique_filename
    let result = generate_unique_filename(&file_path, temp_dir.path()).unwrap();

    // Verify the result is a unique filename with _1 appended
    assert_eq!(result.file_name().unwrap().to_string_lossy(), "test_1.jpg");
}

#[test]
fn test_parse_unix_timestamp_valid() {
    // Test with a valid timestamp
    let timestamp = "1609459200"; // 2021-01-01 00:00:00 UTC

    // Call parse_unix_timestamp
    let result = parse_unix_timestamp(timestamp);

    // Verify the result is OK
    assert!(result.is_ok());

    let date = result.unwrap();

    // Verify the date is correct
    assert_eq!(date.timestamp(), 1609459200);
}

#[test]
fn test_parse_unix_timestamp_invalid() {
    // Test with an invalid timestamp
    let timestamp = "invalid";

    // Call parse_unix_timestamp
    let result = parse_unix_timestamp(timestamp);

    // Verify the result is an error
    assert!(result.is_err());

    // Verify the error type is correct
    match result.unwrap_err() {
        OrganizerError::TimestampParse(_) => (), // Expected
        _ => panic!("Expected TimestampParseError"),
    }
}

#[test]
fn test_organize_single_file_with_metadata() {
    // Create a temporary directory for our test files
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a test media file
    let media_file = temp_dir.path().join("photo.jpg");
    std::fs::write(&media_file, "photo content").unwrap();

    // Create a test JSON file
    let json_file = temp_dir.path().join("photo.jpg.json");
    let json_content = r#"{
        "title": "Test Photo",
        "photoTakenTime": {
            "timestamp": "1609459200"
        }
    }"#;
    std::fs::write(&json_file, json_content).unwrap();

    // Create output directory
    let output_dir = tempfile::tempdir().unwrap();

    // Load metadata
    let metadata = takeout_helper_gphotos::metadata::load_metadata(&json_file).unwrap();

    // Create a MediaMetadataPair
    let pair = MediaMetadataPair::WithMetadata(media_file, Box::new(metadata), None);

    // Call organize_single_file with an empty live_photo_dates map for testing
    let result = organize_single_file(pair, output_dir.path(), &HashMap::new());

    // This should succeed
    assert!(result.is_ok());
}

#[test]
fn test_copy_file_to_organized_location() {
    // Create a temporary directory for our test files
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a test media file
    let media_file = temp_dir.path().join("photo.jpg");
    std::fs::write(&media_file, "photo content").unwrap();

    // Create output directory
    let output_dir = tempfile::tempdir().unwrap();

    // Call copy_file_to_organized_location
    let result = copy_file_to_organized_location(&media_file, output_dir.path()).unwrap();

    assert!(!result.duplicate);

    // Verify the file was copied
    let copied_file = output_dir.path().join("photo.jpg");
    assert!(copied_file.exists());
    assert_eq!(result.destination, copied_file);
    assert_eq!(
        std::fs::read_to_string(&copied_file).unwrap(),
        "photo content"
    );
}

#[test]
fn test_create_date_directory() {
    // Create a temporary directory for our test
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a subdirectory path
    let date_dir = temp_dir.path().join("2021").join("01");

    // Call create_date_directory
    let result = create_date_directory(&date_dir);

    // This should succeed
    assert!(result.is_ok());

    // Verify the directory was created
    assert!(date_dir.exists());
}

#[test]
fn test_get_file_creation_date() {
    // Create a temporary directory for our test
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a test file
    let test_file = temp_dir.path().join("test.txt");
    std::fs::write(&test_file, "test content").unwrap();

    // Call get_file_creation_date
    let result = get_file_creation_date(&test_file);

    // This should succeed
    assert!(result.is_ok());
}

#[test]
fn test_extract_photo_date_with_metadata() {
    // Create a temporary directory for our test files
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a test media file
    let media_file = temp_dir.path().join("photo.jpg");
    std::fs::write(&media_file, "photo content").unwrap();

    // Create a test JSON file
    let json_file = temp_dir.path().join("photo.jpg.json");
    let json_content = r#"{
        "title": "Test Photo",
        "photoTakenTime": {
            "timestamp": "1609459200"
        }
    }"#;
    std::fs::write(&json_file, json_content).unwrap();

    // Load metadata
    let metadata = takeout_helper_gphotos::metadata::load_metadata(&json_file).unwrap();

    // Create a MediaMetadataPair
    let pair = MediaMetadataPair::WithMetadata(media_file.clone(), Box::new(metadata), None);

    // Call extract_photo_date with an empty live_photo_dates map for testing
    let result = extract_photo_date(pair, &HashMap::new());

    // This should succeed
    assert!(result.is_ok());

    let (path, date) = result.unwrap();

    // Verify the path is correct
    assert_eq!(path, media_file);

    // Verify the date is correct (2021-01-01)
    assert_eq!(date.known().unwrap().timestamp(), 1609459200);
}

#[test]
fn test_extract_photo_date_uses_creation_time_fallback() {
    let temp_dir = tempfile::tempdir().unwrap();

    let media_file = temp_dir.path().join("photo.jpg");
    std::fs::write(&media_file, "photo content").unwrap();

    // No photoTakenTime, only creationTime.
    let json_file = temp_dir.path().join("photo.jpg.json");
    let json_content = r#"{
        "title": "Test Photo",
        "creationTime": {
            "timestamp": "1609459200"
        }
    }"#;
    std::fs::write(&json_file, json_content).unwrap();

    let metadata = takeout_helper_gphotos::metadata::load_metadata(&json_file).unwrap();
    let pair = MediaMetadataPair::WithMetadata(media_file, Box::new(metadata), None);

    let (_path, date) = extract_photo_date(pair, &HashMap::new()).unwrap();
    assert_eq!(date.known().unwrap().timestamp(), 1609459200);
}

#[test]
fn test_extract_photo_date_without_metadata() {
    // Create a temporary directory for our test
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a test media file
    let media_file = temp_dir.path().join("photo.jpg");
    std::fs::write(&media_file, "photo content").unwrap();

    // Create a MediaMetadataPair without metadata
    let pair = MediaMetadataPair::WithoutMetadata(media_file.clone());

    // Call extract_photo_date with an empty live_photo_dates map for testing
    let (path, date) = extract_photo_date(pair, &HashMap::new()).unwrap();

    assert_eq!(path, media_file);
    // The file was created by this test run, so its mtime carries no
    // information: it must be Unknown, not "now".
    assert_eq!(date, PhotoDate::Unknown);
}

#[test]
fn test_live_photo_date_mapping() {
    // Create a temporary directory for our test files
    let temp_dir = tempfile::tempdir().unwrap();

    // Create a test media file (photo)
    let photo_file = temp_dir.path().join("IMG_1234.HEIC");
    std::fs::write(&photo_file, "photo content").unwrap();

    // Create a test JSON file for the photo with a specific timestamp
    let photo_json_file = temp_dir
        .path()
        .join("IMG_1234.HEIC.supplemental-metadata.json");
    let photo_json_content = r#"{
        "title": "Test Live Photo",
        "photoTakenTime": {
            "timestamp": "1609459200"
        }
    }"#;
    std::fs::write(&photo_json_file, photo_json_content).unwrap();

    // Create a test video file (part of the Live Photo)
    let video_file = temp_dir.path().join("IMG_1234.MP4");
    std::fs::write(&video_file, "video content").unwrap();

    // Create a MediaMetadataPair for the photo
    let photo_metadata = takeout_helper_gphotos::metadata::load_metadata(&photo_json_file).unwrap();
    let photo_pair =
        MediaMetadataPair::WithMetadata(photo_file.clone(), Box::new(photo_metadata), None);

    // Create a MediaMetadataPair for the video (without metadata)
    let video_pair = MediaMetadataPair::WithoutMetadata(video_file.clone());

    // Create live_photo_dates map with the photo's date
    // Keyed by (parent directory, lower-cased stem) so identical stems in
    // different album folders cannot collide.
    let mut live_photo_dates = takeout_helper_gphotos::organizer::LivePhotoDates::new();
    let photo_date = chrono::DateTime::from_timestamp(1609459200, 0).unwrap();
    live_photo_dates.insert(
        takeout_helper_gphotos::organizer::live_photo_key(&photo_file),
        photo_date,
    );

    // Extract dates for both files
    let (extracted_photo_path, extracted_photo_date) =
        extract_photo_date(photo_pair, &live_photo_dates).unwrap();
    let (extracted_video_path, extracted_video_date) =
        extract_photo_date(video_pair, &live_photo_dates).unwrap();

    // Verify that both files get the same date
    assert_eq!(extracted_photo_path, photo_file);
    assert_eq!(extracted_video_path, video_file);
    assert_eq!(extracted_photo_date, extracted_video_date);
    assert_eq!(
        extracted_photo_date.known().unwrap().timestamp(),
        1609459200
    );
}

/// #3: two threads copying different files that share a name must both survive.
#[test]
fn test_concurrent_same_name_different_content_both_survive() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();

    // 16 distinct files, all called IMG_0001.jpg, in 16 distinct directories.
    let mut sources = Vec::new();
    for i in 0..16 {
        let dir = src_dir.path().join(format!("album{}", i));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("IMG_0001.jpg");
        std::fs::write(&file, format!("unique content {}", i)).unwrap();
        sources.push(file);
    }

    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = sources
            .iter()
            .map(|src| scope.spawn(|| copy_file_to_organized_location(src, out_dir.path())))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for r in &results {
        let outcome = r.as_ref().unwrap();
        assert!(!outcome.duplicate, "distinct content must not be deduped");
    }

    // All 16 destinations are distinct, and every source's bytes are present.
    let destinations: std::collections::HashSet<_> = results
        .iter()
        .map(|r| r.as_ref().unwrap().destination.clone())
        .collect();
    assert_eq!(destinations.len(), 16, "each file must claim its own name");

    let mut contents: Vec<String> = std::fs::read_dir(out_dir.path())
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    contents.sort();
    let mut expected: Vec<String> = (0..16).map(|i| format!("unique content {}", i)).collect();
    expected.sort();
    assert_eq!(contents, expected, "no photo may be silently overwritten");
}

/// #95: the same photo emitted in "Photos from YYYY" and in an album folder is
/// copied once, not twice.
#[test]
fn test_identical_content_duplicate_is_skipped() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();

    let a_dir = src_dir.path().join("Photos from 2021");
    let b_dir = src_dir.path().join("Holiday album");
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();

    let a = a_dir.join("IMG_1234.jpg");
    let b = b_dir.join("IMG_1234.jpg");
    std::fs::write(&a, "identical bytes").unwrap();
    std::fs::write(&b, "identical bytes").unwrap();

    let first = copy_file_to_organized_location(&a, out_dir.path()).unwrap();
    assert!(!first.duplicate);

    let second = copy_file_to_organized_location(&b, out_dir.path()).unwrap();
    assert!(second.duplicate, "identical content must be skipped");
    assert_eq!(second.destination, first.destination);

    let entries: Vec<_> = std::fs::read_dir(out_dir.path()).unwrap().collect();
    assert_eq!(entries.len(), 1, "no _1 copy may be created");
}

/// Re-running on an incremental export must not duplicate what is already there.
#[test]
fn test_rerun_is_idempotent() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();

    let media_file = src_dir.path().join("photo.jpg");
    std::fs::write(&media_file, "photo content").unwrap();
    let json_file = src_dir.path().join("photo.jpg.json");
    std::fs::write(
        &json_file,
        r#"{"photoTakenTime": {"timestamp": "1609459200"}}"#,
    )
    .unwrap();
    let metadata = takeout_helper_gphotos::metadata::load_metadata(&json_file).unwrap();

    let pair =
        || MediaMetadataPair::WithMetadata(media_file.clone(), Box::new(metadata.clone()), None);

    let first = organize_single_file(pair(), out_dir.path(), &HashMap::new()).unwrap();
    assert!(!first.duplicate());

    let second = organize_single_file(pair(), out_dir.path(), &HashMap::new()).unwrap();
    assert!(second.duplicate(), "a second run must skip, not duplicate");
    assert_eq!(second.destination, first.destination);

    let month_dir = out_dir.path().join("2021").join("01");
    assert_eq!(std::fs::read_dir(&month_dir).unwrap().count(), 1);
}

/// #18: undated files go to unknown-date/, never to the current month.
#[test]
fn test_undated_file_routes_to_unknown_date() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();

    // Freshly created => "now-ish" mtime => no meaningful date.
    let media_file = src_dir.path().join("undated.jpg");
    std::fs::write(&media_file, "undated content").unwrap();

    let pair = MediaMetadataPair::WithoutMetadata(media_file.clone());
    let outcome = organize_single_file(pair, out_dir.path(), &HashMap::new()).unwrap();

    assert_eq!(outcome.date, PhotoDate::Unknown);
    assert_eq!(
        outcome.destination,
        out_dir.path().join("unknown-date").join("undated.jpg")
    );
    assert!(outcome.destination.exists());

    // The current month directory must not have been created.
    let current_year = chrono::Utc::now().format("%Y").to_string();
    assert!(
        !out_dir.path().join(current_year).exists(),
        "undated media must not pollute the current month"
    );
}

/// #19: the resolved date must reach the output copy's mtime.
#[test]
fn test_mtime_is_propagated_to_output_copy() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();

    let media_file = src_dir.path().join("photo.jpg");
    std::fs::write(&media_file, "photo content").unwrap();
    let json_file = src_dir.path().join("photo.jpg.json");
    std::fs::write(
        &json_file,
        r#"{"photoTakenTime": {"timestamp": "1609459200"}}"#,
    )
    .unwrap();
    let metadata = takeout_helper_gphotos::metadata::load_metadata(&json_file).unwrap();
    let pair = MediaMetadataPair::WithMetadata(media_file, Box::new(metadata), None);

    let outcome = organize_single_file(pair, out_dir.path(), &HashMap::new()).unwrap();

    let meta = std::fs::metadata(&outcome.destination).unwrap();
    let mtime = filetime::FileTime::from_last_modification_time(&meta);
    assert_eq!(
        mtime.unix_seconds(),
        1609459200,
        "output copy must carry the photo's date"
    );
}

/// The batch entry point reports organized / duplicates / undated / failures.
#[test]
fn test_organize_media_files_summary() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();

    let json_file = src_dir.path().join("photo.jpg.json");
    std::fs::write(
        &json_file,
        r#"{"photoTakenTime": {"timestamp": "1609459200"}}"#,
    )
    .unwrap();
    let metadata = takeout_helper_gphotos::metadata::load_metadata(&json_file).unwrap();

    // Two byte-identical copies of the same photo in different album folders.
    let mut pairs = Vec::new();
    for album in ["Photos from 2021", "Album"] {
        let dir = src_dir.path().join(album);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("photo.jpg");
        std::fs::write(&file, "photo content").unwrap();
        pairs.push(MediaMetadataPair::WithMetadata(
            file,
            Box::new(metadata.clone()),
            None,
        ));
    }

    // One undated file.
    let undated = src_dir.path().join("undated.jpg");
    std::fs::write(&undated, "undated").unwrap();
    pairs.push(MediaMetadataPair::WithoutMetadata(undated.clone()));

    // One missing file -> failure.
    let missing = src_dir.path().join("missing.jpg");
    pairs.push(MediaMetadataPair::WithoutMetadata(missing.clone()));

    let summary = organize_media_files(pairs, out_dir.path(), &HashMap::new()).unwrap();

    assert_eq!(summary.organized, 2, "one dated + one undated copy");
    assert_eq!(summary.duplicates_skipped, 1);
    assert_eq!(summary.unknown_date, 1);
    assert_eq!(summary.failures.len(), 1);
    assert_eq!(summary.failures[0].0, missing);
    assert_eq!(summary.destinations.len(), 3);
    assert!(summary.destinations.iter().any(|(src, _)| src == &undated));
}

/// Stress the parallel path: many byte-identical copies of the same photo
/// (Takeout emits one per album) must collapse to exactly one output file.
#[test]
fn test_parallel_identical_copies_collapse_to_one() {
    let src_dir = tempfile::tempdir().unwrap();
    let out_dir = tempfile::tempdir().unwrap();

    let json_file = src_dir.path().join("photo.jpg.json");
    std::fs::write(
        &json_file,
        r#"{"photoTakenTime": {"timestamp": "1609459200"}}"#,
    )
    .unwrap();
    let metadata = takeout_helper_gphotos::metadata::load_metadata(&json_file).unwrap();

    const N: usize = 32;
    let mut pairs = Vec::new();
    for i in 0..N {
        let dir = src_dir.path().join(format!("album{}", i));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("IMG_0001.jpg");
        std::fs::write(&file, "identical bytes for every album").unwrap();
        pairs.push(MediaMetadataPair::WithMetadata(
            file,
            Box::new(metadata.clone()),
            None,
        ));
    }

    let summary = organize_media_files(pairs, out_dir.path(), &HashMap::new()).unwrap();

    assert_eq!(summary.organized, 1);
    assert_eq!(summary.duplicates_skipped, N - 1);
    assert!(summary.failures.is_empty());
    assert_eq!(summary.destinations.len(), N);

    let month_dir = out_dir.path().join("2021").join("01");
    assert_eq!(std::fs::read_dir(&month_dir).unwrap().count(), 1);
}

// ---------------------------------------------------------------------------
// Organization strategies
// ---------------------------------------------------------------------------

/// Build `<root>/Takeout/Google Photos/<folder>/<name>` holding `content`.
fn takeout_file(
    root: &std::path::Path,
    folder: &str,
    name: &str,
    content: &str,
) -> std::path::PathBuf {
    let dir = root.join("Takeout").join("Google Photos").join(folder);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    path
}

/// Write a sidecar carrying `photoTakenTime` and return the pair for it.
fn dated_pair(media: &std::path::Path, timestamp: &str) -> MediaMetadataPair {
    let json = media.with_file_name(format!(
        "{}.supplemental-metadata.json",
        media.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(
        &json,
        format!(r#"{{"photoTakenTime": {{"timestamp": "{}"}}}}"#, timestamp),
    )
    .unwrap();
    let metadata = takeout_helper_gphotos::metadata::load_metadata(&json).unwrap();
    MediaMetadataPair::WithMetadata(media.to_path_buf(), Box::new(metadata), Some(json))
}

fn organize(
    pairs: Vec<MediaMetadataPair>,
    out: &std::path::Path,
    options: &OrganizeOptions<'_>,
) -> OrganizeSummary {
    organize_media_files_with_options(pairs, out, &HashMap::new(), options).unwrap()
}

#[test]
fn test_organize_mode_parse() {
    assert_eq!(OrganizeMode::parse("date"), Some(OrganizeMode::Date));
    assert_eq!(OrganizeMode::parse("ALBUM"), Some(OrganizeMode::Album));
    assert_eq!(OrganizeMode::parse("flat"), Some(OrganizeMode::Flat));
    assert_eq!(
        OrganizeMode::parse("date-album"),
        Some(OrganizeMode::DateAlbum)
    );
    assert_eq!(
        OrganizeMode::parse("datealbum"),
        Some(OrganizeMode::DateAlbum)
    );
    assert_eq!(OrganizeMode::parse("nonsense"), None);
    assert_eq!(OrganizeMode::default(), OrganizeMode::Date);
    assert_eq!(OrganizeMode::DateAlbum.as_str(), "date-album");
}

/// Album names are user data: separators, dots and reserved names must not
/// escape the output directory or produce an unusable path.
#[test]
fn test_sanitize_album_name() {
    assert_eq!(
        sanitize_album_name("Holiday 2021").as_deref(),
        Some("Holiday 2021")
    );
    assert_eq!(
        sanitize_album_name("../../etc").as_deref(),
        Some("_.._etc"),
        "path separators and leading dots must not survive"
    );
    assert_eq!(sanitize_album_name("a/b").as_deref(), Some("a_b"));
    assert_eq!(sanitize_album_name("a\\b").as_deref(), Some("a_b"));
    assert_eq!(sanitize_album_name(".hidden").as_deref(), Some("hidden"));
    assert_eq!(
        sanitize_album_name("trailing. ").as_deref(),
        Some("trailing")
    );
    assert_eq!(
        sanitize_album_name("with\ttab").as_deref(),
        Some("with_tab")
    );

    // Nothing usable left.
    assert_eq!(sanitize_album_name(""), None);
    assert_eq!(sanitize_album_name("..."), None);
    assert_eq!(sanitize_album_name("   "), None);

    // Names that would collide with the layout the tool itself builds.
    assert_eq!(sanitize_album_name("2021").as_deref(), Some("2021_album"));
    assert_eq!(
        sanitize_album_name("unknown-date").as_deref(),
        Some("unknown-date_album")
    );
    assert_eq!(sanitize_album_name("CON").as_deref(), Some("CON_album"));
    assert_eq!(
        sanitize_album_name("nul.txt").as_deref(),
        Some("nul.txt_album")
    );

    // Long names are truncated, not rejected.
    assert_eq!(sanitize_album_name(&"x".repeat(400)).unwrap().len(), 100);
}

/// `album` mode files album members under their album and everything else
/// under the date layout. Most of a takeout has no album at all.
#[test]
fn test_album_mode_falls_back_to_dates() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    let in_album = takeout_file(src.path(), "Holiday 2021", "IMG_0001.jpg", "album bytes");
    let no_album = takeout_file(
        src.path(),
        "Photos from 2021",
        "IMG_0002.jpg",
        "loose bytes",
    );

    let options = OrganizeOptions {
        mode: OrganizeMode::Album,
        extract_root: Some(src.path()),
        ..Default::default()
    };
    let summary = organize(
        vec![
            dated_pair(&in_album, "1609459200"),
            dated_pair(&no_album, "1609459200"),
        ],
        out.path(),
        &options,
    );

    assert_eq!(summary.organized, 2);
    assert!(
        out.path()
            .join("Holiday 2021")
            .join("IMG_0001.jpg")
            .exists()
    );
    assert!(
        out.path()
            .join("2021")
            .join("01")
            .join("IMG_0002.jpg")
            .exists()
    );
    assert!(
        !out.path().join("Photos from 2021").exists(),
        "Google's own chronological bucket is not a user album"
    );
}

#[test]
fn test_flat_mode_puts_everything_in_the_root() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    let a = takeout_file(src.path(), "Holiday", "IMG_0001.jpg", "one");
    let b = takeout_file(src.path(), "Photos from 2021", "IMG_0002.jpg", "two");

    let options = OrganizeOptions {
        mode: OrganizeMode::Flat,
        extract_root: Some(src.path()),
        ..Default::default()
    };
    let summary = organize(
        vec![dated_pair(&a, "1609459200"), dated_pair(&b, "1609459200")],
        out.path(),
        &options,
    );

    assert_eq!(summary.organized, 2);
    assert!(out.path().join("IMG_0001.jpg").exists());
    assert!(out.path().join("IMG_0002.jpg").exists());
    assert!(!out.path().join("2021").exists());
}

/// `date-album` keeps the chronology *and* the album: the primary copy is
/// filed by date and the album folder gets a second copy.
#[test]
fn test_date_album_mode_makes_both_copies() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    let in_album = takeout_file(src.path(), "Holiday 2021", "IMG_0001.jpg", "album bytes");
    let no_album = takeout_file(
        src.path(),
        "Photos from 2021",
        "IMG_0002.jpg",
        "loose bytes",
    );

    let options = OrganizeOptions {
        mode: OrganizeMode::DateAlbum,
        extract_root: Some(src.path()),
        ..Default::default()
    };
    let summary = organize(
        vec![
            dated_pair(&in_album, "1609459200"),
            dated_pair(&no_album, "1609459200"),
        ],
        out.path(),
        &options,
    );

    assert_eq!(summary.organized, 2);
    assert_eq!(summary.album_copies, 1);

    let month = out.path().join("2021").join("01");
    assert!(
        month.join("IMG_0001.jpg").exists(),
        "chronology is preserved"
    );
    assert!(month.join("IMG_0002.jpg").exists());
    assert!(
        out.path()
            .join("Holiday 2021")
            .join("IMG_0001.jpg")
            .exists(),
        "the album member also appears under its album"
    );
    assert!(!out.path().join("Photos from 2021").exists());
}

/// Without an extraction root nothing has an album, so the album modes must
/// degrade to the date layout rather than inventing folder names.
#[test]
fn test_album_mode_without_extract_root_uses_dates() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let file = takeout_file(src.path(), "Holiday", "IMG_0001.jpg", "bytes");

    let options = OrganizeOptions {
        mode: OrganizeMode::Album,
        extract_root: None,
        ..Default::default()
    };
    let summary = organize(vec![dated_pair(&file, "1609459200")], out.path(), &options);

    assert_eq!(summary.organized, 1);
    assert!(
        out.path()
            .join("2021")
            .join("01")
            .join("IMG_0001.jpg")
            .exists()
    );
}

/// With dedup off, the same photo emitted in two album folders is copied
/// twice, but the second copy still gets its own name instead of overwriting.
#[test]
fn test_no_dedup_keeps_every_copy() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    let a = takeout_file(src.path(), "Photos from 2021", "IMG_0001.jpg", "identical");
    let b = takeout_file(src.path(), "Holiday", "IMG_0001.jpg", "identical");

    let options = OrganizeOptions {
        dedup: false,
        ..Default::default()
    };
    let summary = organize(
        vec![dated_pair(&a, "1609459200"), dated_pair(&b, "1609459200")],
        out.path(),
        &options,
    );

    assert_eq!(summary.organized, 2);
    assert_eq!(summary.duplicates_skipped, 0);

    let month = out.path().join("2021").join("01");
    assert!(month.join("IMG_0001.jpg").exists());
    assert!(month.join("IMG_0001_1.jpg").exists());
    assert_eq!(std::fs::read_dir(&month).unwrap().count(), 2);
}

/// The polarity matters: keeping everything is the default, because the test
/// is a name match and a photo the user called `sunset-pano.jpg` is theirs.
#[test]
fn test_derivatives_are_kept_by_default_and_skipped_on_request() {
    let src = tempfile::tempdir().unwrap();

    let original = takeout_file(src.path(), "Photos from 2021", "IMG_0001.jpg", "original");
    let edited = takeout_file(
        src.path(),
        "Photos from 2021",
        "IMG_0001-edited.jpg",
        "edited",
    );
    let user_pano = takeout_file(src.path(), "Photos from 2021", "sunset-pano.jpg", "mine");

    let pairs = || {
        vec![
            dated_pair(&original, "1609459200"),
            dated_pair(&edited, "1609459200"),
            dated_pair(&user_pano, "1609459200"),
        ]
    };

    // Default: everything is kept.
    let keep_out = tempfile::tempdir().unwrap();
    let summary = organize(pairs(), keep_out.path(), &OrganizeOptions::default());
    assert_eq!(summary.organized, 3);
    assert_eq!(summary.derivatives_skipped, 0);

    // Opt in: leave the derivatives, including the false positive, behind.
    let skip_out = tempfile::tempdir().unwrap();
    let options = OrganizeOptions {
        skip_derivatives: true,
        ..Default::default()
    };
    let summary = organize(pairs(), skip_out.path(), &options);
    assert_eq!(summary.organized, 1);
    assert_eq!(summary.derivatives_skipped, 2);
    assert_eq!(summary.derivatives.len(), 2);

    let month = skip_out.path().join("2021").join("01");
    assert!(month.join("IMG_0001.jpg").exists());
    assert!(!month.join("IMG_0001-edited.jpg").exists());
    assert!(!month.join("sunset-pano.jpg").exists());
}

#[test]
fn test_copy_sidecars_keeps_the_google_suffix() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    let media = takeout_file(src.path(), "Photos from 2021", "IMG_0001.jpg", "photo");

    let options = OrganizeOptions {
        copy_sidecars: true,
        ..Default::default()
    };
    let summary = organize(vec![dated_pair(&media, "1609459200")], out.path(), &options);

    assert_eq!(summary.sidecars_copied, 1);
    let month = out.path().join("2021").join("01");
    assert!(month.join("IMG_0001.jpg").exists());
    assert!(
        month
            .join("IMG_0001.jpg.supplemental-metadata.json")
            .exists()
    );
}

/// When the collision loop renames the media file, the sidecar must follow it
/// so the two stay paired.
#[test]
fn test_copy_sidecars_follows_a_renamed_media_file() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    // Two *different* photos that share a name, in two folders.
    let a = takeout_file(
        src.path(),
        "Photos from 2021",
        "IMG_0001.jpg",
        "first photo",
    );
    let b = takeout_file(src.path(), "Holiday", "IMG_0001.jpg", "second photo");

    let options = OrganizeOptions {
        copy_sidecars: true,
        ..Default::default()
    };
    let summary = organize(
        vec![dated_pair(&a, "1609459200"), dated_pair(&b, "1609459200")],
        out.path(),
        &options,
    );

    assert_eq!(summary.organized, 2);
    assert_eq!(summary.sidecars_copied, 2);

    let month = out.path().join("2021").join("01");
    assert!(
        month
            .join("IMG_0001.jpg.supplemental-metadata.json")
            .exists()
    );
    assert!(
        month
            .join("IMG_0001_1.jpg.supplemental-metadata.json")
            .exists(),
        "the renamed copy needs a sidecar named after its final file name"
    );
    assert!(month.join("IMG_0001_1.jpg").exists());
}

/// A sidecar that cannot be copied is a warning, never an organize failure:
/// the photo is what matters.
#[test]
fn test_sidecar_copy_failure_is_a_warning() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    let media = takeout_file(src.path(), "Photos from 2021", "IMG_0001.jpg", "photo");
    let pair = dated_pair(&media, "1609459200");
    // Delete the sidecar after the pair captured its path.
    std::fs::remove_file(pair.json_path().unwrap()).unwrap();

    let options = OrganizeOptions {
        copy_sidecars: true,
        ..Default::default()
    };
    let summary = organize(vec![pair], out.path(), &options);

    assert_eq!(summary.organized, 1);
    assert!(summary.failures.is_empty(), "the photo itself succeeded");
    assert_eq!(summary.warnings.len(), 1);
    assert!(
        out.path()
            .join("2021")
            .join("01")
            .join("IMG_0001.jpg")
            .exists()
    );
}

/// Turn the records a run produced into a manifest, the way `app::run` does.
fn manifest_from(summary: &OrganizeSummary, output_dir: &Path) -> Manifest {
    let mut manifest = Manifest::default();
    for (hash, destination) in &summary.records {
        manifest
            .record(output_dir, hash.clone(), destination)
            .unwrap();
    }
    manifest
}

#[test]
fn test_manifest_resume_skips_already_organized_content() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    let media = takeout_file(
        src.path(),
        "Photos from 2021",
        "IMG_0001.jpg",
        "photo bytes",
    );

    // First pass: organize and collect the manifest records.
    let options = OrganizeOptions {
        record_manifest: true,
        ..Default::default()
    };
    let first = organize(vec![dated_pair(&media, "1609459200")], out.path(), &options);
    assert_eq!(first.organized, 1);
    assert_eq!(first.records.len(), 1);

    let manifest = manifest_from(&first, out.path());

    // Second pass with the manifest: skipped without resolving the date at all.
    let resume = OrganizeOptions {
        record_manifest: true,
        resume: Some(&manifest),
        ..Default::default()
    };
    let second = organize(vec![dated_pair(&media, "1609459200")], out.path(), &resume);
    assert_eq!(second.resumed_skips, 1);
    assert_eq!(second.organized, 0);
    assert_eq!(second.duplicates_skipped, 0);

    // Without the manifest (`--force`) the file is reprocessed; the copy already
    // in place makes it a duplicate rather than a second copy.
    let forced = organize(vec![dated_pair(&media, "1609459200")], out.path(), &options);
    assert_eq!(forced.resumed_skips, 0);
    assert_eq!(forced.duplicates_skipped, 1);
    assert_eq!(forced.organized, 0);
}

/// A manifest entry whose output file has been deleted must not skip the file.
#[test]
fn test_manifest_resume_reprocesses_a_deleted_output() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let media = takeout_file(
        src.path(),
        "Photos from 2021",
        "IMG_0001.jpg",
        "photo bytes",
    );

    let options = OrganizeOptions {
        record_manifest: true,
        ..Default::default()
    };
    let manifest = manifest_from(
        &organize(vec![dated_pair(&media, "1609459200")], out.path(), &options),
        out.path(),
    );

    let destination = out.path().join("2021").join("01").join("IMG_0001.jpg");
    std::fs::remove_file(&destination).unwrap();

    let resume = OrganizeOptions {
        record_manifest: true,
        resume: Some(&manifest),
        ..Default::default()
    };
    let second = organize(vec![dated_pair(&media, "1609459200")], out.path(), &resume);
    assert_eq!(second.resumed_skips, 0);
    assert_eq!(second.organized, 1);
    assert!(destination.exists(), "the deleted photo is put back");
}

#[test]
fn test_dry_run_writes_nothing_but_reports_what_it_would_do() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();

    let a = takeout_file(src.path(), "Photos from 2021", "IMG_0001.jpg", "identical");
    let b = takeout_file(src.path(), "Holiday", "IMG_0001.jpg", "identical");
    let c = takeout_file(src.path(), "Holiday", "IMG_0002.jpg", "different");

    let options = OrganizeOptions {
        dry_run: true,
        extract_root: Some(src.path()),
        ..Default::default()
    };
    let summary = organize(
        vec![
            dated_pair(&a, "1609459200"),
            dated_pair(&b, "1609459200"),
            dated_pair(&c, "1609459200"),
        ],
        out.path(),
        &options,
    );

    // Two distinct photos would be copied; the third is byte-identical to the
    // first and would be skipped.
    assert_eq!(summary.planned, 2);
    assert_eq!(summary.planned_duplicates, 1);
    assert_eq!(summary.organized, 0);
    assert_eq!(summary.duplicates_skipped, 0);
    assert!(summary.failures.is_empty());

    assert_eq!(
        std::fs::read_dir(out.path()).unwrap().count(),
        0,
        "a dry run must not create a single file or directory"
    );
}

/// A dry run projects the sidecar copies and still writes nothing.
#[test]
fn test_dry_run_projects_sidecar_copies() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let media = takeout_file(src.path(), "Photos from 2021", "IMG_0001.jpg", "photo");

    let options = OrganizeOptions {
        dry_run: true,
        copy_sidecars: true,
        ..Default::default()
    };
    let summary = organize(vec![dated_pair(&media, "1609459200")], out.path(), &options);

    assert_eq!(summary.planned, 1);
    assert_eq!(summary.sidecars_copied, 1);
    assert!(summary.warnings.is_empty());
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
}

/// A single-file dry run reports the destination it would have used.
#[test]
fn test_dry_run_reports_the_planned_destination() {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let media = takeout_file(src.path(), "Holiday", "IMG_0001.jpg", "photo");

    let context = OrganizeContext::new();
    let options = OrganizeOptions {
        dry_run: true,
        mode: OrganizeMode::Album,
        extract_root: Some(src.path()),
        ..Default::default()
    };
    let outcome = organize_one(
        dated_pair(&media, "1609459200"),
        out.path(),
        &HashMap::new(),
        &context,
        &options,
    )
    .unwrap();

    assert_eq!(outcome.disposition, Disposition::Planned);
    assert_eq!(
        outcome.destination,
        out.path().join("Holiday").join("IMG_0001.jpg")
    );
    assert!(!outcome.destination.exists());
}

/// The sidecar naming rule, in isolation.
#[test]
fn test_sidecar_destination_name_rules() {
    // The common case: keep Google's suffix, rebased onto the final name.
    assert_eq!(
        sidecar_destination_name(
            "IMG_0001.jpg.supplemental-metadata.json",
            "IMG_0001.jpg",
            "IMG_0001_1.jpg"
        ),
        "IMG_0001_1.jpg.supplemental-metadata.json"
    );
    assert_eq!(
        sidecar_destination_name("IMG_0001.jpg.json", "IMG_0001.jpg", "IMG_0001.jpg"),
        "IMG_0001.jpg.json"
    );
    // Truncated / counter-shuffled names fall back to `<final name>.json`.
    assert_eq!(
        sidecar_destination_name("IMG_0001.jpg(1).json", "IMG_0001(1).jpg", "IMG_0001(1).jpg"),
        "IMG_0001(1).jpg.json"
    );
    // An `-edited` copy inherits the base photo's sidecar but still gets its own.
    assert_eq!(
        sidecar_destination_name(
            "IMG_0001.jpg.supplemental-metadata.json",
            "IMG_0001-edited.jpg",
            "IMG_0001-edited.jpg"
        ),
        "IMG_0001-edited.jpg.json"
    );
}
