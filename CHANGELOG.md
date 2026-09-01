# Changelog

All notable changes to this project will be documented here. Releases follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.1] - 2026-09-01

- Extract parallel Takeout shards into isolated scratch subtrees so repeated
  paths cannot race and every media file stays with the sidecar from its own
  shard; repeated names within one archive are still preserved as numbered
  siblings instead of truncating an earlier entry.
- Atomically account for space other archives have already reserved when
  parallel extractions run their pre-flight disk-space checks.
- Delete a stale `takeout-helper-report.csv` when a later run finishes with
  nothing to report.
- Keep a metadata write counted as successful when the embedded write worked
  and only the follow-up modification-time update failed.
- Share one set of Live Photo extension lists between sidecar pairing, the
  Live Photo date map, and date resolution, so PNG stills and M4V/3GP videos
  are covered everywhere.
- List undated files in the CSV report for every layout and on resumed runs,
  and describe their destinations accurately for date, album, flat, and dry-run
  layouts.
- Report a failed Ctrl+C handler installation as a warning instead of
  aborting, and let a later attempt see the real error.

- Coordinate progress bars, status messages, and logs through one terminal
  renderer so Windows window resizing and warnings do not overwrite live rows.
- Replace graphical bars with compact animated spinners, numbered processing
  steps, task percentages when a total is known, and item counts that remain
  short when a terminal window is resized.
- Use one aggregate extraction row instead of one live row per archive, and
  clear completed detail rows instead of leaving stale cursor positions behind.
- Make `-v` show useful phase-level information, reserve per-file diagnostics
  for `-vv`, and hide animated progress when debug or trace logs are active.

- Show preparation, archive discovery, extraction, media discovery, sidecar
  pairing, Live Photo indexing, metadata writing, organization, verification,
  and finalization in the progress display.
- Show per-file verification progress while the finished library is re-hashed.
- Record every organized media path in the resume manifest, even when multiple
  files have identical content, and store paths relative to the output library.
- Load existing version 1 manifests while writing the corrected version 2
  format.
- Identify orphan sidecars and successful fresh EXIF replacements more clearly
  in warnings and the run summary.

## [0.1.0] - 2026-08-28

Initial public release.

- Extract ZIP, TGZ, and TAR.GZ Google Photos Takeout archives with path and size
  limits.
- Restore image EXIF, filesystem timestamps, and QuickTime video dates from
  JSON sidecars.
- Organize by date, album, flat layout, or date plus album copies.
- Resume interrupted runs, de-duplicate by content, copy sidecars, and verify
  output through a BLAKE3 manifest.
- Add cross-platform release archives, checksums, dependency auditing, security
  policy, focused documentation, and Apache-2.0 licensing.

[Unreleased]: https://github.com/shoon/takeout-helper-gphotos/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/shoon/takeout-helper-gphotos/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/shoon/takeout-helper-gphotos/releases/tag/v0.1.0
