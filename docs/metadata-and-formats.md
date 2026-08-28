# Metadata and supported formats

Google Takeout places important capture information in JSON **sidecars**: small
companion files stored beside photos and videos rather than inside them. A
sidecar can contain fields such as capture time, description, GPS coordinates,
and album context. The tool pairs those files with their media, validates
individual fields, writes the fields supported by each format, and keeps
the original archives unchanged.

## Sidecar matching

The matcher handles:

- current `.supplemental-metadata.json` names;
- legacy `.json` names;
- Google's truncated supplemental suffixes and long base names;
- collision counters placed before or after the extension;
- localized `-edited` copies that inherit the base photo's sidecar; and
- Live Photo or motion-photo video companions that inherit the still's date
  only when the video has no sidecar of its own.

Housekeeping files such as album `metadata.json` are not counted as orphan
sidecars. Invalid individual values are dropped or truncated while usable fields
remain available. A sidecar larger than 16 MiB is rejected before reading as a
resource-exhaustion guard.

## Date precedence

Organization chooses the first trustworthy date in this order:

1. `photoTakenTime` from the media's own sidecar;
2. `creationTime` from that sidecar;
3. a same-directory Live Photo still-image date for an undated video; and
4. a plausible preserved filesystem modification time.

Timestamps that look like extraction time, lie implausibly near the Unix epoch,
or cannot be read are not treated as capture dates. Such files go to
`unknown-date/`.

## Image metadata

For formats supported by the pure-Rust EXIF writer, available sidecar fields are
written as:

- `DateTimeOriginal`, `CreateDate`, and `ModifyDate`;
- `OffsetTimeOriginal` and `OffsetTimeDigitized`;
- GPS latitude, longitude, direction references, altitude, and altitude
  reference; and
- `ImageDescription`.

The write occurs in a sibling temporary copy. The copy is parsed and checked
before it replaces the extracted working file, and the desired filesystem
modification time is applied after the EXIF rewrite.

EXIF is currently written for discovered JPEG, PNG, HEIC/HEIF, AVIF, TIFF, and
WebP files. Other recognized still or RAW formats retain corrected filesystem
timestamps but are not promised embedded EXIF support.

## Timezones

With no override, GPS coordinates are resolved to an IANA timezone entirely
offline. Dates are rendered at the real UTC offset for the capture instant, so
daylight-saving transitions are respected. Zero/zero coordinates and invalid
coordinates are ignored.

Without a usable location, image dates are written in UTC with an explicit
`+00:00` offset. `--timezone <TZ>` forces one IANA zone for every image.

## Video dates

MP4, MOV, and M4V files may carry creation and modification timestamps in the
QuickTime movie header (`moov/mvhd`). The tool walks the atom tree instead of
searching raw bytes, supports version-0 and version-1 fields, rejects invalid
ranges, patches a temporary copy, re-parses it, and then replaces the working
copy.

QuickTime timestamps remain UTC as required by the container format. A video
without a usable `mvhd` still receives its corrected filesystem modification
time and is counted separately from an embedded-video-date success.

## Recognized media extensions

Matching is case-insensitive.

Stills:

```text
jpg jpeg png heic heif avif gif webp bmp tif tiff
```

RAW:

```text
dng cr2 cr3 nef arw orf rw2 raf
```

Video:

```text
mp4 mov m4v 3gp 3g2 avi mkv wmv mpg mpeg mts m2ts mp
```

Only MP4, MOV, and M4V currently receive embedded QuickTime dates. Other video
formats receive filesystem modification times. Files outside the recognized
extension list are skipped and counted rather than copied silently.
