// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

//! End-to-end pipeline tests.
//!
//! Each test drives [`takeout_helper_gphotos::app::run`] over a real (tiny)
//! Takeout archive, so the wiring between extraction, pairing, organization,
//! the resume manifest, and verification is exercised the way a
//! user exercises it.
//!
//! The fixtures are `.mp4` files: they take the modification-time path through
//! the EXIF phase rather than the embedded-EXIF path, so a few bytes of dummy
//! content are enough and the assertions below are about organization rather
//! than about whether `little_exif` liked a fake JPEG.

use std::io::Write;
use std::path::{Path, PathBuf};

use takeout_helper_gphotos::MANIFEST_FILE_NAME;
use takeout_helper_gphotos::app::{self, AppConfig, RunOutcome};
use takeout_helper_gphotos::organizer::OrganizeMode;
use tempfile::TempDir;
use zip::write::SimpleFileOptions;

/// 2021-01-01T00:00:00Z, the timestamp every fixture sidecar carries.
const TAKEN_AT: &str = "1609459200";

/// Build a zip archive at `path` from `(name, contents)` pairs.
fn write_zip(path: &Path, entries: &[(String, Vec<u8>)]) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, contents) in entries {
        zip.start_file(name.as_str(), SimpleFileOptions::default())
            .unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
}

/// Build a zip whose entries all carry a chosen archive mtime.
fn write_zip_at(path: &Path, entries: &[(String, Vec<u8>)], modified: zip::DateTime) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, contents) in entries {
        zip.start_file(
            name.as_str(),
            SimpleFileOptions::default().last_modified_time(modified),
        )
        .unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap();
}

/// A media entry plus its `.supplemental-metadata.json` sidecar, under
/// `Takeout/Google Photos/<folder>/`.
fn takeout_entries(folder: &str, name: &str, contents: &str) -> Vec<(String, Vec<u8>)> {
    takeout_entries_at(folder, name, contents, TAKEN_AT)
}

fn takeout_entries_at(
    folder: &str,
    name: &str,
    contents: &str,
    taken_at: &str,
) -> Vec<(String, Vec<u8>)> {
    let base = format!("Takeout/Google Photos/{}/{}", folder, name);
    vec![
        (base.clone(), contents.as_bytes().to_vec()),
        (
            format!("{}.supplemental-metadata.json", base),
            format!(r#"{{"photoTakenTime": {{"timestamp": "{}"}}}}"#, taken_at)
                .as_bytes()
                .to_vec(),
        ),
    ]
}

/// A fixture run: an input directory holding one archive, an output directory
/// and a scratch directory *outside* the output so the
/// output can be asserted empty.
struct Fixture {
    _root: TempDir,
    input: PathBuf,
    output: PathBuf,
    scratch: PathBuf,
}

impl Fixture {
    fn new(entries: Vec<(String, Vec<u8>)>) -> Self {
        let root = TempDir::new().unwrap();
        let input = root.path().join("input");
        let output = root.path().join("output");
        let scratch = root.path().join("scratch");
        for dir in [&input, &output, &scratch] {
            std::fs::create_dir_all(dir).unwrap();
        }
        write_zip(&input.join("takeout-20210101T000000Z-001.zip"), &entries);

        Fixture {
            _root: root,
            input,
            output,
            scratch,
        }
    }

    fn config(&self) -> AppConfig {
        AppConfig {
            input: self.input.clone(),
            output: self.output.clone(),
            temp_dir: Some(self.scratch.clone()),
            ..Default::default()
        }
    }

    fn run(&self, config: AppConfig) -> RunOutcome {
        app::run(config).expect("the pipeline should not abort")
    }

    /// Everything under the output directory, as paths relative to it.
    fn output_tree(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = walkdir::WalkDir::new(&self.output)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().strip_prefix(&self.output).unwrap().to_path_buf())
            .collect();
        found.sort();
        found
    }
}

