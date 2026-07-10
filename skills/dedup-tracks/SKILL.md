---
name: dedup-tracks
description: >-
  Find and clean up duplicate songs in a music folder - identical files and the
  same track appearing in multiple formats (e.g. a WAV and an MP3 of the same
  song). Use whenever someone wants to deduplicate, find duplicates, remove
  repeated tracks, or clean up a bloated music/DJ library. Detects exact
  byte-identical files and same-song duplicates by Artist + Title, keeps the
  best-quality copy, and moves the extras aside safely. Trigger on "find
  duplicate songs", "I have the same track twice", "clean up duplicates", or
  "remove duplicate music files".
---

# Dedup Tracks

`scripts/dedup_tracks.py` finds duplicate songs and, when asked, moves the extra
copies aside. Two detection modes:

- **exact**: byte-for-byte identical files (grouped by size, then hashed).
  These are certain duplicates.
- **same-song**: files whose Artist + Title match after normalisation, even
  across formats (a `.wav` and a `.mp3` of the same track). The mix/version is
  part of the key, so an Original Mix and an Extended Mix are kept as distinct
  songs, not merged.

For each duplicate set it picks a keeper, preferring lossless formats
(WAV/AIFF/FLAC) over lossy, then longer duration, then larger file.

## Safety

- Default behaviour writes only a report CSV. Nothing is moved or deleted.
- `--move DEST` relocates the extra copies into a folder, so the action is fully
  reversible (just move them back).
- `--delete` exists but is discouraged; prefer `--move` and review first.

## Workflow

1. Report duplicates (no changes):
   ```bash
   python3 scripts/dedup_tracks.py "/path/to/music"
   ```
2. Open `duplicates.csv`. Each set lists one `keep` row and one or more
   `duplicate` rows, with the reason (`exact` or `same-song`).
3. Once happy, move the extras aside:
   ```bash
   python3 scripts/dedup_tracks.py "/path/to/music" --move "/path/to/music/_duplicates"
   ```

## Options reference

```
python3 scripts/dedup_tracks.py FOLDER [options]

  --recursive        recurse into sub-folders
  --mode exact|song|both   which duplicates to detect (default: both)
  --report PATH      where to write the report CSV
  --move DEST        move extra copies into DEST (reversible, recommended)
  --delete           delete extra copies (dangerous)
```

## Notes

- Uses `ffprobe` to read tags when available; otherwise it derives Artist/Title
  from the filename (`Artist - Title.ext`), so it still works on untagged files
  that follow the naming convention.
- Exact and same-song passes never report the same file twice.
- Reviewing the CSV before moving anything is strongly recommended, especially
  for same-song matches where remixes or edits could share a title.
