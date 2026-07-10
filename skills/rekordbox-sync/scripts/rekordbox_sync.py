#!/usr/bin/env python3
"""Relink renamed tracks in the Rekordbox collection and refresh their titles.

When you rename audio files outside Rekordbox, its database (master.db) still
points at the old paths, so the tracks show up as missing. This tool updates the
stored path and filename for each renamed track, which relinks it WITHOUT any
re-analysis, so cue points, beatgrids, hot cues and playlist membership are all
kept. It can also set the Title directly from the new filename.

Artist and full metadata are best refreshed inside Rekordbox with "Reload Tag"
once the tracks are relinked, because the files already carry correct tags
(see the tag-from-filename tool) and Rekordbox then updates the linked artist /
album tables correctly.

Requirements:
    pip install pyrekordbox

Safety:
    - Rekordbox MUST be closed before running with --apply.
    - master.db is always backed up before any write; the run aborts if the
      backup cannot be made.
    - Dry run is the default. Nothing is written without --apply.
"""
import argparse
import csv
import datetime as dt
import os
import shutil
import sys


def load_mapping(csv_path):
    """Return {old_basename: new_basename} from a rename plan or rollback CSV.

    Accepts either the plan CSV (header 'OLD name,NEW name,...') or the rollback
    CSV (header 'current_name,restore_to' meaning new,old).
    """
    mapping = {}
    with open(csv_path, newline="", encoding="utf-8-sig") as fh:
        reader = csv.reader(fh)
        header = next(reader, [])
        h = [c.strip().lower() for c in header]
        if "old name" in h and "new name" in h:
            oi, ni = h.index("old name"), h.index("new name")
            for row in reader:
                if len(row) > max(oi, ni):
                    mapping[row[oi].strip()] = row[ni].strip()
        elif "current_name" in h and "restore_to" in h:
            ci, ri = h.index("current_name"), h.index("restore_to")
            for row in reader:
                if len(row) > max(ci, ri):
                    mapping[row[ri].strip()] = row[ci].strip()  # old -> new
        else:
            sys.exit(f"Unrecognised mapping CSV header: {header}")
    return mapping


def parse_name(fn):
    base = os.path.splitext(fn)[0]
    if " - " in base:
        a, t = base.split(" - ", 1)
        return a.strip(), t.strip()
    return "", base.strip()


def open_database():
    try:
        from pyrekordbox import MasterDatabase as _DB  # newer pyrekordbox
    except ImportError:
        try:
            from pyrekordbox import Rekordbox6Database as _DB  # older
        except ImportError:
            sys.exit("pyrekordbox is not installed. Run:  pip install pyrekordbox")
    try:
        return _DB()
    except Exception as e:  # noqa: BLE001
        sys.exit(f"Could not open the Rekordbox database: {e}\n"
                 "Make sure Rekordbox is installed and has been opened at least "
                 "once, and that it is fully CLOSED right now.")


def locate_db_file(db):
    for attr in ("db_path", "path", "filename", "_path"):
        p = getattr(db, attr, None)
        if p and os.path.isfile(str(p)):
            return str(p)
    # fall back to pyrekordbox config
    try:
        from pyrekordbox.config import get_config
        p = get_config("rekordbox6", "db_path") or get_config("rekordbox7", "db_path")
        if p and os.path.isfile(str(p)):
            return str(p)
    except Exception:  # noqa: BLE001
        pass
    return None


def backup_db(db_file, backup_dir):
    os.makedirs(backup_dir, exist_ok=True)
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    dest = os.path.join(backup_dir, f"master.db.{stamp}.bak")
    shutil.copy2(db_file, dest)
    # also copy the -wal / -shm side files if present
    for ext in ("-wal", "-shm"):
        side = db_file + ext
        if os.path.exists(side):
            shutil.copy2(side, dest + ext)
    return dest


def main():
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--map", required=True,
                    help="rename_plan.csv or rename_rollback.csv from the "
                         "normalize-music-filenames tool")
    ap.add_argument("--folder", default=None,
                    help="Only touch tracks whose folder path contains this "
                         "string (recommended, limits scope to your library)")
    ap.add_argument("--set-title", action="store_true",
                    help="Also set the Title in the database from the new "
                         "filename (artist is left for Rekordbox 'Reload Tag')")
    ap.add_argument("--apply", action="store_true",
                    help="Actually write to the database (default: dry run)")
    ap.add_argument("--backup-dir", default=None,
                    help="Where to store the master.db backup "
                         "(default: ./rekordbox_backups)")
    ap.add_argument("--yes", action="store_true",
                    help="Skip the interactive 'Rekordbox is closed' prompt")
    args = ap.parse_args()

    mapping = load_mapping(args.map)
    print(f"loaded {len(mapping)} renamed entries from {args.map}")

    db = open_database()
    from_content = db.get_content()
    if args.folder:
        # filter in Python to keep it simple/portable across schema versions
        contents = [c for c in from_content
                    if args.folder in (getattr(c, "FolderPath", "") or "")]
    else:
        contents = list(from_content)

    plan = []
    for c in contents:
        folder_path = getattr(c, "FolderPath", "") or ""
        cur_name = os.path.basename(folder_path) or (getattr(c, "FileNameL", "") or "")
        if cur_name in mapping and mapping[cur_name] != cur_name:
            new_name = mapping[cur_name]
            new_path = os.path.join(os.path.dirname(folder_path), new_name)
            artist, title = parse_name(new_name)
            plan.append((c, cur_name, new_name, folder_path, new_path, title))

    print(f"tracks to relink: {len(plan)}")
    for _, old, new, _, _, _ in plan[:15]:
        print(f"  {old}  ->  {new}")
    if len(plan) > 15:
        print(f"  ... and {len(plan) - 15} more")

    if not args.apply:
        print("\nDry run only. Re-run with --apply to write the changes.")
        return

    if not plan:
        print("Nothing to do.")
        return

    if not args.yes:
        ans = input("\nIs Rekordbox FULLY CLOSED? Type 'yes' to continue: ").strip().lower()
        if ans != "yes":
            sys.exit("Aborted. Close Rekordbox and run again.")

    db_file = locate_db_file(db)
    if not db_file:
        sys.exit("Could not locate master.db to back it up. Aborting for safety.")
    backup_dir = args.backup_dir or os.path.join(os.getcwd(), "rekordbox_backups")
    try:
        dest = backup_db(db_file, backup_dir)
    except Exception as e:  # noqa: BLE001
        sys.exit(f"Backup failed ({e}). Aborting without writing anything.")
    print(f"backed up master.db -> {dest}")

    changed = 0
    for c, _, new_name, _, new_path, title in plan:
        c.FolderPath = new_path
        if hasattr(c, "FileNameL"):
            c.FileNameL = new_name
        if hasattr(c, "FileNameS"):
            c.FileNameS = new_name
        if args.set_title and title:
            c.Title = title
        changed += 1
    db.commit()
    print(f"\nrelinked {changed} tracks. Backup at: {dest}")
    print("Next: in Rekordbox, select these tracks and use 'Reload Tag' to pull "
          "Artist/Title/etc from the files (they already carry correct tags).")


if __name__ == "__main__":
    main()
