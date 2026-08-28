// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use std::path::{Path, PathBuf};
use takeout_helper_gphotos::metadata;
use takeout_helper_gphotos::stats::ProcessingStats;
use tempfile::TempDir;

/// Create an empty media file and return its path.
fn touch(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::File::create(&path).unwrap();
    path
}

/// Write a sidecar with a recognisable title.
fn sidecar(dir: &Path, name: &str, title: &str) {
    std::fs::write(
        dir.join(name),
        format!(
            r#"{{"title": "{}", "photoTakenTime": {{"timestamp": "1609459200"}}}}"#,
            title
        ),
    )
    .unwrap();
}

fn title_of(pair: &metadata::MediaMetadataPair) -> Option<String> {
    match pair {
        metadata::MediaMetadataPair::WithMetadata(_, m, ..) => m.title.clone(),
        metadata::MediaMetadataPair::WithoutMetadata(_) => None,
    }
}

/// Every sidecar form Google Takeout actually emits, in one directory.
#[test]
fn test_realistic_takeout_pairing() {
    let temp_dir = TempDir::new().unwrap();
    let album = temp_dir.path().join("Photos from 2019");
    std::fs::create_dir_all(&album).unwrap();

    // 1. modern supplemental-metadata sidecar
    let img1 = touch(&album, "IMG_0001.jpg");
    sidecar(&album, "IMG_0001.jpg.supplemental-metadata.json", "one");

    // 2. legacy sidecar with no supplemental infix
    let img2 = touch(&album, "IMG_0002.jpg");
    sidecar(&album, "IMG_0002.jpg.json", "two");

    // 3. truncated sidecar: the cut lands inside the media basename
    let long1 = touch(&album, "averylongscreenshotname_something.jpg");
    sidecar(
        &album,
        "averylongscreenshotname_somethi.jpg.supplemental-met.json",
        "long-basename-cut",
    );

    // 4. truncated sidecar: 46 characters total, the cut removed the extension
    //    and the whole suffix.
    let long2 = touch(&album, "verylongphotoname_that_is_quite_long_here.jpeg");
    let cut = "verylongphotoname_that_is_quite_long_here.json";
    assert_eq!(cut.len(), 46, "Google truncates sidecar names at 46 chars");
    sidecar(&album, cut, "long-ext-cut");

    // 5. duplicate counter, which Google moves after the extension
    let img3 = touch(&album, "IMG_0003.jpg");
    sidecar(&album, "IMG_0003.jpg.supplemental-metadata.json", "three");
    let img3_dup = touch(&album, "IMG_0003(1).jpg");
    sidecar(
        &album,
        "IMG_0003.jpg.supplemental-metadata(1).json",
        "three-dup",
    );

    // 6. `-edited` copy: no sidecar of its own, inherits the base photo's
    let img4 = touch(&album, "IMG_0004.jpg");
    sidecar(&album, "IMG_0004.jpg.supplemental-metadata.json", "four");
    let img4_edited = touch(&album, "IMG_0004-edited.jpg");

    // 7. Live Photo: uppercase media, lowercase sidecar, video inherits
    let img5 = touch(&album, "IMG_0005.HEIC");
    sidecar(&album, "img_0005.heic.supplemental-metadata.json", "five");
    let img5_mov = touch(&album, "IMG_0005.MOV");

    // 8. a name with consecutive dots
    let dotted = touch(&album, "photo..jpg");
    sidecar(&album, "photo..jpg.json", "dotted");

    // 9. album-level housekeeping JSON must never be treated as a sidecar
    sidecar(&album, "metadata.json", "album");
    sidecar(&album, "print-subscriptions.json", "print");

    // 10. an orphan sidecar with no media file at all
    sidecar(&album, "IMG_9999.jpg.supplemental-metadata.json", "orphan");

    let media = vec![
        img1.clone(),
        img2.clone(),
        long1.clone(),
        long2.clone(),
        img3.clone(),
        img3_dup.clone(),
        img4.clone(),
        img4_edited.clone(),
        img5.clone(),
        img5_mov.clone(),
        dotted.clone(),
    ];

    let mut stats = ProcessingStats::default();
    let pairs = metadata::pair_media_with_metadata(media.clone(), &mut stats).unwrap();

    assert_eq!(pairs.len(), media.len());

    let expected = [
        "one",
        "two",
        "long-basename-cut",
        "long-ext-cut",
        "three",
        "three-dup",
        "four",
        "four", // -edited inherits
        "five",
        "five", // Live Photo video inherits
        "dotted",
    ];

    for (pair, want) in pairs.iter().zip(expected.iter()) {
        let path = match pair {
            metadata::MediaMetadataPair::WithMetadata(p, ..) => p,
            metadata::MediaMetadataPair::WithoutMetadata(p) => p,
        }
        .clone();
        assert_eq!(
            title_of(pair).as_deref(),
            Some(*want),
            "wrong metadata for {}",
            path.display()
        );
    }

    // Nine media files own a sidecar; two inherit one.
    assert_eq!(stats.metadata_json_files_found, 9);
    assert_eq!(stats.files_without_metadata, 0);
    // Housekeeping JSONs are excluded; only IMG_9999 is a true orphan.
    assert_eq!(stats.orphan_sidecars, 1);
}

