# Safety and recovery

The program handles untrusted archive structures and personal media. It expects
malformed entries, duplicate names, interruptions, full disks, and pre-existing
output files.

## What is and is not modified

Input archives are opened for reading and never rewritten. Metadata changes are
made only to extracted scratch copies. Organized output is newly created; an
existing destination is compared or bypassed with a numbered name, never
overwritten.

Do not use the output directory as the only copy of a library. Keep the Takeout
downloads and make an independent backup after inspecting the result.

## Extraction confinement

Every run creates a uniquely named `gphotos-takeout-<random>` child under the
selected scratch parent. Archive names are lexically normalized and must remain
inside the canonical scratch root.

The extractor rejects or skips:

- `..` traversal components;
- absolute paths and platform path prefixes;
- names beyond the path-length or depth guard;
- tar symlinks, hard links, devices, and FIFOs; and
- entries beyond configured count or byte limits.

The per-file limit is enforced with a bounded reader against bytes actually
written. The total limit accumulates actual output bytes. Declared ZIP sizes are
also pre-scanned for an early disk-space and compression-ratio warning.

## Default limits

| Guard | Default |
| --- | --- |
| Entries per archive | 100,000 |
| Uncompressed bytes per file | 50 GiB |
| Uncompressed bytes per archive | 100 GiB |
| JSON sidecar size | 16 MiB, fixed |
| Extracted path length | 1,024 bytes/OS string units |
| Archive path depth | 100 components |

Command-line byte limits may be lowered or raised. The sidecar, path, and depth
guards are fixed safety boundaries.

## Atomic metadata and manifest writes

Image EXIF and QuickTime video updates are written to a sibling temporary copy.
The result is validated before replacement. If parsing or verification fails,
the original extracted file remains intact.

The resume manifest uses the same temporary-file-then-rename pattern. A crash
during serialization should not leave half a JSON document that silently
disables resume.

## Output placement

Parallel workers claim names with atomic create-new operations. Cross-filesystem
placement copies the file through the claimed handle, verifies the result, and
only then removes the scratch source. Same-filesystem placement may use an
atomic rename.

BLAKE3 hashing determines byte identity for duplicate handling, manifest
records, and final verification.

## Dry run

`--dry-run` exercises discovery, extraction, pairing, date resolution, layout,
collision planning, duplicate detection, album planning, and sidecar planning.
It does not write metadata or anything inside the output directory.

Scratch extraction is still real and needs disk space. Unless `--keep-temp` is
set, the generated scratch child is deleted at the end.

## Interrupted runs

Press Ctrl+C once and allow the current operation to stop. The program records
the interruption, saves the manifest and report where possible, and exits 130.

Repeat the same command to resume. Content already recorded at an existing
output path is skipped. If interruption occurred before a particular placement
was recorded, the normal collision and duplicate rules handle it again without
overwriting existing files.

## Full disk or partial failure

Do not delete the source downloads. Free space or move the scratch parent to a
larger volume, then repeat the command. Review `takeout-helper-report.csv` and
the summary. The tool can complete useful work and still return exit code 1
when some files failed.

Use `--verify` after recovery. A missing or mismatched manifest target is a hard
failure and remains visible in the CSV report.

## Privacy

Sidecars can contain descriptions, timestamps, filenames, GPS coordinates, and
account-related context. Logs and CSV reports can contain local paths. Redact
these before sharing diagnostics, and remove retained scratch directories when
they are no longer needed.

Report a safety-boundary failure privately through [SECURITY.md](../SECURITY.md).
