# Documentation

Takeout Helper for Google Photos processes a local export from archive discovery
through final verification. Start with the first two guides if this is your
first run; use the remaining guides when choosing a layout, diagnosing a result,
or contributing code.

## Start here

1. [Getting started](getting-started.md): prepare the export, install a release,
   verify its checksum, plan storage, and complete a first dry run.
2. [Usage recipes](usage.md): choose a practical command for a basic migration,
   album preservation, sidecar retention, or a constrained machine.
3. [Safety and recovery](safety-and-recovery.md): understand what the tool
   changes, what it refuses to change, and how to recover after interruption.

## Understand the result

- [Organization and resume](organization-and-resume.md) explains layouts,
  duplicate handling, the manifest, albums, derivatives, sidecars, and
  `unknown-date/`.
- [Metadata and formats](metadata-and-formats.md) explains sidecar matching,
  date precedence, timezone handling, EXIF writes, video timestamps, and the
  supported extension list.
- [CLI reference](cli-reference.md) documents every option, report phase, and
  exit code.

## Solve a problem

- [Troubleshooting](troubleshooting.md) covers missing archives, incomplete
  shard sets, disk-space errors, unknown dates, metadata failures, permissions,
  logging, and unsigned-binary warnings.
- The CSV report in the output directory identifies affected source and
  destination paths without requiring you to scrape logs.
- Security problems belong in the [private reporting process](../SECURITY.md),
  not a public issue.

## Contribute or release

- [Development](development.md) covers the architecture, minimum Rust version,
  validation commands, dependency policy, vendored EXIF patch, CI, and release
  process.
- [CONTRIBUTING.md](../CONTRIBUTING.md) covers DCO sign-off, pull-request scope,
  privacy expectations, and non-negotiable data-safety boundaries.

Return to the [project README](../README.md).
