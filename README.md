<p align="center">
  <img src="assets/takeout-helper-gphotos.png" width="128" height="128" alt="Takeout Helper for Google Photos icon">
</p>

<h1 align="center">Takeout Helper for Google Photos</h1>

<p align="center">
  Turn Google Photos Takeout archives into a clean, dated, metadata-corrected photo library.
</p>

<p align="center">
  <a href="https://github.com/shoon/takeout-helper-gphotos/actions/workflows/ci.yml"><img src="https://github.com/shoon/takeout-helper-gphotos/actions/workflows/ci.yml/badge.svg" alt="CI status"></a>
  <a href="https://github.com/shoon/takeout-helper-gphotos/actions/workflows/audit.yml"><img src="https://github.com/shoon/takeout-helper-gphotos/actions/workflows/audit.yml/badge.svg" alt="Dependency audit status"></a>
  <a href="https://github.com/shoon/takeout-helper-gphotos/releases/latest"><img src="https://img.shields.io/github/v/release/shoon/takeout-helper-gphotos?display_name=tag&amp;sort=semver" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-67ac09" alt="Apache 2.0 license"></a>
  <a href="https://github.com/sponsors/shoon"><img src="https://img.shields.io/badge/Sponsor-shoon-EA4AAA?logo=githubsponsors&amp;logoColor=white" alt="Sponsor shoon on GitHub"></a>
</p>

Takeout Helper for Google Photos is a cross-platform command-line tool that
processes Google Photos exports without requiring ExifTool or uploading your
library anywhere. It finds `.zip`, `.tgz`, and `.tar.gz` Takeout shards,
extracts them within configured path and size limits, and pairs each photo or
video with its Google JSON **sidecar**. A sidecar is a small companion file with
details such as capture time, location, and description. The tool restores that
metadata and organizes the result into predictable folders.

Your downloaded archives are never modified. Existing output files are never
overwritten, and a dry run lets you inspect the plan before committing to a
large migration.

## Download

