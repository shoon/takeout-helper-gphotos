// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use std::io::Write;
use std::path::Path;
use takeout_helper_gphotos::archive::{self, ArchiveError};
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

/// Build a zip archive at `path` from `(name, contents)` pairs.
fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, contents) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
}

/// Build a `.tar.gz`/`.tgz` archive at `path` from `(name, contents, mtime)`.
fn write_tgz(path: &Path, entries: &[(&str, &[u8], u64)]) {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::Builder;

    let file = std::fs::File::create(path).unwrap();
    let gz_encoder = GzEncoder::new(file, Compression::default());
    let mut tar_builder = Builder::new(gz_encoder);

    for (name, contents, mtime) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(*mtime);
        header.set_cksum();
        tar_builder.append(&header, &contents[..]).unwrap();
    }
    tar_builder.finish().unwrap();
}

#[test]
fn test_find_archive_files() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    for name in [
        "archive1.zip",
        "archive2.ZIP",
        "archive3.zip",
        "archive4.tar.tgz",
        "archive5.TAR.TGZ",
        "archive6.tar.tgz",
        "takeout-20251111T183213Z-1-001.tgz",
        "google-photos-backup.tgz",
    ] {
        std::fs::File::create(temp_path.join(name)).unwrap();
    }

    // Non-archives that must be ignored.
    for name in ["readme.txt", "just.tar", "just.gz"] {
        std::fs::File::create(temp_path.join(name)).unwrap();
    }

    let archive_files = archive::find_archive_files(temp_path, true).unwrap();
    assert_eq!(archive_files.len(), 8);
}

/// `.tar.gz` must be discovered: `Path::extension()` reports only `"gz"`.
#[test]
fn test_find_archive_files_discovers_tar_gz() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    for name in [
        "takeout-20260101T000000Z-001.tar.gz",
        "TAKEOUT-UPPER.TAR.GZ",
        "shard.tgz",
        "shard.zip",
    ] {
        std::fs::File::create(temp_path.join(name)).unwrap();
    }
    for name in ["notes.gz", "bundle.tar", "readme.txt"] {
        std::fs::File::create(temp_path.join(name)).unwrap();
    }

    let found = archive::find_archive_files(temp_path, true).unwrap();
    let names: Vec<String> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();

    assert_eq!(found.len(), 4, "found: {:?}", names);
    assert!(names.contains(&"takeout-20260101T000000Z-001.tar.gz".to_string()));
    assert!(names.contains(&"TAKEOUT-UPPER.TAR.GZ".to_string()));
    assert!(!names.contains(&"notes.gz".to_string()));
    assert!(!names.contains(&"bundle.tar".to_string()));
}

#[test]
fn test_find_zip_files_only() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    for name in ["archive1.zip", "archive2.ZIP", "archive3.zip"] {
        std::fs::File::create(temp_path.join(name)).unwrap();
    }
    std::fs::File::create(temp_path.join("readme.txt")).unwrap();

    let zip_files = archive::find_archive_files(temp_path, true).unwrap();
    assert_eq!(zip_files.len(), 3);

    for file in zip_files {
        assert_eq!(
            file.extension()
                .unwrap_or_default()
                .to_str()
                .unwrap()
                .to_lowercase(),
            "zip"
        );
    }
}

#[test]
fn test_create_temp_directory() {
    let temp_dir = archive::create_temp_directory().unwrap();

    assert!(temp_dir.exists());
    assert!(
        temp_dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(archive::TEMP_DIR_PREFIX)
    );

    let path = temp_dir.path().to_path_buf();
    drop(temp_dir);
    assert!(!path.exists());
}

/// `TempDir::create_inside` must never touch the base directory the user named.
#[test]
fn test_temp_dir_create_inside_preserves_base() {
    let base = TempDir::new().unwrap();
    let base_path = base.path().to_path_buf();

    // Pretend the user pointed --temp-dir at a directory holding real data.
    let precious = base_path.join("my-only-copy.zip");
    std::fs::write(&precious, b"original").unwrap();

    let scratch = archive::TempDir::create_inside(&base_path).unwrap();
    let scratch_path = scratch.path().to_path_buf();

    assert!(scratch_path.starts_with(&base_path));
    assert_ne!(scratch_path, base_path);
    std::fs::write(scratch_path.join("extracted.bin"), b"work").unwrap();

    drop(scratch);

    assert!(!scratch_path.exists(), "the scratch subdir should be gone");
    assert!(base_path.exists(), "the user's directory must survive");
    assert!(precious.exists(), "the user's data must survive");
}