/// A dry run must extract (it cannot plan otherwise) but write nothing at all
/// into the output directory: no photos, no manifest, no report.
#[test]
fn dry_run_writes_nothing_into_the_output() {
    let mut entries = takeout_entries("Photos from 2021", "IMG_0001.mp4", "photo one");
    entries.extend(takeout_entries("Holiday", "IMG_0002.mp4", "photo two"));
    let fixture = Fixture::new(entries);

    // Every writing feature turned on at once: none of them may write.
    let outcome = fixture.run(AppConfig {
        dry_run: true,
        copy_sidecars: true,
        organize: OrganizeMode::DateAlbum,
        verify: true,
        ..fixture.config()
    });

    let stats = outcome.stats();
    assert!(stats.dry_run);
    assert_eq!(stats.media_files_found, 2);
    assert!(
        !stats.verify_ran,
        "there is no manifest to verify during a dry run"
    );
    assert_eq!(
        stats.planned_organize, 2,
        "the run must say what it would have done"
    );
    assert_eq!(stats.files_organized, 0);
    assert_eq!(stats.exif_written, 0, "no file may be modified");
    assert!(stats.report_path.is_none());
    assert!(stats.manifest_path.is_none());

    assert!(
        fixture.output_tree().is_empty(),
        "a dry run left files behind: {:?}",
        fixture.output_tree()
    );
    assert!(!fixture.output.join(MANIFEST_FILE_NAME).exists());
}

/// The manifest turns a second run into a no-op, and `--force` overrides it.
#[test]
fn a_second_run_resumes_from_the_manifest() {
    let mut entries = takeout_entries("Photos from 2021", "IMG_0001.mp4", "photo one");
    entries.extend(takeout_entries("Holiday", "IMG_0002.mp4", "photo two"));
    let fixture = Fixture::new(entries);

    let first = fixture.run(fixture.config());
    assert_eq!(first.stats().files_organized, 2);
    assert_eq!(first.stats().resumed_skips, 0);
    assert!(fixture.output.join(MANIFEST_FILE_NAME).exists());
    assert_eq!(first.exit_code(), 0);

    let second = fixture.run(fixture.config());
    assert_eq!(
        second.stats().resumed_skips,
        2,
        "both files are already recorded"
    );
    assert_eq!(second.stats().files_organized, 0);
    assert_eq!(second.stats().duplicates_skipped, 0);

    // `--force` ignores the manifest; the copies already in place then make
    // every file a byte-identical duplicate rather than a second copy.
    let forced = fixture.run(AppConfig {
        force: true,
        ..fixture.config()
    });
    assert_eq!(forced.stats().resumed_skips, 0);
    assert_eq!(forced.stats().duplicates_skipped, 2);
    assert_eq!(forced.stats().files_organized, 0);

    assert_eq!(
        fixture
            .output_tree()
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "mp4"))
            .count(),
        2,
        "no run may duplicate the library"
    );
}

/// Shards can repeat the same logical path. Their media and sidecars must stay
/// together so parallel extraction cannot assign one shard's date to another.
#[test]
fn colliding_shards_keep_media_with_their_own_sidecars() {
    let mut first_entries =
        takeout_entries_at("Holiday", "IMG_0001.mp4", "first shard media", "1609459200");
    first_entries.push((
        "Takeout/Google Photos/Holiday/metadata.json".to_string(),
        br#"{"title":"first"}"#.to_vec(),
    ));
    let fixture = Fixture::new(first_entries);

    let mut second_entries = takeout_entries_at(
        "Holiday",
        "IMG_0001.mp4",
        "second shard media",
        "1640995200",
    );
    second_entries.push((
        "Takeout/Google Photos/Holiday/metadata.json".to_string(),
        br#"{"title":"second"}"#.to_vec(),
    ));
    write_zip(
        &fixture.input.join("takeout-20210101T000000Z-002.zip"),
        &second_entries,
    );

    let outcome = fixture.run(fixture.config());
    let stats = outcome.stats();
    assert_eq!(stats.archives_extracted, 2);
    assert_eq!(stats.media_files_found, 2);
    assert_eq!(stats.metadata_json_files_found, 2);
    assert_eq!(stats.orphan_sidecars, 0);
    assert_eq!(
        std::fs::read_to_string(fixture.output.join("2021/01/IMG_0001.mp4")).unwrap(),
        "first shard media"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.output.join("2022/01/IMG_0001.mp4")).unwrap(),
        "second shard media"
    );
}

