# Development

The project is a Rust 2024 workspace with one library, one CLI binary, integration
tests, and one locally patched vendored dependency.

## Requirements

- Rust 1.88 or newer
- Cargo with the committed lockfile
- `cargo-audit` 0.22.1 for advisory checks
- `cargo-about` 0.9.2 for third-party notices

No external ExifTool, image library, database, or network service is used at
runtime.

## Architecture

| Module | Responsibility |
| --- | --- |
| `main.rs` | CLI parsing, logging, process exit codes |
| `app.rs` | End-to-end phase orchestration and reports |
| `archive.rs` | Discovery, split-set warnings, confined extraction, scratch ownership |
| `metadata.rs` | Media discovery, sidecar indexing, parsing, validation |
| `exif.rs` | Image EXIF, timezone resolution, QuickTime timestamps, atomic replacement |
| `organizer.rs` | Date resolution, layouts, atomic placement, albums, sidecars, hashing |
| `manifest.rs` | Resume state and atomic persistence |
| `dedup.rs` | Shared BLAKE3 identity implementation |
| `verify.rs` | Manifest target verification |
| `stats.rs` | Phase accounting and user-facing summary |

The integration tests exercise real temporary trees and small image/container
fixtures. Test safety behavior at the boundary where it occurs and with unit
tests where appropriate.

## Local checks

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
cargo audit
```

The full suite currently contains 182 tests after the oversized-sidecar and
Windows timestamp regressions are included. Do not hard-code that number in
automation; Cargo's exit status is authoritative.

To verify the minimum supported toolchain:

```bash
rustup toolchain install 1.88.0 --profile minimal
cargo +1.88.0 check --locked --all-targets
```

## Dependency policy

Runtime dependencies are locked and Dependabot checks Cargo and GitHub Actions
weekly. CI denies Clippy warnings. A scheduled RustSec workflow runs
`cargo audit`; release publication repeats the audit.

When dependencies change:

1. update intentionally with `cargo update`;
2. inspect additions, removals, features, licenses, and MSRV impact;
3. run the full test, Clippy, release-build, and audit set;
4. regenerate third-party notices; and
5. review `Cargo.lock`, `about.toml`, and the generated notice together.

Regenerate notices with:

```bash
cargo about generate about-third-party.hbs --locked --offline --fail \
  --output-file THIRD_PARTY_NOTICES.txt
```

`about.toml` covers every release target so target-specific dependencies are not
omitted.

## Vendored `little_exif` patch

The latest published `little_exif` release is 0.6.23. Its upstream dependency
constraint holds `quick-xml` at 0.37, which is affected by
RUSTSEC-2026-0194 and RUSTSEC-2026-0195, and it uses the unmaintained `paste`
crate.

Until upstream publishes a fixed release, `vendor/little_exif` contains the
released source with two dependency-only changes:

- `quick-xml` 0.37.5 to 0.42.0; and
- `paste` 1.0.15 to its maintained successor, `pastey` 0.2.3.

The only source changes are the two macro import paths, a small XMP parser API
adaptation for `quick-xml` 0.42, and rustfmt normalization.
Original Apache and MIT licenses, README, copyright headers, and upstream
repository metadata are retained. Project tests cover JPEG, PNG, HEIC, XMP
preservation paths, timezone tags, malformed-file behavior, and atomic writes.

Remove the vendor only after a published upstream version resolves both
advisories and passes the same tests.

## CI and releases

CI runs formatting, Clippy, tests, and release builds on Linux, Windows, and
macOS, plus a Rust 1.88 compatibility check. Dependency review runs on pull
requests and RustSec auditing runs on dependency changes and weekly.

A tag exactly matching the Cargo package version (`v0.1.0`, for example)
triggers release builds for:

- Linux x64 and ARM64;
- Windows x64 and ARM64; and
- macOS Intel and Apple Silicon.

Each runner builds and tests natively, packages the binary with legal and
documentation files, and uploads an artifact. The publish job combines the
archives, writes `SHA256SUMS.txt`, and creates a GitHub release with generated
notes.

Before tagging:

1. update `Cargo.toml` and `CHANGELOG.md`;
2. regenerate third-party notices;
3. run the complete local check set;
4. confirm `main` CI and the dependency audit pass; and
5. create and push a signed or annotated `vX.Y.Z` tag.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for DCO and data-safety requirements.