/// Two successive `create_inside` calls must not collide.
#[test]
fn test_temp_dir_create_inside_is_unique() {
    let base = TempDir::new().unwrap();
    let a = archive::TempDir::create_inside(base.path()).unwrap();
    let b = archive::TempDir::create_inside(base.path()).unwrap();
    assert_ne!(a.path(), b.path());
}

#[test]
fn test_extract_single_archive() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("test.zip");
    write_zip(&archive_path, &[("test.txt", b"test content")]);

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    let summary =
        archive::extract_single_archive(&archive_path, &extract_dir, None, None, None).unwrap();

    assert_eq!(summary.files_extracted, 1);
    assert_eq!(summary.bytes_written, b"test content".len() as u64);

    let extracted_file = extract_dir.join("test.txt");
    assert!(extracted_file.exists());
    assert_eq!(
        std::fs::read_to_string(&extracted_file).unwrap(),
        "test content"
    );
}

/// Regression for the "every archive after the first aborts" bug: the same
/// extraction directory must keep working once the subtree already exists, even
/// when the caller passes a relative path.
#[test]
fn test_extract_multiple_archives_into_same_dir() {
    let temp_dir = TempDir::new().unwrap();
    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    let mut archives = Vec::new();
    for i in 0..3 {
        let path = temp_dir.path().join(format!("shard{}.zip", i));
        let name = format!("Takeout/Google Photos/IMG_{}.jpg", i);
        write_zip(&path, &[(name.as_str(), b"jpegdata")]);
        archives.push(path);
    }

    let results = archive::extract_archives(archives, &extract_dir, None, None, None);
    assert_eq!(results.len(), 3);
    for (path, result) in &results {
        assert!(result.is_ok(), "{} failed: {:?}", path.display(), result);
    }

    for i in 0..3 {
        assert!(
            extract_dir
                .join(format!("Takeout/Google Photos/IMG_{}.jpg", i))
                .exists()
        );
    }
}

/// The old default capped archives at 10,000 entries. Real Takeout shards go
/// well past that, so an archive with more entries must extract fine.
#[test]
fn test_extract_archive_with_more_than_old_file_count_limit() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("many.zip");

    let entry_count = 10_050;
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
        for i in 0..entry_count {
            zip.start_file(format!("f{}.txt", i), SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();
    }

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    let summary =
        archive::extract_single_archive(&archive_path, &extract_dir, None, None, None).unwrap();
    assert_eq!(summary.files_extracted, entry_count);
}

/// `max_archive_size` bounds *bytes*, never the entry count. Passing a small
/// byte budget must not be reinterpreted as a file-count budget (and vice
/// versa).
#[test]
fn test_max_archive_size_is_not_a_file_count() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("five.zip");
    write_zip(
        &archive_path,
        &[
            ("a.txt", b"a"),
            ("b.txt", b"b"),
            ("c.txt", b"c"),
            ("d.txt", b"d"),
            ("e.txt", b"e"),
        ],
    );

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    // 5 entries, 5 bytes total. A byte budget of 3 must fail on *bytes*...
    let result = archive::extract_single_archive(&archive_path, &extract_dir, None, Some(3), None);
    assert!(matches!(result, Err(ArchiveError::LimitExceeded(_))));

    // ...while a byte budget of 3 with an entry cap of 5 is fine when the byte
    // budget is generous.
    let extract_dir2 = temp_dir.path().join("extracted2");
    std::fs::create_dir(&extract_dir2).unwrap();
    let summary =
        archive::extract_single_archive(&archive_path, &extract_dir2, None, Some(1000), Some(5))
            .unwrap();
    assert_eq!(summary.files_extracted, 5);

    // An explicit entry cap below the entry count is what rejects by count.
    let extract_dir3 = temp_dir.path().join("extracted3");
    std::fs::create_dir(&extract_dir3).unwrap();
    let result =
        archive::extract_single_archive(&archive_path, &extract_dir3, None, Some(1000), Some(2));
    assert!(matches!(result, Err(ArchiveError::LimitExceeded(_))));
}

/// One oversized entry must be skipped and counted, not abort the archive.
#[test]
fn test_oversized_entry_is_skipped_not_fatal() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("mixed.zip");
    write_zip(
        &archive_path,
        &[
            ("small_before.txt", b"ok"),
            ("huge_video.mp4", &[7u8; 4096]),
            ("small_after.txt", b"ok"),
        ],
    );

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    // Per-file cap of 1 KiB: only the 4 KiB entry violates it.
    let summary =
        archive::extract_single_archive(&archive_path, &extract_dir, Some(1024), None, None)
            .unwrap();

    assert_eq!(summary.skipped_oversize, 1);
    assert_eq!(summary.files_extracted, 2);
    assert!(extract_dir.join("small_before.txt").exists());
    assert!(extract_dir.join("small_after.txt").exists());
    assert!(
        !extract_dir.join("huge_video.mp4").exists(),
        "oversized entry must not be left on disk"
    );
}

