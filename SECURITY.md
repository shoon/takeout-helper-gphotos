# Security policy

## Supported versions

Security fixes are applied to the latest release and the `main` branch. Older
pre-1.0 releases are not maintained after a fixed release is available.

## Report a vulnerability

Do not open a public issue for a suspected vulnerability. Use
[GitHub private vulnerability reporting](https://github.com/shoon/takeout-helper-gphotos/security/advisories/new).

Include:

- the affected version, operating system, and archive type;
- a clear description of the impact;
- reproduction steps using synthetic files with no personal media or metadata;
- whether the issue can write outside the selected output or scratch tree,
  overwrite an existing file, delete user data, exhaust resources despite a
  configured limit, or corrupt a media file; and
- any suggested mitigation or disclosure constraints.

Do not attach real Takeout archives, photos, videos, sidecars, manifests, CSV
reports, GPS coordinates, or account data. You should receive an acknowledgement
within seven days. Please allow time to investigate and prepare a coordinated
fix before public disclosure.

## High-priority scope

High-priority reports include path traversal, symlink or hard-link escapes,
arbitrary file overwrite or deletion, archive-limit bypasses, zip or tar bombs,
unsafe metadata parsing, output-name races, manifest path abuse, corrupted
atomic replacement, and failures that delete the only usable copy of a file.

Incorrect metadata matching for a particular export is usually a correctness
bug rather than a security vulnerability unless it crosses one of those safety
boundaries.
