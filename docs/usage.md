# Usage recipes

Every invocation requires named input and output directories:

```text
takeout-helper-gphotos --input <DIR> --output <DIR> [OPTIONS]
```

There are no positional directory arguments. Run `--help` for the authoritative
options supported by your installed version.

## Recommended first migration

Plan, then run with verification:

```bash
takeout-helper-gphotos --dry-run \
  --input ./takeout --output ./photos

takeout-helper-gphotos --verify \
  --input ./takeout --output ./photos
```

The commands use the default `YYYY/MM` layout, content de-duplication, automatic
resume, GPS-derived per-photo timezones, and one worker per logical CPU.

## Keep chronology and albums

`date-album` gives every file a primary dated copy and gives album members an
additional album copy:

```bash
takeout-helper-gphotos --organize date-album \
  --input ./takeout --output ./photos
```

This layout uses more storage than `date`, but removing an album folder later
cannot remove the only organized copy.

If only album folders are wanted, use `--organize album`. Files that do not
belong to a user album still fall back to `YYYY/MM` so they are not lost.

## Retain Google's sidecars

```bash
takeout-helper-gphotos --copy-sidecars \
  --input ./takeout --output ./photos
```

The JSON name follows the final media filename, including a collision suffix.
Keeping the JSON provides an archival copy of fields that cannot be embedded in
the media.

## Use a larger scratch disk

```bash
takeout-helper-gphotos --temp-dir /mnt/scratch \
  --input ./takeout --output /mnt/library/photos
```

Add `--keep-temp` only for diagnosis. The extracted tree can be as large as the
library and can contain personal sidecar data, so delete it securely after use.

## Search nested download folders

```bash
takeout-helper-gphotos --recursive \
  --input ./downloads --output ./photos
```

Without `--recursive`, only archives directly inside the input directory are
considered.

## Limit resource use

Use fewer worker threads on a memory-constrained computer:

```bash
takeout-helper-gphotos --jobs 4 \
  --input ./takeout --output ./photos
```

The archive guards can also be tightened. Sizes accept a bare byte count or
`K`, `KB`, `M`, `MB`, `G`, `GB`, `T`, and `TB` suffixes:

```bash
takeout-helper-gphotos --max-file-size 20G \
  --max-archive-size 80G --max-files 75000 \
  --input ./takeout --output ./photos
```

Raise limits only when the export needs them. A legitimate file beyond a limit
is skipped and reported; a malformed archive should not be given unlimited
room.

## Force one display timezone

Normally each geotagged photo uses its own offline GPS-derived timezone and
other photos fall back to UTC. Override all image date rendering when the
library should use one known zone:

```bash
takeout-helper-gphotos --timezone America/New_York \
  --input ./takeout --output ./photos
```

QuickTime container timestamps remain UTC, as required by that format.

## Reprocess after changing options

The manifest normally skips content that is already present:

```bash
takeout-helper-gphotos --force --copy-sidecars \
  --input ./takeout --output ./photos
```

`--force` ignores resume decisions; it does not overwrite existing output.
Name collisions still follow the content and `_N` rules described in
[Organization and resume](organization-and-resume.md).

## Keep every byte-identical copy

```bash
takeout-helper-gphotos --no-dedup \
  --input ./takeout --output ./photos
```

Most migrations should keep de-duplication enabled. It prevents repeated
exports and album duplicates from multiplying the library.

## Skip filename-identified derivatives

```bash
takeout-helper-gphotos --skip-derivatives \
  --input ./takeout --output ./photos
```

The flag recognizes suffixes such as `-edited`, `-pano`, and `-collage`. It is
off by default because a user-created name such as `sunset-pano.jpg` can match.
Always dry-run before enabling it.

See the [CLI reference](cli-reference.md) for the full flag table.