/// A `..` traversal entry is rejected while `a..b.json` file names pass.
#[test]
fn test_traversal_entry_rejected_but_dotdot_filenames_pass() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("evil.zip");
    write_zip(
        &archive_path,
        &[
            ("../escaped.txt", b"pwned"),
            ("Takeout/a..b.json", b"{}"),
            ("Takeout/Weird Album../photo..json", b"{}"),
            ("Takeout/normal.jpg", b"jpg"),
        ],
    );

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    let summary =
        archive::extract_single_archive(&archive_path, &extract_dir, None, None, None).unwrap();

    assert_eq!(summary.skipped_unsafe, 1, "the ../ entry must be skipped");
    assert_eq!(summary.files_extracted, 3);

    assert!(!temp_dir.path().join("escaped.txt").exists());
    assert!(extract_dir.join("Takeout/a..b.json").exists());
    assert!(
        extract_dir
            .join("Takeout/Weird Album../photo..json")
            .exists()
    );
    assert!(extract_dir.join("Takeout/normal.jpg").exists());
}

/// Zip entry modification times must survive extraction.
#[test]
fn test_zip_entry_mtime_is_preserved() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("dated.zip");

    let dt = zip::DateTime::from_date_and_time(2015, 6, 24, 10, 30, 0).unwrap();
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
        zip.start_file(
            "IMG_0001.jpg",
            SimpleFileOptions::default().last_modified_time(dt),
        )
        .unwrap();
        zip.write_all(b"jpegdata").unwrap();
        zip.finish().unwrap();
    }

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();
    archive::extract_single_archive(&archive_path, &extract_dir, None, None, None).unwrap();

    let extracted = extract_dir.join("IMG_0001.jpg");
    let mtime =
        filetime::FileTime::from_last_modification_time(&std::fs::metadata(&extracted).unwrap());

    let expected = chrono::NaiveDate::from_ymd_opt(2015, 6, 24)
        .unwrap()
        .and_hms_opt(10, 30, 0)
        .unwrap()
        .and_utc()
        .timestamp();

    assert_eq!(
        mtime.unix_seconds(),
        expected,
        "extracted file should carry the zip entry's timestamp, not 'now'"
    );
}

/// Tar entry modification times must survive extraction.
#[test]
fn test_tar_entry_mtime_is_preserved() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("dated.tar.gz");

    let mtime_secs = 1_435_141_800u64; // 2015-06-24T10:30:00Z
    write_tgz(&archive_path, &[("IMG_0002.jpg", b"jpegdata", mtime_secs)]);

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();
    archive::extract_single_tgz_archive(&archive_path, &extract_dir, None, None, None).unwrap();

    let extracted = extract_dir.join("IMG_0002.jpg");
    let mtime =
        filetime::FileTime::from_last_modification_time(&std::fs::metadata(&extracted).unwrap());
    assert_eq!(mtime.unix_seconds(), mtime_secs as i64);
}

#[test]
fn test_extract_archives_reports_per_archive_results() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    let good = temp_path.join("good.zip");
    write_zip(&good, &[("ok.txt", b"fine")]);

    let bad = temp_path.join("bad.zip");
    std::fs::write(&bad, "fake zip content").unwrap();

    let extract_dir = TempDir::new().unwrap();

    let results = archive::extract_archives(
        vec![good.clone(), bad.clone()],
        extract_dir.path(),
        None,
        None,
        None,
    );

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, good);
    assert!(results[0].1.is_ok());
    assert_eq!(results[1].0, bad);
    assert!(
        results[1].1.is_err(),
        "a corrupt archive must be reported as failed, not swallowed"
    );
}

#[test]
fn test_extract_single_tgz_archive() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("test.tar.tgz");
    write_tgz(&archive_path, &[("test.txt", b"test content", 0)]);

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    let summary =
        archive::extract_single_tgz_archive(&archive_path, &extract_dir, None, None, None).unwrap();
    assert_eq!(summary.files_extracted, 1);

    let extracted_file = extract_dir.join("test.txt");
    assert!(extracted_file.exists());
    assert_eq!(
        std::fs::read_to_string(&extracted_file).unwrap(),
        "test content"
    );
}

