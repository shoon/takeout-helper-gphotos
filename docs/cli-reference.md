# CLI reference

```text
takeout-helper-gphotos --input <DIR> --output <DIR> [OPTIONS]
```

`--input` and `--output` are required named flags. Values shown as defaults
below describe version 0.1.0.

## Required paths

| Option | Meaning |
| --- | --- |
| `-i`, `--input <DIR>` | Directory containing Takeout ZIP/TGZ/TAR.GZ archives |
| `-o`, `--output <DIR>` | Organized library destination; created when missing |

## Discovery and scratch space

| Option | Meaning |
| --- | --- |
| `-r`, `--recursive` | Search below the input directory instead of only its root |
| `-t`, `--temp-dir <DIR>` | Scratch parent; default `<output>/temp` |
| `--keep-temp` | Retain the generated scratch child after the run |

The scratch parent is never deleted. A unique `gphotos-takeout-<random>` child
is created inside it, and only that child is eligible for automatic cleanup.

## Processing

| Option | Meaning |
| --- | --- |
| `-j`, `--jobs <N>` | Worker threads; default one per logical CPU |
| `--dry-run` | Plan without EXIF writes, output copies, manifest, or report |
| `--organize <LAYOUT>` | `date`, `album`, `flat`, or `date-album`; default `date` |
| `--force` | Ignore resume matches and reconsider every input file |
| `--no-dedup` | Keep byte-identical copies instead of collapsing them |
| `--copy-sidecars` | Copy matched JSON next to organized media |
| `--skip-derivatives` | Skip recognized generated-name suffixes; off by default |
| `--verify` | Re-hash manifest targets after organization |
| `--timezone <TZ>` | Force an IANA timezone such as `America/New_York` |

`--preserve-albums` remains accepted as a deprecated alias for
`--organize date-album`. It cannot be combined with `--organize`.

## Archive limits

| Option | Default | Meaning |
| --- | --- | --- |
| `--max-file-size <SIZE>` | `50G` | Maximum actual uncompressed bytes for one entry |
| `--max-archive-size <SIZE>` | `100G` | Maximum total uncompressed bytes for one archive |
| `--max-files <N>` | `100000` | Maximum entries extracted from one archive |

Sizes accept `K`/`KB`, `M`/`MB`, `G`/`GB`, `T`/`TB`, or bare bytes.
Whitespace and letter case are ignored. An invalid size aborts instead of
silently falling back.

## Logging

| Option | Meaning |
| --- | --- |
| `--log-level <LEVEL>` | `error`, `warn`, `info`, `debug`, or `trace`; default `warn` |
| `-v`, `--verbose` | Shortcut for debug logging |
| `-h`, `--help` | Print help |
| `-V`, `--version` | Print version |

Precedence is `RUST_LOG`, then `--verbose`, then `--log-level`.

## Reports

When something needs attention, the tool writes:

```text
<output>/takeout-helper-report.csv
```

Columns are `phase,source,destination,detail`. Spreadsheet-leading formula
characters are neutralized and CSV quoting is applied.

| Phase | Meaning |
| --- | --- |
| `archive` | An archive failed to extract |
| `archive-entries-skipped` | Unsafe or oversized archive entries were skipped |
| `exif` | Metadata writing failed |
| `organize` | A media file could not be placed |
| `organize-warning` | Media succeeded but an album or sidecar copy did not |
| `derivative-skipped` | Excluded by `--skip-derivatives` |
| `unknown-date` | Filed under `unknown-date/` |
| `verify-failed` | A manifest target was missing or mismatched |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Completed without recorded failures |
| `1` | Completed with one or more failures; inspect summary and CSV |
| `2` | Command-line usage error reported by Clap |
| `130` | Interrupted by Ctrl+C |

Warnings such as a sidecar copy failure can still leave usable media in place.
Treat a nonzero code as a reason to inspect, not as proof that nothing happened.

Run `takeout-helper-gphotos --help` for the exact reference installed on your
machine.
