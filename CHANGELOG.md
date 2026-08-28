# Changelog

All notable changes to this project will be documented here. Releases follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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