/// The tgz path must skip an oversized entry and keep going, in a single pass.
#[test]
fn test_tgz_oversized_entry_is_skipped_not_fatal() {
    let temp_dir = TempDir::new().unwrap();
    let archive_path = temp_dir.path().join("mixed.tar.gz");
    let big = vec![9u8; 4096];
    write_tgz(
        &archive_path,
        &[
            ("a.txt", b"ok", 0),
            ("huge.mp4", big.as_slice(), 0),
            ("b.txt", b"ok", 0),
        ],
    );

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    let summary =
        archive::extract_single_tgz_archive(&archive_path, &extract_dir, Some(1024), None, None)
            .unwrap();

    assert_eq!(summary.skipped_oversize, 1);
    assert_eq!(summary.files_extracted, 2);
    assert!(extract_dir.join("a.txt").exists());
    assert!(extract_dir.join("b.txt").exists());
    assert!(!extract_dir.join("huge.mp4").exists());
}

#[test]
fn test_extract_single_archive_auto() {
    let temp_dir = TempDir::new().unwrap();
    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    let zip_path = temp_dir.path().join("test.zip");
    write_zip(&zip_path, &[("test.txt", b"zip content")]);

    assert!(
        archive::extract_single_archive_auto(&zip_path, &extract_dir, None, None, None).is_ok()
    );
    let zip_extracted_file = extract_dir.join("test.txt");
    assert_eq!(
        std::fs::read_to_string(&zip_extracted_file).unwrap(),
        "zip content"
    );
    std::fs::remove_file(&zip_extracted_file).unwrap();

    // `.tar.gz` must route to the tgz extractor, not be rejected as `.gz`.
    let tgz_path = temp_dir.path().join("test.tar.gz");
    write_tgz(&tgz_path, &[("test.txt", b"tgz content", 0)]);

    assert!(
        archive::extract_single_archive_auto(&tgz_path, &extract_dir, None, None, None).is_ok()
    );
    assert_eq!(
        std::fs::read_to_string(extract_dir.join("test.txt")).unwrap(),
        "tgz content"
    );
}

/// Discovery plus split detection, over real files on disk: a Takeout download
/// that is missing part 002 must be reported as incomplete, because those
/// photos will silently not appear in the library.
#[test]
fn test_split_archives_detected_from_a_real_directory() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    for name in [
        "takeout-20240101T000000Z-001.zip",
        "takeout-20240101T000000Z-003.zip",
        "takeout-20240101T000000Z-004.zip",
        "unrelated.zip",
    ] {
        write_zip(&temp_path.join(name), &[("Takeout/a.jpg", b"jpg")]);
    }

    let found = archive::find_archive_files(temp_path, false).unwrap();
    assert_eq!(found.len(), 4);

    let groups = archive::detect_split_archives(&found);
    assert_eq!(groups.len(), 1, "only the numbered sequence is a group");
    assert_eq!(groups[0].base_name, "takeout-20240101T000000Z");
    assert_eq!(groups[0].part_numbers, vec![1, 3, 4]);
    assert!(groups[0].has_gaps());
    assert_eq!(groups[0].missing, vec![2]);

    // Advisory only: every part that *is* present is still there to extract.
    assert_eq!(groups[0].parts.len(), 3);

    // Reporting must not panic on a group with gaps.
    archive::report_split_archives(&groups);
}

/// A complete sequence must not be reported as having gaps.
#[test]
fn test_complete_split_archive_set_has_no_gaps() {
    let temp_dir = TempDir::new().unwrap();
    let temp_path = temp_dir.path();

    for name in [
        "takeout-20240101T000000Z-001.tgz",
        "takeout-20240101T000000Z-002.tgz",
    ] {
        write_tgz(&temp_path.join(name), &[("Takeout/a.jpg", b"jpg", 0)]);
    }

    let found = archive::find_archive_files(temp_path, false).unwrap();
    let groups = archive::detect_split_archives(&found);
    assert_eq!(groups.len(), 1);
    assert!(!groups[0].has_gaps());
    assert!(groups[0].missing.is_empty());
}

#[test]
fn test_extract_corrupt_tgz() {
    let temp_dir = TempDir::new().unwrap();

    let corrupt_tgz_path = temp_dir.path().join("corrupt.tar.tgz");
    std::fs::write(&corrupt_tgz_path, "This is not a valid TGZ file content").unwrap();

    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir(&extract_dir).unwrap();

    let result =
        archive::extract_single_tgz_archive(&corrupt_tgz_path, &extract_dir, None, None, None);
    assert!(result.is_err());

    let extracted_files: Vec<_> = std::fs::read_dir(&extract_dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .collect();
    assert_eq!(extracted_files.len(), 0);
}
