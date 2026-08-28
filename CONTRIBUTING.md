# Contributing

Bug reports and focused pull requests are welcome. This project transforms
personal photo libraries, so changes involving archive extraction, path
handling, metadata writes, file placement, manifests, or cleanup require
careful review and regression tests.

## Before opening an issue

- Check the latest release and existing issues.
- Include the application version, operating system, archive format, and the
  exact command used.
- Share the smallest safe reproduction you can. Remove photos, GPS coordinates,
  descriptions, account data, and filenames you consider private.
- Do not upload real Takeout archives or manifests to a public issue.
- Report suspected security problems through [SECURITY.md](SECURITY.md).

## Developer Certificate of Origin

Contributions use the [Developer Certificate of Origin](https://developercertificate.org/)
instead of a contributor license agreement. Sign off each commit with `-s`:

```bash
git commit -s -m "Describe the change"
```

That adds a `Signed-off-by` line certifying that you wrote the contribution or
otherwise have the right to submit it under Apache-2.0.

## Development checks

Use Rust 1.88 or newer and run:

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
cargo audit
cargo about generate about-third-party.hbs --locked --offline --fail \
  --output-file THIRD_PARTY_NOTICES.txt
```

Tests and builds must use the committed `Cargo.lock`. If dependencies change,
regenerate `THIRD_PARTY_NOTICES.txt` and confirm that every new license is
compatible and explicitly accepted in `about.toml`.

## Data-safety rules

Changes must preserve these boundaries unless a proposal includes a specific
threat analysis and migration plan:

- Input archives remain read-only.
- Archive entries cannot escape the generated scratch directory.
- Non-regular tar entries are never materialized.
- Limits apply to the bytes written, regardless of the sizes declared in an
  archive.
- Metadata and manifest replacement remains atomic.
- Existing output files are never overwritten.
- A source file is not removed after a cross-filesystem copy until the copy has
  been verified.
- Dry-run mode does not write to the output directory.
- Ctrl+C preserves the manifest and problem report.

## Pull requests

Keep each pull request focused. Explain user-visible behavior, data-safety
impact, tests performed, dependency changes, and documentation updates. New
first-party Rust files should begin with:

```rust
// SPDX-License-Identifier: Apache-2.0
//
// Copyright 2026 Shaun Murphy
```

By contributing, you agree that your contribution is licensed under the Apache
License 2.0.