#[test]
fn test_find_media_files_expanded_allowlist() {
    let temp_dir = TempDir::new().unwrap();
    let dir = temp_dir.path();

    let media_names = [
        "a.jpg", "a.JPEG", "a.png", "a.heic", "a.heif", "a.avif", "a.gif", "a.webp", "a.bmp",
        "a.tif", "a.tiff", "a.dng", "a.cr2", "a.cr3", "a.nef", "a.arw", "a.orf", "a.rw2", "a.raf",
        "a.mp4", "a.MOV", "a.m4v", "a.3gp", "a.3g2", "a.avi", "a.mkv", "a.wmv", "a.mpg", "a.mpeg",
        "a.mts", "a.m2ts", "a.mp",
    ];
    for name in media_names {
        touch(dir, name);
    }
    touch(dir, "readme.txt");
    touch(dir, "archive_browser.html");
    touch(dir, "a.jpg.supplemental-metadata.json");

    let mut stats = ProcessingStats::default();
    let found = metadata::find_media_files_with_stats(dir, &mut stats).unwrap();

    assert_eq!(found.len(), media_names.len());
    // The two non-media files are counted, the JSON sidecar is not.
    assert_eq!(stats.files_skipped_extension, 2);
}

#[test]
fn test_localized_edited_suffix_inherits() {
    let temp_dir = TempDir::new().unwrap();
    let dir = temp_dir.path();

    let base = touch(dir, "IMG_1000.jpg");
    sidecar(dir, "IMG_1000.jpg.supplemental-metadata.json", "base");
    let edited = touch(dir, "IMG_1000-bearbeitet.jpg");

    let mut stats = ProcessingStats::default();
    let pairs = metadata::pair_media_with_metadata(vec![base, edited], &mut stats).unwrap();

    assert_eq!(title_of(&pairs[0]).as_deref(), Some("base"));
    assert_eq!(title_of(&pairs[1]).as_deref(), Some("base"));
    assert_eq!(stats.metadata_json_files_found, 1);
    assert_eq!(stats.orphan_sidecars, 0);
}

#[test]
fn test_bad_geo_data_degrades_instead_of_discarding() {
    let temp_dir = TempDir::new().unwrap();
    let dir = temp_dir.path();

    let photo = touch(dir, "IMG_2000.jpg");
    std::fs::write(
        dir.join("IMG_2000.jpg.supplemental-metadata.json"),
        r#"{"title":"kept","photoTakenTime":{"timestamp":"1609459200"},
            "geoData":{"latitude":999.0,"longitude":-74.0}}"#,
    )
    .unwrap();

    let mut stats = ProcessingStats::default();
    let pairs = metadata::pair_media_with_metadata(vec![photo], &mut stats).unwrap();

    match &pairs[0] {
        metadata::MediaMetadataPair::WithMetadata(_, m, ..) => {
            assert_eq!(m.title.as_deref(), Some("kept"));
            assert!(m.photo_taken_time.is_some());
            let geo = m.geo_data.as_ref().unwrap();
            assert!(geo.latitude.is_none(), "out-of-range latitude dropped");
            assert_eq!(geo.longitude, Some(-74.0), "valid longitude kept");
        }
        metadata::MediaMetadataPair::WithoutMetadata(_) => {
            panic!("a single bad field must not discard the whole sidecar")
        }
    }
    assert_eq!(stats.files_without_metadata, 0);
}

