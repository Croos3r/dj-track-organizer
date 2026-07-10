---
name: tag-from-filename
description: >-
  Write Artist and Title metadata into audio files based on their filenames,
  for WAV, MP3 and AIFF, with no re-encoding and no third-party libraries. Use
  this whenever someone wants to add, fill, fix, or refresh the tags/metadata of
  music tracks so they match the file names, make an ID3 or WAV tag library
  consistent, tag untagged WAV or AIFF files, or prepare a DJ collection so that
  Artist and Title show up correctly in Rekordbox, Serato, or a media player.
  Trigger even if the user just says "these files have no tags" or "set the
  artist and title from the file name".
---

# Tag From Filename

Fill each file's Artist and Title tags from its filename, assuming the
`Artist - Title.ext` convention. Everything after the first ` - ` (including a
`(Mix)` suffix) becomes the title. Powered by
`scripts/tag_from_filename.py` (Python 3 only, no dependencies).

## Why this approach

Audio tag editors usually re-mux or re-encode the whole file. For a large
library (tens of GB) that is slow and can strip DJ metadata. This script writes
tags at the byte level instead:

- **WAV**: a RIFF `LIST/INFO` chunk (`IART`, `INAM`) appended after the audio.
  Existing cue points, ACID loop info and `adtl` label chunks are preserved;
  stale `id3 ` chunks are dropped so players read one consistent source.
- **MP3**: an `ID3v2.4` tag (`TPE1`, `TIT2`) at the front. If the existing tag
  has room the new one is written in place with padding, so the audio is never
  moved; otherwise the file is rewritten once (still no re-encode).
- **AIFF/AIFC**: an `ID3 ` chunk carrying the ID3v2.4 tag, appended to the FORM
  container with big-endian sizes.

The audio stream is never decoded or re-encoded, so quality is untouched and it
is fast even on huge files.

## Workflow

1. Make sure filenames are already in `Artist - Title` form. If they are not,
   run the **normalize-music-filenames** skill first.
2. Preview what will be written:
   ```bash
   python3 scripts/tag_from_filename.py "/path/to/music" --dry-run
   ```
3. Apply:
   ```bash
   python3 scripts/tag_from_filename.py "/path/to/music"
   ```
4. Add `--recursive` to walk sub-folders.

## Behaviour

- Artist = text before the first ` - `; Title = everything after it.
- Files with no ` - ` in the name get a title only (no artist) and are counted
  as skipped-style; nothing is guessed.
- UTF-8 is used throughout, so accents and non-Latin characters survive.
- Re-running is safe and idempotent: it replaces its own tags rather than
  stacking duplicates.
- The script is resilient: any file that fails is reported at the end and the
  rest still get tagged.

## Options reference

```
python3 scripts/tag_from_filename.py FOLDER [options]

  --dry-run     show artist/title that would be written, change nothing
  --recursive   recurse into sub-folders
```

## Verifying

Check a few files with ffprobe:
```bash
ffprobe -v quiet -show_entries format_tags=artist,title -of default=noprint_wrappers=1 "Artist - Title.wav"
```