Download the latest build from [GitHub Releases](https://github.com/shoon/takeout-helper-gphotos/releases/latest).

| Platform | Release archive |
| --- | --- |
| Windows x64 | `takeout-helper-gphotos-vX.Y.Z-windows-x64.zip` |
| Windows ARM64 | `takeout-helper-gphotos-vX.Y.Z-windows-arm64.zip` |
| Linux x64 | `takeout-helper-gphotos-vX.Y.Z-linux-x64.tar.gz` |
| Linux ARM64 | `takeout-helper-gphotos-vX.Y.Z-linux-arm64.tar.gz` |
| macOS Intel | `takeout-helper-gphotos-vX.Y.Z-macos-x64.tar.gz` |
| macOS Apple Silicon | `takeout-helper-gphotos-vX.Y.Z-macos-arm64.tar.gz` |

Every release includes SHA-256 checksums, this README, the Apache license,
project notices, third-party license texts, and the full documentation set.
The binaries are not currently code signed; verify `SHA256SUMS.txt` before use.

## Five-minute start

1. In Google Takeout, create an export containing Google Photos. Download every
   Takeout archive part into one directory, such as `takeout-downloads/`.
2. Download the `takeout-helper-gphotos` release for your operating system from
   [GitHub Releases](https://github.com/shoon/takeout-helper-gphotos/releases/latest).
3. Extract the downloaded **takeout-helper-gphotos release** (`.zip` or
   `.tar.gz`) into a tools directory. Leave the Google Takeout archives as they
   were downloaded. The application extracts them during processing.
4. Keep the Takeout downloads as your source backup and choose a separate,
   empty directory for the organized photo library.
5. Run a dry run first:

```bash
takeout-helper-gphotos --dry-run \
  --input ./takeout-downloads \
  --output ./organized-photos
```

6. Review the summary, confirm that every numbered Takeout shard is present,
   then run the real migration with verification:

```bash
takeout-helper-gphotos --verify \
  --input ./takeout-downloads \
  --output ./organized-photos
```

On Windows PowerShell, invoke the executable as
`.\takeout-helper-gphotos.exe`; the flags are identical.

Large exports need substantial temporary space. By default, extraction uses
`<output>/temp`; plan for roughly twice the uncompressed library size on that
volume, or use `--temp-dir` to place scratch data elsewhere.

See [Getting started](docs/getting-started.md) for export preparation,
installation, checksum verification, disk planning, and platform-specific
commands.

## What it does

- Extracts `.zip`, `.tgz`, and `.tar.gz` exports with path, entry-count,
  per-file, total-size, and actual-written-byte limits.
- Warns before extraction when a numbered Takeout download set has missing
  parts.
- Matches modern, legacy, truncated, localized edited-copy, and Live Photo
  sidecar naming patterns.
- Writes capture time, UTC offset, GPS coordinates, altitude, and description
  into supported image formats using pure Rust.
- Patches QuickTime `mvhd` dates in MP4, MOV, and M4V files through an atomic,
  verified temporary copy.
- Organizes by `YYYY/MM`, album, a flat directory, or dated folders plus album
  copies.
- Uses BLAKE3 content hashes to skip exact duplicates and resume interrupted
  runs.
- Optionally copies JSON sidecars, skips Google-generated derivatives, and
  re-hashes the finished library.
- Reports totals, writes problems to CSV, and returns a nonzero exit code when
  files need attention.
- Shows each processing stage and per-file work for long-running metadata,
  organization, and verification steps.
- Stops cleanly on Ctrl+C while preserving the resume manifest and report.

## Default output

The default `date` layout looks like this:

```text
organized-photos/
├── 2019/
│   └── 07/
│       ├── IMG_1001.jpg
│       └── VID_1002.mp4
├── 2024/
│   └── 12/
│       └── IMG_9000.heic
├── unknown-date/
│   └── scanned-photo.jpg
├── .gphotos-manifest.json
└── takeout-helper-report.csv   # only when something needs attention
```

The current date is never invented for an undated file. If neither a sidecar,
Live Photo match, nor a plausible preserved filesystem timestamp can supply a
date, the file goes to `unknown-date/` for review.

## Common recipes

```bash
# Preserve chronology and also create album copies
takeout-helper-gphotos --organize date-album \
  --input ./takeout --output ./photos

# Keep JSON sidecars next to their media files
takeout-helper-gphotos --copy-sidecars \
  --input ./takeout --output ./photos

# Search nested download folders and use another disk for scratch space
takeout-helper-gphotos --recursive --temp-dir /mnt/scratch \
  --input ./downloads --output ./photos

# Force one timezone instead of GPS-based per-photo timezone resolution
takeout-helper-gphotos --timezone America/New_York \
  --input ./takeout --output ./photos
```

For details about every flag, see the [CLI reference](docs/cli-reference.md)
and [usage recipes](docs/usage.md).

## Safety model

The tool treats source data conservatively:

- source archives are read-only inputs;
- archive paths are normalized and confined to a uniquely named scratch tree;
- symlinks, hard links, devices, FIFOs, traversal paths, and absolute paths are
  not materialized;
- EXIF and video changes happen to extracted copies, then use verified atomic
  replacement;
- output names are claimed atomically and existing files are never overwritten;
- cross-filesystem copies are verified before the temporary source is removed;
- resume data is written atomically and verification failures return a nonzero
  exit status;
- oversized sidecar JSON is rejected before it can cause an unbounded memory
  allocation.

These safeguards do not replace backups. Keep the Takeout
archives until you have inspected and independently backed up the finished
library. Read [Safety and recovery](docs/safety-and-recovery.md) before a large
run and use [private vulnerability reporting](SECURITY.md) for security issues.

## Documentation

| Guide | Covers |
| --- | --- |
| [Documentation home](docs/index.md) | Guide map and recommended reading order |
| [Getting started](docs/getting-started.md) | Export preparation, installation, checksums, first run |
| [Usage recipes](docs/usage.md) | Common migration strategies and worked commands |
| [Organization and resume](docs/organization-and-resume.md) | Layouts, duplicates, albums, manifest, sidecars |
| [Metadata and formats](docs/metadata-and-formats.md) | Date precedence, EXIF, video dates, supported media |
| [Safety and recovery](docs/safety-and-recovery.md) | Threat model, limits, backups, interruption recovery |
| [Troubleshooting](docs/troubleshooting.md) | Missing shards, disk space, unknown dates, OS warnings |
| [CLI reference](docs/cli-reference.md) | Every option, reports, and exit codes |
| [Development](docs/development.md) | Architecture, tests, dependency policy, releases |

## Build from source

Install Rust 1.88 or newer, then run:

```bash
git clone https://github.com/shoon/takeout-helper-gphotos.git
cd takeout-helper-gphotos
cargo build --locked --release
```

The executable is written to `target/release/takeout-helper-gphotos` (with
`.exe` on Windows). Development checks and the rationale for the locally patched
EXIF dependency are documented in [Development](docs/development.md).

## Support development

Takeout Helper for Google Photos is free and open source. If it helps you
recover or organize an important photo library, consider
[sponsoring @shoon on GitHub](https://github.com/sponsors/shoon). Sponsorship
helps cover test systems, code signing, storage, and the time required to
maintain this project and other practical utilities.

## Trademarks and independence

This is an independent open-source project. It is not affiliated with,
authorized, sponsored, approved, or endorsed by Google LLC. Google Photos,
Google Takeout, and Google are trademarks of Google LLC. See
[TRADEMARKS.md](TRADEMARKS.md).

## License

Copyright 2026 Shaun Murphy

Licensed under the [Apache License 2.0](LICENSE). The software is distributed
on an **AS IS** basis, without warranties or conditions of any kind. See
[NOTICE](NOTICE), [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt), and
[TRADEMARKS.md](TRADEMARKS.md) for attribution and non-affiliation information.