#[test]
fn test_find_media_files() {
    // Create a temporary directory with test media files
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    // Create test media files
    let _photo1 = temp_path.join("photo1.jpg");
    let _photo2 = temp_path.join("photo2.png");
    let _photo3 = temp_path.join("photo3.HEIC");
    let _video1 = temp_path.join("video1.mp4");
    let _video2 = temp_path.join("video2.MOV");
    std::fs::File::create(&_photo1).unwrap();
    std::fs::File::create(&_photo2).unwrap();
    std::fs::File::create(&_photo3).unwrap();
    std::fs::File::create(&_video1).unwrap();
    std::fs::File::create(&_video2).unwrap();

    // Create a non-media file to make sure it's not included
    let _text_file = temp_path.join("readme.txt");
    std::fs::File::create(&_text_file).unwrap();

    // Run the function
    let media_files = metadata::find_media_files(temp_path).unwrap();

    // Verify we found the correct number of media files
    assert_eq!(media_files.len(), 5);

    // Verify they are all media files
    for file in media_files {
        let ext = file
            .extension()
            .unwrap_or_default()
            .to_str()
            .unwrap()
            .to_lowercase();
        assert!(["jpg", "jpeg", "png", "heic", "mov", "mp4"].contains(&ext.as_str()));
    }
}

#[test]
fn test_load_metadata() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    // Create a test JSON file with valid metadata
    let json_content = r#"{
        "title": "Test Photo",
        "description": "A test photo",
        "photoTakenTime": {
            "timestamp": "1609459200"
        },
        "geoData": {
            "latitude": 40.7128,
            "longitude": -74.0060
        }
    }"#;

    let json_file = temp_path.join("photo.json");
    std::fs::write(&json_file, json_content).unwrap();

    // Run the function
    let metadata = metadata::load_metadata(&json_file).unwrap();

    // Verify the metadata was loaded correctly
    assert_eq!(metadata.title, Some("Test Photo".to_string()));
    assert_eq!(metadata.description, Some("A test photo".to_string()));
    assert!(metadata.photo_taken_time.is_some());
}

#[test]
fn test_load_metadata_invalid_json() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    // Create a test JSON file with invalid metadata
    let json_content = r#"{
        "title": "Test Photo",
        "description": "A test photo",
        "photoTakenTime": {
            "timestamp": "invalid"
        }
    }"#;

    let json_file = temp_path.join("photo.jpg.json");
    std::fs::write(&json_file, json_content).unwrap();

    // Run the function - should succeed since serde is permissive
    let result = metadata::load_metadata(&json_file);

    // Verify that we succeeded in parsing the JSON
    assert!(result.is_ok());

    // Let's examine what we actually got
    let metadata = result.unwrap();
    // photo_taken_time should be Some since it exists in the JSON
    assert!(metadata.photo_taken_time.is_some());
    // But timestamp within photo_taken_time might be None due to parsing failure
    // This is actually expected behavior - the field exists but has invalid data
    let photo_taken_time = metadata.photo_taken_time.unwrap();
    // We can't assert that timestamp is None because serde will just ignore invalid values
    // and set the field to its default (None for Option<String>)
    println!("Photo taken time: {:?}", photo_taken_time); // This is for debugging
}

#[test]
fn test_pair_media_with_metadata() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    // Create test media files
    let photo_file = temp_path.join("photo.jpg");
    std::fs::File::create(&photo_file).unwrap();

    // Create matching metadata file with correct Google Takeout naming convention
    // For photo.jpg, the metadata file should be photo.jpg.supplemental-metadata.json
    let json_content = r#"{"title": "Test Photo"}"#;
    let json_file = temp_path.join("photo.jpg.supplemental-metadata.json");
    std::fs::write(&json_file, json_content).unwrap();

    // Create an unmatched media file
    let unmatched_photo = temp_path.join("unmatched.jpg");
    std::fs::File::create(&unmatched_photo).unwrap();

    // Create a statistics object for the test
    let mut stats = ProcessingStats::default();

    // Run the function
    let pairs =
        metadata::pair_media_with_metadata(vec![photo_file, unmatched_photo], &mut stats).unwrap();

    // Verify we got the correct results
    assert_eq!(pairs.len(), 2);

    // Check that the first pair has metadata and the second doesn't
    match &pairs[0] {
        metadata::MediaMetadataPair::WithMetadata(_, metadata, ..) => {
            assert!(metadata.title.is_some());
        }
        metadata::MediaMetadataPair::WithoutMetadata(_) => {
            panic!("Expected pair with metadata");
        }
    }

    match &pairs[1] {
        metadata::MediaMetadataPair::WithMetadata(..) => {
            panic!("Expected pair without metadata");
        }
        metadata::MediaMetadataPair::WithoutMetadata(_) => {
            // This is correct
        }
    }
}

