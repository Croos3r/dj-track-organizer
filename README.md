# dj-track-organizer

Two dependency-free command-line tools (packaged as Claude skills) for keeping a
music / DJ library tidy:

1. **normalize-music-filenames** - rename a folder of tracks to one consistent
   `Artist - Title (Mix).ext` scheme.
2. **tag-from-filename** - write the Artist and Title metadata of each file from
   its filename, for WAV, MP3 and AIFF, without re-encoding the audio.

They are designed to be used together: normalize the filenames first, then push
those clean names into the file tags.

## Requirements

- Python 3.8+
- `ffmpeg`/`ffprobe` is optional but recommended. `normalize-music-filenames`
  uses `ffprobe` to read embedded tags when present; without it, it falls back
  to parsing the existing filename. `tag-from-filename` needs no external tools
  at all.

## Quick start

```bash
# 1. Preview a rename plan (nothing is changed)
python3 skills/normalize-music-filenames/scripts/normalize_filenames.py "/path/to/music"

# 2. Review rename_plan.csv, then apply (writes rename_rollback.csv for undo)
python3 skills/normalize-music-filenames/scripts/normalize_filenames.py "/path/to/music" --apply

# 3. Write the clean names into each file's Artist/Title tags
python3 skills/tag-from-filename/scripts/tag_from_filename.py "/path/to/music"
```

## Naming scheme

```
Artist - Title (Mix).ext
Artist A, Artist B - Title (Extended Mix).ext      # collaborators, comma-separated, sorted
Group Name & Other - Title (Remix).ext             # "&" groups kept intact as one act
```

Highlights:

- Prefers embedded tags, falls back to the filename.
- Sorts comma-separated artists alphabetically; never splits `&` acts.
- Normalises `featuring` / `ft` / `ft.` to `feat.`.
- Collapses compilation / mixtape rips
  (`Artist - MIXTAPE -VCU017- - 02 Track` -> `Artist - Track`).
- Strips leading track numbers and title-cases lowercase hyphen-slugs.
- Removes zero-width and control characters; preserves accents; strips
  filename-illegal characters.
- De-duplicates colliding target names with ` (2)`, ` (3)` suffixes.

## Safety

- The normaliser is a **dry run by default**; it only renames with `--apply`.
- Renames happen in two phases (temp names first) so no file can overwrite
  another, even in swap/chain cases.
- Every applied run writes a `rename_rollback.csv` mapping new names back to
  originals.
- The tagger writes metadata at the byte level and never re-encodes audio, so
  quality is untouched and cue/loop chunks on WAV/AIFF are preserved.

## Using as Claude skills

Each folder under `skills/` is a self-contained skill with its own `SKILL.md`.
Drop the folders into your skills directory (or install the `.skill` bundle) and
Claude will trigger them when you ask to organise filenames or tag a music
library.

## License

MIT - see [LICENSE](LICENSE).
