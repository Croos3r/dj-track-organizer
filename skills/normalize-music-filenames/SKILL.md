---
name: normalize-music-filenames
description: >-
  Rename a folder of audio files (mp3, wav, aiff, flac, m4a, ogg) to one
  consistent "Artist - Title (Mix)" scheme. Use this whenever someone wants to
  clean up, standardise, tidy, or organise the filenames of a music library or
  DJ collection, fix inconsistent track names, strip compilation/mixtape names
  and leading track numbers, normalise "feat." wording, or sort collaborating
  artists. Trigger even if the user only says "my music files are a mess" or
  "make all these tracks follow the same naming format".
---

# Normalize Music Filenames

Turn an inconsistent pile of audio filenames into a uniform
`Artist - Title (Mix).ext` library. The work is done by
`scripts/normalize_filenames.py`, which is dependency-free (Python 3 only) and
uses `ffprobe` when it is installed to read embedded tags.

## Why a dry run first

Renaming hundreds of irreplaceable music files is high-stakes, so the script
never renames anything unless asked. Its default output is a plan CSV
(`OLD name, NEW name, from`) that a human can review. Always show the plan and
get confirmation before applying.

## Workflow

1. Confirm the target format with the user. The default is `Artist - Title`
   with the mix/version kept in parentheses. Ask whether embedded **tags** or
   the existing **filename** should win when they disagree (default: tags), and
   whether artists should be sorted **alphabetically** (default: yes).
2. Dry run to produce the plan:
   ```bash
   python3 scripts/normalize_filenames.py "/path/to/music"
   ```
3. Open `rename_plan.csv` and review it together. Look especially at the rows
   marked `manual` (the script could not derive a confident Artist - Title) and
   any duplicate targets (auto-suffixed ` (2)`).
4. Apply once approved. This renames in two phases (via temp names) so no file
   can ever overwrite another, and writes `rename_rollback.csv` for undo:
   ```bash
   python3 scripts/normalize_filenames.py "/path/to/music" --apply
   ```

## What the normaliser handles

- **Source of truth**: prefers embedded tags, falls back to the filename.
  Override with `--source filename`.
- **Artist ordering**: comma-separated artists are sorted alphabetically.
  Groups joined by `&` (e.g. `D-Block & S-te-Fan`) are treated as one act name
  and never split. Disable sorting with `--no-alphabetical`.
- **feat. wording**: `featuring`, `ft`, `ft.` all become `feat.`.
- **Compilation / mixtape rips**: names like
  `Artist - MIXTAPE -VCU017- - 02 Track Title` collapse to `Artist - Track Title`.
- **Leading track numbers** (`02  `) and lowercase hyphen-slugs
  (`tha-playah-still-a-playah`) are cleaned and title-cased.
- **Mix suffix**: kept from the filename when the tag omits it. Drop it with
  `--drop-mix`.
- **UTF-8 hygiene**: zero-width and control characters are stripped; accents
  (é, ä, ø, Œ) are preserved. Characters illegal in filenames (`\ / : * ? " < > |`)
  are removed.
- **Collisions**: identical target names get ` (2)`, ` (3)` suffixes instead of
  clobbering each other.

## Options reference

```
python3 scripts/normalize_filenames.py FOLDER [options]

  --apply             actually rename (default: dry run only)
  --source tags|filename   preferred source on conflict (default: tags)
  --no-alphabetical   keep original artist order
  --drop-mix          do not re-attach a filename-only mix suffix
  --plan PATH         where to write the plan CSV
```

## Notes

- Files the script cannot confidently name (no tags and an ambiguous filename)
  are listed as `manual` and left untouched. Offer to name those by hand.
- `rename_rollback.csv` maps each new name back to its original, so a rename run
  can always be reversed.
- Pair this with the **tag-from-filename** skill to write the cleaned names back
  into each file's Artist/Title metadata afterwards.
