# Getting started

This guide takes a new export from Google Takeout to a verified local library.
Nothing needs to be uploaded to this project or another service.

## 1. Prepare the export

Create an export at [Google Takeout](https://takeout.google.com/) that includes
Google Photos. The exact Takeout screens can change, but these choices make a
migration easier to reason about:

- export Google Photos separately from unrelated Google products;
- choose ZIP or TGZ/TAR.GZ archives;
- choose a shard size your filesystem and browser can handle reliably; and
- download every numbered part before starting.

Place all parts for one export in a single directory. Do not manually merge or
repack them. The tool reads each shard independently and warns when a recognized
numbered series has a gap.

Keep the original downloads until the finished library has been inspected,
verified, and backed up somewhere else.

## 2. Download and verify the program

Choose the archive for your CPU and operating system from
[GitHub Releases](https://github.com/shoon/takeout-helper-gphotos/releases/latest).
Download `SHA256SUMS.txt` from the same release.

On Linux or macOS, verify all downloaded release assets in the directory:

```bash
shasum -a 256 -c SHA256SUMS.txt
```

If `sha256sum` is installed, this equivalent command also works:

```bash
sha256sum -c SHA256SUMS.txt
```

On Windows PowerShell, compare the printed hash with the matching line in
`SHA256SUMS.txt`:

```powershell
Get-FileHash .\takeout-helper-gphotos-v0.1.0-windows-x64.zip -Algorithm SHA256
```

Do not run an asset whose checksum differs.

## 3. Extract and test the binary

Windows PowerShell:

```powershell
Expand-Archive .\takeout-helper-gphotos-v0.1.0-windows-x64.zip
.\takeout-helper-gphotos-v0.1.0-windows-x64\takeout-helper-gphotos.exe --version
```

Linux x64:

```bash
tar -xzf takeout-helper-gphotos-v0.1.0-linux-x64.tar.gz
./takeout-helper-gphotos-v0.1.0-linux-x64/takeout-helper-gphotos --version
```

macOS Apple Silicon:

```bash
tar -xzf takeout-helper-gphotos-v0.1.0-macos-arm64.tar.gz
./takeout-helper-gphotos-v0.1.0-macos-arm64/takeout-helper-gphotos --version
```

The binaries are not currently code signed. Windows SmartScreen or macOS
Gatekeeper may warn about an unfamiliar download. Verify the checksum and the
release origin first. On macOS, if Gatekeeper still quarantines a verified
binary, approve it through Privacy & Security or remove quarantine from that
specific file:

```bash
xattr -d com.apple.quarantine ./takeout-helper-gphotos
```

Do not disable platform security globally.

## 4. Plan storage

The process has three relevant sizes:

- the compressed Takeout downloads;
- the extracted scratch tree; and
- the organized output library.

The default scratch parent is `<output>/temp`, so extraction and output normally
share a volume. Plan for roughly twice the uncompressed library size there. If
another disk has more room, select it explicitly:

```bash
takeout-helper-gphotos --temp-dir /mnt/large-scratch \
  --input ./takeout --output ./photos
```

The named parent is never deleted. The tool creates and owns only a child named
`gphotos-takeout-<random>`.

## 5. Run a dry run

Use an empty output directory and run:

```bash
takeout-helper-gphotos --dry-run \
  --input ./takeout --output ./photos
```

Dry run still extracts the archives because their contents cannot otherwise be
inspected. It does not write photos, metadata, a manifest, or a CSV report into
the output directory.

Review:

- the number of archives found and successfully extracted;
- any incomplete split-archive warning;
- the count of media files and matched JSON sidecars (the companion metadata
  files Google places beside photos and videos);
- how many files would be duplicates or `unknown-date`; and
- any skipped unsafe or oversized archive entries.

## 6. Run and verify

When the dry-run counts make sense:

```bash
takeout-helper-gphotos --verify \
  --input ./takeout --output ./photos
```

Exit code `0` means the run completed without recorded failures. Exit code `1`
means output may still have been produced, but the summary and
`takeout-helper-report.csv` need review. Exit code `130` means Ctrl+C interrupted
the run; repeat the same command to resume.

## 7. Inspect before deleting anything

Open samples from several years, albums, image formats, and video formats.
Check `unknown-date/` and any CSV report. Confirm the organized library in the
photo manager or backup system you intend to use, then make an independent
backup before deciding whether to remove the original Takeout downloads.

Next: [Usage recipes](usage.md) or [Safety and recovery](safety-and-recovery.md).