#[test]
fn test_user_files_metadata_pairing() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    // Create the same file structure as mentioned by the user
    // IMG_4595.HEIC with metadata
    let photo_file = temp_path.join("IMG_4595.HEIC");
    std::fs::File::create(&photo_file).unwrap();

    let json_content = r#"{
        "title": "Live Photo",
        "photoTakenTime": {
            "timestamp": "1609459200"
        }
    }"#;
    let json_file = temp_path.join("IMG_4595.HEIC.supplemental-metadata.json");
    std::fs::write(&json_file, json_content).unwrap();

    // IMG_4595.MP4 without its own metadata (should use photo's metadata)
    let video_file = temp_path.join("IMG_4595.MP4");
    std::fs::File::create(&video_file).unwrap();

    // IMG_4597.MOV with its own metadata
    let mov_file = temp_path.join("IMG_4597.MOV");
    std::fs::File::create(&mov_file).unwrap();

    let mov_json_content = r#"{
        "title": "Normal Video",
        "photoTakenTime": {
            "timestamp": "1609559200"
        }
    }"#;
    let mov_json_file = temp_path.join("IMG_4597.MOV.supplemental-metadata.json");
    std::fs::write(&mov_json_file, mov_json_content).unwrap();

    // Screenshot with metadata
    let screenshot_file = temp_path.join("Screenshot 2025-06-20 at 2.35.23 PM.jpeg");
    std::fs::File::create(&screenshot_file).unwrap();

    let screenshot_json_content = r#"{
        "title": "Screenshot",
        "photoTakenTime": {
            "timestamp": "1619459200"
        }
    }"#;
    let screenshot_json_file =
        temp_path.join("Screenshot 2025-06-20 at 2.35.23 PM.jpeg.suppl.json");
    std::fs::write(&screenshot_json_file, screenshot_json_content).unwrap();

    // Create a statistics object for the test
    let mut stats = ProcessingStats::default();

    // Run the function on all files
    let pairs = metadata::pair_media_with_metadata(
        vec![photo_file, video_file, mov_file, screenshot_file],
        &mut stats,
    )
    .unwrap();

    // Verify we got the correct results - all files should have metadata
    assert_eq!(pairs.len(), 4);
    assert_eq!(stats.files_without_metadata, 0);

    // Actually, all files should be found to have metadata:
    // - IMG_4595.HEIC has its own metadata file
    // - IMG_4595.MP4 should use the HEIC's metadata file
    // - IMG_4597.MOV has its own metadata file
    // - Screenshot has its own metadata file
    // The statistics should show:
    // - 3 metadata files found (IMG_4595.HEIC.supplemental-metadata.json, IMG_4597.MOV.supplemental-metadata.json,
    //   and Screenshot 2025-06-20 at 2.35.23 PM.jpeg.suppl.json)
    // - 0 files without metadata (all files should be paired with metadata)
    assert_eq!(stats.metadata_json_files_found, 3);
    assert_eq!(stats.files_without_metadata, 0);

    // Check that all pairs have metadata
    for pair in pairs {
        match pair {
            metadata::MediaMetadataPair::WithMetadata(_, metadata, ..) => {
                assert!(metadata.title.is_some());
            }
            metadata::MediaMetadataPair::WithoutMetadata(_) => {
                panic!("All files should be paired with metadata");
            }
        }
    }
}

