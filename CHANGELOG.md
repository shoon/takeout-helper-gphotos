# Changelog

All notable changes to this project will be documented here. Releases follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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

[Unreleased]: https://github.com/shoon/takeout-helper-gphotos/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/shoon/takeout-helper-gphotos/releases/tag/v0.1.0
