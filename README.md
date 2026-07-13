# dj-track-organizer

Dependency-free command-line tools (packaged as Claude skills) for keeping a
music / DJ library tidy end to end:

1. **normalize-music-filenames** - rename a folder of tracks to one consistent
   `Artist - Title (Mix).ext` scheme.
2. **tag-from-filename** - write the Artist and Title metadata of each file from
   its filename, for WAV, MP3 and AIFF, without re-encoding the audio.
3. **rekordbox-sync** - relink the renamed files inside a Rekordbox collection
   and refresh titles, preserving cues, beatgrids and playlists (uses
   `pyrekordbox`; always backs up `master.db`).
4. **dedup-tracks** - find duplicate songs (identical files and same track in
   multiple formats) and move the extras aside safely.

A typical run goes: normalize the filenames, tag from the filenames, sync
Rekordbox, and dedup, in that order.

## Desktop app

The same pipeline ships as a native Windows app under [`app/`](app/): pick your
library folder, hit **Organize**, and review each destructive step (rename
table, Rekordbox relink plan, duplicate list) before it happens. The app is a
Tauri v2 + Rust rewrite of the four tools — no Python needed at runtime — and
talks to Rekordbox's encrypted `master.db` through the
[`rbox`](https://crates.io/crates/rbox) crate (always backing up first, and
refusing to write while Rekordbox is running). Improvements over the CLI:
embedded tags are read natively (no ffmpeg needed), stale Beatport-style WAV
INFO chunks are fully replaced instead of shadowed, the Rekordbox artist
column is maintained directly so no manual "Reload Tag" is needed, and the
collection-entry dedup (the CLI's --rekordbox-db mode) runs as a fifth
pipeline step with its own review gate.

Behavioral parity with the Python tools is pinned by fixtures generated from
them (`tools/gen_fixtures.py` → `app/core/tests/fixtures/`); `cargo test` in
`app/` runs the whole parity suite.

```bash
cd app
npm install
npm run tauri dev    # develop
npm run tauri build  # produce the installer / exe
```

Licensing: the repository is MIT, **except `app/`, which is GPL-3.0-only**
(see `app/LICENSE`) because it links the GPL-licensed `rbox` crate.

## Requirements

- Python 3.8+
- `ffmpeg`/`ffprobe` - optional but recommended. Used to read embedded tags and
  durations. The tag writer needs no external tools at all.
- `pyrekordbox` - only for the `rekordbox-sync` tool: `pip install pyrekordbox`.

## Quick start

```bash
NM=skills/normalize-music-filenames/scripts/normalize_filenames.py
TG=skills/tag-from-filename/scripts/tag_from_filename.py
RB=skills/rekordbox-sync/scripts/rekordbox_sync.py
DD=skills/dedup-tracks/scripts/dedup_tracks.py

# 1. Preview a rename plan (nothing is changed), review, then apply
python3 $NM "/path/to/music"
python3 $NM "/path/to/music" --apply          # writes rename_rollback.csv

# 2. Write the clean names into each file's Artist/Title tags
python3 $TG "/path/to/music"

# 3. Relink the renamed files in Rekordbox (close Rekordbox first!)
python3 $RB --map "/path/to/music/rename_rollback.csv" --folder music --set-title
python3 $RB --map "/path/to/music/rename_rollback.csv" --folder music --set-title --apply

# 4. Find and move duplicates aside
python3 $DD "/path/to/music"
python3 $DD "/path/to/music" --move "/path/to/music/_duplicates"
```

## Naming scheme

```
Artist - Title (Mix).ext
Artist A, Artist B - Title (Extended Mix).ext      # collaborators, comma-separated, sorted
Group Name & Other - Title (Remix).ext             # "&" groups kept intact as one act
```

Highlights: prefers embedded tags with filename fallback, sorts comma-separated
artists alphabetically while keeping `&` acts intact, normalises `feat.`,
collapses compilation/mixtape rips, strips leading track numbers, removes
zero-width/control characters, preserves accents, and de-duplicates colliding
target names with ` (2)` suffixes.

## Safety model

- The normaliser is a **dry run by default**; it only renames with `--apply`,
  renames in two phases so no file can overwrite another, and writes a
  `rename_rollback.csv` for undo.
- The tagger writes metadata at the byte level and never re-encodes audio, so
  quality is untouched and cue/loop chunks on WAV/AIFF are preserved.
- The Rekordbox tool is a **dry run by default**, always backs up `master.db`
  before writing (aborting if it cannot), and requires Rekordbox to be closed.
- The dedup tool only writes a report by default; extras are **moved** (not
  deleted) unless you explicitly ask.

## Using as Claude skills

Each folder under `skills/` is a self-contained skill with its own `SKILL.md`.
Drop the folders into your skills directory (or install the `.skill` bundles)
and Claude will trigger them when you ask to organise, tag, sync, or dedup a
music library.

## License

MIT - see [LICENSE](LICENSE).