#[test]
fn test_pair_media_with_metadata_live_photo() {
    // Create a temporary directory
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    // Create test photo file (HEIC)
    let photo_file = temp_path.join("IMG_4595.HEIC");
    std::fs::File::create(&photo_file).unwrap();

    // Create metadata file for the photo
    let json_content = r#"{
        "title": "Live Photo",
        "photoTakenTime": {
            "timestamp": "1609459200"
        }
    }"#;
    let json_file = temp_path.join("IMG_4595.HEIC.supplemental-metadata.json");
    std::fs::write(&json_file, json_content).unwrap();

    // Create test video file with same base name but no metadata
    let video_file = temp_path.join("IMG_4595.MP4");
    std::fs::File::create(&video_file).unwrap();

    // Create a statistics object for the test
    let mut stats = ProcessingStats::default();

    // Run the function
    let pairs =
        metadata::pair_media_with_metadata(vec![photo_file, video_file], &mut stats).unwrap();

    // Verify we got the correct results - both files should have metadata
    assert_eq!(pairs.len(), 2);
    // Only one JSON file was actually found on disk (for the HEIC photo)
    // The MP4 file inherits metadata from the HEIC file, so it doesn't increment the counter
    assert_eq!(stats.metadata_json_files_found, 1);
    assert_eq!(stats.files_without_metadata, 0);

    // Check that both pairs have metadata
    for pair in pairs {
        match pair {
            metadata::MediaMetadataPair::WithMetadata(_, metadata, ..) => {
                assert!(metadata.title.is_some());
                assert_eq!(metadata.title.as_ref().unwrap(), "Live Photo");
            }
            metadata::MediaMetadataPair::WithoutMetadata(_) => {
                panic!("Expected pair with metadata for photo");
            }
        }
    }
}

/// `--copy-sidecars` needs the sidecar path to survive on the pair; re-deriving
/// it from the media name would have to reproduce Google's truncation, counter
/// and inheritance rules, which is exactly what the pairing pass already did.
#[test]
fn test_pairs_carry_their_sidecar_path() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    let media = touch(temp_path, "IMG_0001.jpg");
    let json = temp_path.join("IMG_0001.jpg.supplemental-metadata.json");
    std::fs::write(&json, r#"{"title": "One"}"#).unwrap();
    touch(temp_path, "IMG_0002.jpg");

    let mut stats = ProcessingStats::default();
    let pairs =
        metadata::pair_media_with_metadata(vec![media, temp_path.join("IMG_0002.jpg")], &mut stats)
            .unwrap();

    let paired = pairs
        .iter()
        .find(|p| p.path().file_name().unwrap() == "IMG_0001.jpg")
        .unwrap();
    assert_eq!(paired.json_path(), Some(json.as_path()));

    let unpaired = pairs
        .iter()
        .find(|p| p.path().file_name().unwrap() == "IMG_0002.jpg")
        .unwrap();
    assert_eq!(unpaired.json_path(), None);
}

/// Album detection: only a real user album counts, and only relative to the
/// extraction root the run actually used.
#[test]
fn test_extract_album_name() {
    let root = Path::new("/scratch/gphotos-takeout-abc");
    let takeout = root.join("Takeout").join("Google Photos");

    assert_eq!(
        metadata::extract_album_name(&takeout.join("Holiday 2021").join("IMG_0001.jpg"), root)
            .as_deref(),
        Some("Holiday 2021")
    );

    // Google's own buckets are not albums.
    for folder in [
        "Photos from 2021",
        "Archive",
        "Trash",
        "Bin",
        "Locked Folder",
        "Failed Videos",
        "Untitled",
    ] {
        assert_eq!(
            metadata::extract_album_name(&takeout.join(folder).join("IMG_0001.jpg"), root),
            None,
            "{} must not become an album",
            folder
        );
    }

    // A file outside the extraction root has no album.
    assert_eq!(
        metadata::extract_album_name(Path::new("/elsewhere/Album/IMG_0001.jpg"), root),
        None
    );
}

/// `--skip-derivatives` is driven entirely by this test, so its edges matter:
/// it must catch Google's suffixes (including localized `-edited`) and it must
/// be understood as a heuristic, which is why skipping is opt-in.
#[test]
fn test_is_derivative() {
    for name in [
        "IMG_0001-edited.jpg",
        "IMG_0001-bearbeitet.jpg",
        "IMG_0001-edited(1).jpg",
        "PANO-effects.jpg",
        "holiday-collage.jpg",
        "clip-animation.gif",
        "trip-movie.mp4",
    ] {
        assert!(metadata::is_derivative(name), "{} is a derivative", name);
    }

    for name in ["IMG_0001.jpg", "IMG_0001(1).jpg", "my-editorial.jpg"] {
        assert!(!metadata::is_derivative(name), "{} is an original", name);
    }

    // The known false positive, and the reason the flag defaults to off: this
    // is a name the user chose, not something Google generated.
    assert!(metadata::is_derivative("sunset-pano.jpg"));
}
