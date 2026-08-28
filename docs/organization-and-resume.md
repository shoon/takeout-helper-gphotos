# Organization, duplicates, and resume

The organizer never overwrites an existing destination. Layout selection
changes where files are placed; it does not weaken collision or content checks.

## Output layouts

| Layout | Result |
| --- | --- |
| `date` | `<output>/YYYY/MM/filename` |
| `album` | `<output>/<album>/filename`; non-album files fall back to `YYYY/MM` |
| `flat` | `<output>/filename` |
| `date-album` | primary `YYYY/MM` copy for every file plus an album copy for album members |

`date` is the default. `date-album` is the safest choice when album browsing
matters because the chronological tree remains complete independently of album
folders.

## Album identification

An album is inferred from the folder surrounding a media file inside the
Takeout tree. Google's own buckets, such as `Photos from 2024`, `Archive`,
`Trash`, `Bin`, `Locked Folder`, `Failed Videos`, and `Untitled`, are not treated
as user albums.

Album names are untrusted path data. Separators and Windows-illegal characters
become `_`, leading dots and trailing dots or spaces are removed, names are
limited to 100 bytes without splitting UTF-8, and structural collisions are
renamed. Examples include bare years, `unknown-date`, and Windows device names
such as `CON`.

If sanitization leaves no useful name, the file falls back to its dated path.

## Name collisions and duplicates

Destination names are claimed atomically, which matters because organization is
parallel. When a candidate already exists:

1. file sizes are compared;
2. if sizes match and de-duplication is enabled, both files are hashed with
   BLAKE3;
3. identical content is skipped and counted as a duplicate; and
4. different content takes the next available `_1`, `_2`, and so on.

A source named `IMG_0001.jpg` therefore becomes `IMG_0001_1.jpg` only when the
existing `IMG_0001.jpg` contains different bytes. Nothing is overwritten.

`--no-dedup` skips the content comparison and always advances to a numbered
name on collision.

## Resume manifest

Successful placements are recorded in:

```text
<output>/.gphotos-manifest.json
```

The key is the BLAKE3 content hash, not the temporary extraction path. Scratch
paths change on every run, while content identity remains stable.

On the next run, content is skipped only when both conditions are true:

- its hash exists in the manifest; and
- the recorded output path still exists.

If the output file was deleted, it is processed again. The manifest is saved
through a sibling temporary file on normal completion, failure, and Ctrl+C.

`--force` ignores resume entries for the current run. Deleting the manifest is
also safe, but every input file must then be hashed and reconsidered.

## Verification

`--verify` re-hashes every output path recorded by the manifest. It reports:

- `missing` when the path no longer exists; and
- `mismatched` when the bytes no longer match the recorded hash.

Either condition produces exit code `1` and a `verify-failed` CSV row. In
`date-album` mode, verification checks the primary dated copy.

## Sidecars

`--copy-sidecars` places JSON next to the final media destination. Google's
supplemental suffix is preserved when possible. If a media collision adds `_1`,
the copied JSON follows the renamed media so the pair remains obvious.

A sidecar-copy failure is a warning because the media itself is already in
place. It appears under `organize-warning` in the CSV report.

## Derivatives

Google-generated names such as `-edited`, localized edited suffixes, `-pano`,
`-collage`, `-effects`, `-animation`, `-movie`, `-mix`, and `-smile` can be
excluded with `--skip-derivatives`.

The option is off by default because it relies on filenames and can match a name
you chose yourself. Dry-run counts the files that would be skipped.

## Unknown dates

Files with no sidecar timestamp, Live Photo match, or plausible preserved
filesystem time go to:

```text
<output>/unknown-date/
```

They also receive `unknown-date` CSV rows. The current time is never substituted
because doing so would silently misfile the photo.
