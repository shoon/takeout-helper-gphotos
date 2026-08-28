# Troubleshooting

Start with the process exit code, the final summary, and
`takeout-helper-report.csv` when present. Add `--log-level info` or `--verbose`
for more detail; `RUST_LOG` takes precedence when it is set.

## Progress rows overlap or redraw incorrectly

Current builds coordinate every progress row and log line through one terminal
renderer. Progress uses short animated spinners, percentages, and item counts
instead of width-filling bars. Archive extraction uses one aggregate detail row
rather than one row per archive. Resizing Windows Terminal or PowerShell should
therefore redraw the small set of active rows without leaving old text behind.
Completed detail rows are cleared, and their results remain available in the
final summary.

The overall row uses `Step N of N` after setup and archive discovery. Detail
rows show percentages when the task has a known total. Streaming `.tgz`
extraction cannot know its final entry count in advance, so that row reports the
number of input archives and entries processed while its spinner shows that
work is continuing.

If a terminal still renders incorrectly, capture the terminal application and
version, shell, initial and resized window widths, and whether a warning was
printed at the same time. The final summary is plain text and remains the
authoritative result.

## No archives were found

Confirm that `--input` names a directory, not an individual archive. By default,
only that directory is searched. Add `--recursive` when downloads are nested.

Recognized archive names end in `.zip`, `.tgz`, or `.tar.gz`, case-insensitively.

## An incomplete split set is reported

Takeout commonly produces `...-001.zip`, `...-002.zip`, and later parts. The
warning lists missing numbers before extraction starts. Download the missing
parts into the input tree and rerun. Continuing without them creates an
incomplete library even though every present archive may extract successfully.

## Disk-space check failed

The scratch tree and organized output often coexist on one volume. Free space
or select another scratch parent:

```bash
takeout-helper-gphotos --temp-dir /path/on/larger/disk \
  --input ./takeout --output ./photos
```

The scratch parent itself is retained. Only the generated child is removed.

## Files appear under `unknown-date/`

The tool found no trustworthy sidecar date, Live Photo match, or preserved
filesystem date. Search the CSV for `unknown-date`. Check whether the matching
JSON shard was downloaded, whether the sidecar failed parsing, and whether an
earlier manual extraction replaced all modification times with the current
time.

Prefer running against the original Takeout archives, which preserve archive
entry timestamps, instead of manually extracted folders.

## Metadata write failed but a file was organized

Formats outside the embedded-EXIF set receive filesystem timestamps only.
Malformed or unsupported image containers may also reject EXIF while leaving
the file bytes untouched. Review `exif` rows in the CSV and test a copy of the
affected file with a current release.

`--copy-sidecars` can retain the source JSON next to organized media for tools
that understand Google's sidecar format.

## A fresh EXIF block warning appeared

The image contained an EXIF block that could not be parsed, but the tool was
able to write and validate a fresh block containing the usable Google sidecar
metadata. The organized file is usable and counts as an EXIF success. Existing
tags from the unreadable block may not have been preserved, so keep the Takeout
archive and inspect an affected output sample.

## The process appears to pause after metadata writing

With `--verify`, the program must read and hash every primary media output after
organization. On a large library or a slower disk, verification can take
several minutes. Current builds show verification as a named stage with a
per-file counter and estimated time. Wait for the final summary unless the
counter has stopped changing and the disk is no longer active.

An orphan-sidecar warning means a Google JSON companion file had no matching
recognized media file in the downloaded export. The warning lists the path so
you can determine whether an archive part is missing or the media filename uses
an unfamiliar Takeout naming pattern.

## Video date was not embedded

Only MP4, MOV, and M4V are candidates for QuickTime `mvhd` patching. Some files
do not contain a usable movie header or have timestamp ranges their header
version cannot represent. They still receive a filesystem modification time.

## The second run still processes files

Resume requires `.gphotos-manifest.json` and the recorded output path. Content
is reprocessed if `--force` is used, the manifest was deleted or corrupt, the
recorded output was moved or deleted, or the previous run ended before the
placement was recorded.

Different bytes with the same filename are intentionally not treated as the
same file.

Version 0.1.0 manifests could record only one path for a given content hash.
After upgrading, run once with `--force --verify` to rebuild complete path-based
verification records. Existing byte-identical output files are reused.

## Permission denied

Confirm read access to the input archives and write access to both the output
and scratch parent. Avoid protected system directories. On removable media,
check that the filesystem is mounted read-write and supports files of the
required size.

The program does not need administrator or root privileges for normal use.

## Windows SmartScreen or macOS Gatekeeper warning

Release binaries are not currently code signed. Download only from this
project's GitHub Releases page and verify `SHA256SUMS.txt`. Approve only the
specific verified file. Do not turn off SmartScreen, Gatekeeper, or antivirus
globally.

## The process was interrupted

Exit code 130 is expected after Ctrl+C. Repeat the same command and output path;
the manifest skips completed content. If `--keep-temp` was used, remove the
retained scratch tree after diagnostics because it contains extracted personal
data.

## A report path points into a deleted temp directory

Current releases translate known failure paths to final destinations when
possible. Some failures occur before a destination exists, so the source column
may describe an ephemeral scratch path. The detail column and phase identify
the operation that failed.

## Getting help without sharing private data

Open a public issue only with synthetic or redacted data. Include version,
operating system, archive type, command, exit code, relevant summary lines, and
the smallest non-private reproduction. Never attach a real export, photo,
sidecar, manifest, or unredacted CSV.

Use [private vulnerability reporting](../SECURITY.md) for a suspected security
boundary failure.