/// A resumed undated file is still actionable and must keep its report row on
/// a later run instead of triggering stale-report deletion.
#[test]
fn resumed_undated_file_keeps_the_report() {
    let fixture = Fixture::new(Vec::new());
    let media_path = "Takeout/Google Photos/Photos from 2099/UNDATED.mp4";
    let future = zip::DateTime::from_date_and_time(2099, 1, 1, 0, 0, 0).unwrap();
    write_zip_at(
        &fixture.input.join("takeout-20210101T000000Z-001.zip"),
        &[(media_path.to_string(), b"undated media".to_vec())],
        future,
    );

    let first = fixture.run(fixture.config());
    assert_eq!(first.stats().unknown_date, 1);
    let first_report = first.stats().report_path.as_ref().unwrap();
    assert!(
        std::fs::read_to_string(first_report)
            .unwrap()
            .contains("unknown-date")
    );

    let second = fixture.run(fixture.config());
    assert_eq!(second.stats().resumed_skips, 1);
    assert_eq!(second.stats().unknown_date, 1);
    let second_report = second.stats().report_path.as_ref().unwrap();
    assert!(
        std::fs::read_to_string(second_report)
            .unwrap()
            .contains("unknown-date")
    );
}

/// `--verify` re-hashes the library and passes on a clean run.
#[test]
fn verify_passes_on_a_clean_run() {
    let fixture = Fixture::new(takeout_entries(
        "Photos from 2021",
        "IMG_0001.mp4",
        "photo one",
    ));

    let outcome = fixture.run(AppConfig {
        verify: true,
        ..fixture.config()
    });

    let stats = outcome.stats();
    assert!(stats.verify_ran);
    assert_eq!(stats.verified, 1);
    assert_eq!(stats.verify_failures(), 0);
    assert_eq!(outcome.exit_code(), 0);
}

/// Different media paths can legitimately contain the same bytes. Both must
/// remain in the manifest and be verified independently.
#[test]
fn verify_checks_each_path_when_files_have_identical_content() {
    let mut entries = takeout_entries("Photos from 2021", "IMG_0001.mp4", "identical media bytes");
    entries.extend(takeout_entries(
        "Photos from 2021",
        "IMG_0002.mp4",
        "identical media bytes",
    ));
    let fixture = Fixture::new(entries);

    let outcome = fixture.run(AppConfig {
        verify: true,
        ..fixture.config()
    });

    let stats = outcome.stats();
    assert_eq!(stats.files_organized, 2);
    assert_eq!(stats.verified, 2);
    assert_eq!(stats.verify_failures(), 0);
    assert_eq!(outcome.exit_code(), 0);
}

/// Version 1 retained only one path per content hash. A forced pass must reuse
/// existing files while rebuilding complete version 2 verification records.
#[test]
fn force_rebuilds_a_legacy_manifest_with_identical_content_paths() {
    let mut entries = takeout_entries("Photos from 2021", "IMG_0001.mp4", "same bytes");
    entries.extend(takeout_entries(
        "Photos from 2021",
        "IMG_0002.mp4",
        "same bytes",
    ));
    let fixture = Fixture::new(entries);
    assert_eq!(fixture.run(fixture.config()).stats().files_organized, 2);

    let manifest_path = fixture.output.join(MANIFEST_FILE_NAME);
    let current: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let retained = &current["entries"][0];
    let hash = retained["hash"].as_str().unwrap();
    let relative_path = PathBuf::from(retained["path"].as_str().unwrap());
    let mut legacy_entries = serde_json::Map::new();
    legacy_entries.insert(
        hash.to_string(),
        serde_json::json!({
            "hash": hash,
            "output_path": fixture.output.join(relative_path),
            "processed_at": retained["processed_at"]
        }),
    );
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&serde_json::json!({ "entries": legacy_entries })).unwrap(),
    )
    .unwrap();

    let rebuilt = fixture.run(AppConfig {
        force: true,
        verify: true,
        ..fixture.config()
    });

    assert_eq!(rebuilt.stats().duplicates_skipped, 2);
    assert_eq!(rebuilt.stats().verified, 2);
    assert_eq!(rebuilt.exit_code(), 0);

    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
    assert_eq!(saved["version"], 2);
    assert_eq!(saved["entries"].as_array().unwrap().len(), 2);
}

