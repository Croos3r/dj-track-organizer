---
name: rekordbox-sync
description: >-
  Relink renamed audio files inside a Rekordbox collection and refresh their
  titles, without losing cues, beatgrids, hot cues or playlists. Use this
  whenever someone renamed or reorganised music files outside Rekordbox and now
  the tracks show as missing/relocated, or wants to apply a new filename/tag
  scheme to an existing analyzed Rekordbox library programmatically. Trigger on
  mentions of Rekordbox, master.db, "tracks are missing in Rekordbox", relinking
  or relocating a DJ collection, or syncing renamed files back into Rekordbox.
---

# Rekordbox Sync

After you rename audio files outside Rekordbox, its database still points at the
old paths and the tracks appear as missing. `scripts/rekordbox_sync.py` updates
the stored path and filename for each renamed track, which relinks it **without
re-analysis**, so cue points, beatgrids, hot cues and playlist membership are
preserved. It can also set the Title directly.

This tool intentionally does the minimum risky work. It relinks paths (safe) and
optionally sets the Title (a plain string, safe). It deliberately does NOT try to
rewrite the linked Artist/Album tables, because Rekordbox's own **Reload Tag**
does that correctly and the files already carry good tags (see the
tag-from-filename tool).

## Requirements

```bash
pip install pyrekordbox
```

pyrekordbox reads the database key from your local Rekordbox config
automatically. It supports Rekordbox 6 and 7 (`master.db`).

## Safety rules (read before running)

- **Close Rekordbox completely** before applying. It locks and caches the
  database, and concurrent writes can corrupt it.
- The script **always backs up `master.db`** (plus any `-wal`/`-shm` side files)
  before writing, and aborts if the backup cannot be made.
- **Dry run is the default.** Nothing is written without `--apply`.
- A brand-new Rekordbox release can occasionally break key extraction until
  pyrekordbox catches up. If opening the DB fails, update pyrekordbox.

## Workflow

1. Rename and tag your files first with the normalize-music-filenames and
   tag-from-filename tools. Keep the `rename_rollback.csv` (or `rename_plan.csv`)
   they produce - it maps old names to new ones and drives the relink.
2. Dry run to see what would be relinked:
   ```bash
   python3 scripts/rekordbox_sync.py --map rename_rollback.csv --folder "Track" --set-title
   ```
   `--folder` limits changes to tracks whose stored path contains that string,
   so you never touch unrelated parts of the collection.
3. Close Rekordbox, then apply:
   ```bash
   python3 scripts/rekordbox_sync.py --map rename_rollback.csv --folder "Track" --set-title --apply
   ```
4. Reopen Rekordbox. The tracks are linked again with all analysis intact.
   Select them and use **Reload Tag** to pull Artist/Title/etc from the files.

## Options reference

```
python3 scripts/rekordbox_sync.py --map CSV [options]

  --map CSV        rename_plan.csv or rename_rollback.csv (required)
  --folder STR     only touch tracks whose stored path contains STR
  --set-title      also set Title in the DB from the new filename
  --apply          write changes (default: dry run)
  --backup-dir DIR where to store the master.db backup
                   (default: ./rekordbox_backups)
  --yes            skip the interactive "Rekordbox is closed?" prompt
```

## Notes

- If you did not keep a rename mapping CSV, you can still relink by matching on
  Artist/Title, but the mapping CSV is the reliable path and is strongly
  preferred.
- Restoring a backup is just copying the `master.db.<timestamp>.bak` file back
  over `master.db` (with Rekordbox closed).