/// A file that changed under us must fail verification, land in the report and
/// change the exit code. Silence here would mean claiming a library is intact
/// when it is not.
#[test]
fn verify_reports_content_that_changed_on_disk() {
    let fixture = Fixture::new(takeout_entries(
        "Photos from 2021",
        "IMG_0001.mp4",
        "photo one",
    ));

    fixture.run(fixture.config());
    let organized = fixture.output.join("2021").join("01").join("IMG_0001.mp4");
    assert!(organized.exists());
    std::fs::write(&organized, "corrupted by a failing disk").unwrap();

    let outcome = fixture.run(AppConfig {
        verify: true,
        ..fixture.config()
    });

    let stats = outcome.stats();
    assert!(stats.verify_ran);
    assert_eq!(stats.verify_mismatched, 1);
    assert_eq!(stats.verify_failures(), 1);
    assert!(stats.has_failures());
    assert_eq!(outcome.exit_code(), 1);

    let report = std::fs::read_to_string(stats.report_path.as_ref().unwrap()).unwrap();
    assert!(report.contains("verify-failed"), "report was: {}", report);
}

/// `--organize date-album` builds both trees from a real archive.
#[test]
fn date_album_layout_builds_both_trees() {
    let mut entries = takeout_entries("Holiday 2021", "IMG_0001.mp4", "album photo");
    entries.extend(takeout_entries("Photos from 2021", "IMG_0002.mp4", "loose"));
    let fixture = Fixture::new(entries);

    let outcome = fixture.run(AppConfig {
        organize: OrganizeMode::DateAlbum,
        ..fixture.config()
    });

    assert_eq!(outcome.stats().files_organized, 2);
    assert_eq!(outcome.stats().album_copies, 1);

    let month = fixture.output.join("2021").join("01");
    assert!(month.join("IMG_0001.mp4").exists());
    assert!(month.join("IMG_0002.mp4").exists());
    assert!(
        fixture
            .output
            .join("Holiday 2021")
            .join("IMG_0001.mp4")
            .exists()
    );
}

/// `--skip-derivatives` leaves Google's generated copies behind and says so in
/// the report.
#[test]
fn skip_derivatives_is_opt_in_and_reported() {
    let mut entries = takeout_entries("Photos from 2021", "IMG_0001.mp4", "original");
    entries.extend(takeout_entries(
        "Photos from 2021",
        "IMG_0001-edited.mp4",
        "edited",
    ));
    let fixture = Fixture::new(entries);

    // Default: nothing is dropped.
    let kept = fixture.run(fixture.config());
    assert_eq!(kept.stats().files_organized, 2);
    assert_eq!(kept.stats().derivatives_skipped, 0);

    // Opt in, on a fresh output directory.
    let fresh = Fixture::new({
        let mut entries = takeout_entries("Photos from 2021", "IMG_0001.mp4", "original");
        entries.extend(takeout_entries(
            "Photos from 2021",
            "IMG_0001-edited.mp4",
            "edited",
        ));
        entries
    });
    let skipped = fresh.run(AppConfig {
        skip_derivatives: true,
        ..fresh.config()
    });

    assert_eq!(skipped.stats().files_organized, 1);
    assert_eq!(skipped.stats().derivatives_skipped, 1);

    let report = std::fs::read_to_string(skipped.stats().report_path.as_ref().unwrap()).unwrap();
    assert!(
        report.contains("derivative-skipped"),
        "report was: {}",
        report
    );
    assert!(
        fresh
            .output
            .join("2021")
            .join("01")
            .join("IMG_0001.mp4")
            .exists()
    );
    assert!(
        !fresh
            .output
            .join("2021")
            .join("01")
            .join("IMG_0001-edited.mp4")
            .exists()
    );
}

/// `--copy-sidecars` puts the JSON next to the organized photo.
#[test]
fn copy_sidecars_places_the_json_next_to_the_photo() {
    let fixture = Fixture::new(takeout_entries(
        "Photos from 2021",
        "IMG_0001.mp4",
        "photo one",
    ));

    let outcome = fixture.run(AppConfig {
        copy_sidecars: true,
        ..fixture.config()
    });

    assert_eq!(outcome.stats().sidecars_copied, 1);
    let month = fixture.output.join("2021").join("01");
    assert!(month.join("IMG_0001.mp4").exists());
    assert!(
        month
            .join("IMG_0001.mp4.supplemental-metadata.json")
            .exists()
    );
}
