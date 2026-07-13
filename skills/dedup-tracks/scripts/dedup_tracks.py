#!/usr/bin/env python3
"""Find duplicate songs in a music folder and (optionally) move the extras aside.

Two kinds of duplicates are detected:

1. Exact file duplicates - byte-for-byte identical files (grouped by size, then
   hashed). These are certain duplicates.
2. Same-song duplicates - files whose Artist + Title (including the mix/version)
   match after normalisation, even across formats (e.g. a .wav and a .mp3 of the
   same track). Different mixes (Original vs Extended) are kept separate.

By default nothing is changed: a report CSV is written. With --move the extras
are relocated into a folder so the action is fully reversible. Deletion requires
the explicit --delete flag.

Uses ffprobe (from ffmpeg) to read tags when available; otherwise it reads the
Artist/Title from the filename ("Artist - Title.ext"). No other dependencies.

With --rekordbox-db the tool instead deduplicates ENTRIES inside the Rekordbox
collection (master.db): several rows that point at the same file on disk (path
case / slash-direction variants), or at different files of the same song. The
keeper is the best-quality entry; on a tie, the one with the most info (cues,
playlist memberships, play count, rating, comment), then the oldest. Redundant
rows are removed and their playlist memberships (and cues, if the keeper has
none) are transferred to the keeper. Needs pyrekordbox, Rekordbox closed, and
is a dry run by default; master.db is always backed up before writing.
"""
import argparse
import csv
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import unicodedata
from collections import defaultdict

AUDIO_EXT = (".mp3", ".wav", ".aiff", ".aif", ".aifc", ".flac", ".m4a", ".ogg")
# lossless first: used to decide which copy to keep
QUALITY_RANK = {".wav": 3, ".aiff": 3, ".aif": 3, ".aifc": 3, ".flac": 3,
                ".m4a": 1, ".ogg": 1, ".mp3": 1}


def strip_accents(s):
    return "".join(c for c in unicodedata.normalize("NFKD", s)
                   if unicodedata.category(c) != "Mn")


def norm_key(artist, title):
    """Normalised 'artist|title' key: lowercase, no accents/punctuation."""
    def clean(x):
        x = strip_accents(x).lower()
        x = re.sub(r"[^a-z0-9]+", " ", x)
        return re.sub(r"\s+", " ", x).strip()
    return clean(artist) + "|" + clean(title)


def read_tags(path):
    try:
        r = subprocess.run(
            ["ffprobe", "-v", "quiet", "-print_format", "json",
             "-show_format", "-show_entries", "format=duration:format_tags=artist,title",
             path], capture_output=True, text=True, timeout=15)
        d = json.loads(r.stdout).get("format", {})
        tags = {k.lower(): v for k, v in d.get("tags", {}).items()}
        return tags.get("artist", ""), tags.get("title", ""), d.get("duration")
    except Exception:  # noqa: BLE001
        return "", "", None


def from_filename(fn):
    base = os.path.splitext(fn)[0]
    if " - " in base:
        a, t = base.split(" - ", 1)
        return a.strip(), t.strip()
    return "", base.strip()


def sha1_file(path, chunk=1 << 20):
    h = hashlib.sha1()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(chunk), b""):
            h.update(block)
    return h.hexdigest()


def keeper_score(path, duration):
    """Higher is better: prefer lossless, then longer, then larger file."""
    ext = os.path.splitext(path)[1].lower()
    size = os.path.getsize(path)
    return (QUALITY_RANK.get(ext, 0), float(duration or 0), size)


def scan(folder, recursive, use_ffprobe):
    files = []
    walker = os.walk(folder) if recursive else [(folder, [], os.listdir(folder))]
    for root, _, names in walker:
        for n in names:
            p = os.path.join(root, n)
            if os.path.isfile(p) and n.lower().endswith(AUDIO_EXT):
                files.append(p)
    infos = []
    for p in files:
        a = t = ""
        dur = None
        if use_ffprobe:
            a, t, dur = read_tags(p)
        if not a or not t:
            fa, ft = from_filename(os.path.basename(p))
            a = a or fa
            t = t or ft
        infos.append({"path": p, "artist": a, "title": t,
                      "dur": dur, "size": os.path.getsize(p)})
    return infos


def find_exact(infos):
    """Byte-identical files, grouped by size then hash."""
    by_size = defaultdict(list)
    for i in infos:
        by_size[i["size"]].append(i)
    groups = []
    for size, items in by_size.items():
        if len(items) < 2:
            continue
        by_hash = defaultdict(list)
        for it in items:
            by_hash[sha1_file(it["path"])].append(it)
        for h, dups in by_hash.items():
            if len(dups) > 1:
                groups.append(dups)
    return groups


def find_same_song(infos):
    by_key = defaultdict(list)
    for i in infos:
        if i["title"]:
            by_key[norm_key(i["artist"], i["title"])].append(i)
    return [items for items in by_key.values() if len(items) > 1]


def choose(group):
    ranked = sorted(group, key=lambda i: keeper_score(i["path"], i["dur"]),
                    reverse=True)
    return ranked[0], ranked[1:]


# ---------------------------------------------------------------------------
# Rekordbox database mode (--rekordbox-db)
# ---------------------------------------------------------------------------

def rb_path_key(p):
    """Canonical form of a stored path so case / slash variants collide."""
    p = unicodedata.normalize("NFC", p or "")
    return os.path.normcase(os.path.normpath(p))


def rb_info_score(entry):
    """How much user data an entry carries (higher = more worth keeping)."""
    return (len(entry["cues"]) * 3 + len(entry["playlists"]) * 2
            + entry["plays"] + (1 if entry["rating"] else 0)
            + (1 if entry["comment"] else 0))


def rb_keeper(group):
    """Best quality first; ties by most info, then oldest entry."""
    ranked = sorted(
        group,
        key=lambda e: (-QUALITY_RANK.get(e["ext"], 0), -rb_info_score(e),
                       e["created"] or "", e["id"]))
    return ranked[0], ranked[1:]


def rb_open_database(path=None):
    """Open the Rekordbox database; `path` overrides the auto-detected one."""
    try:
        from pyrekordbox import MasterDatabase as _DB  # newer pyrekordbox
    except ImportError:
        try:
            from pyrekordbox import Rekordbox6Database as _DB  # older
        except ImportError:
            sys.exit("pyrekordbox is not installed. Run:  pip install pyrekordbox")
    try:
        return _DB(path) if path else _DB()
    except Exception as e:  # noqa: BLE001
        sys.exit(f"Could not open the Rekordbox database: {e}\n"
                 "Make sure Rekordbox is installed and fully CLOSED.")


def rb_locate_db_file(db, override=None):
    if override:
        return override if os.path.isfile(override) else None
    for attr in ("db_path", "path", "filename", "_db_path", "_path"):
        p = getattr(db, attr, None)
        if p and os.path.isfile(str(p)):
            return str(p)
    try:
        from pyrekordbox.config import get_config
        for section in ("rekordbox7", "rekordbox6", "rekordbox5"):
            try:
                p = get_config(section, "db_path")
            except Exception:  # noqa: BLE001 - empty section raises KeyError
                continue
            if p and os.path.isfile(str(p)):
                return str(p)
    except Exception:  # noqa: BLE001
        pass
    return None


def rb_backup_db(db_file, backup_dir):
    import datetime as dt
    os.makedirs(backup_dir, exist_ok=True)
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    dest = os.path.join(backup_dir, f"master.db.{stamp}.bak")
    shutil.copy2(db_file, dest)
    for ext in ("-wal", "-shm"):
        side = db_file + ext
        if os.path.exists(side):
            shutil.copy2(side, dest + ext)
    return dest


def rb_load_entries(db, folder_filter):
    """One dict per collection entry, with the data the keeper choice needs."""
    from pyrekordbox.db6 import tables
    sp_by_content = defaultdict(list)
    for sp in db.session.query(tables.DjmdSongPlaylist):
        sp_by_content[sp.ContentID].append(sp)
    entries = []
    for c in db.get_content():
        path = c.FolderPath or ""
        if not path or path.startswith("spotify:"):
            continue
        if folder_filter and folder_filter.lower() not in path.lower():
            continue
        entries.append({
            "row": c, "id": c.ID, "path": path,
            "ext": os.path.splitext(path)[1].lower(),
            "title": c.Title or "", "created": str(c.created_at or ""),
            "cues": list(c.Cues or []),
            "playlists": sp_by_content.get(c.ID, []),
            "plays": int(c.DJPlayCount or 0),
            "rating": int(c.Rating or 0),
            "comment": (c.Commnt or "").strip(),
        })
    return entries


def rb_find_groups(entries):
    """[(kind, [entries])] - same-file first, then same-song across the rest."""
    groups = []
    grouped_ids = set()
    by_path = defaultdict(list)
    for e in entries:
        by_path[rb_path_key(e["path"])].append(e)
    for items in by_path.values():
        if len(items) > 1:
            groups.append(("same-file", items))
            grouped_ids.update(e["id"] for e in items)
    by_song = defaultdict(list)
    for e in entries:
        if e["id"] in grouped_ids:
            continue
        a, t = from_filename(os.path.basename(e["path"]))
        if t:
            by_song[norm_key(a, t)].append(e)
    for items in by_song.values():
        if len(items) > 1 and len({rb_path_key(e["path"]) for e in items}) > 1:
            groups.append(("same-song", items))
    return groups


def rb_merge_into_keeper(db, keeper, extra):
    """Move the extra's playlist rows (and cues, if useful) onto the keeper."""
    keeper_playlists = {sp.PlaylistID for sp in keeper["playlists"]}
    for sp in extra["playlists"]:
        if sp.PlaylistID in keeper_playlists:
            db.delete(sp)
        else:
            sp.ContentID = keeper["id"]
            keeper["playlists"].append(sp)
            keeper_playlists.add(sp.PlaylistID)
    if not keeper["cues"] and extra["cues"]:
        for cue in extra["cues"]:
            cue.ContentID = keeper["id"]
        # flush the repoint and drop the stale relationship collection, or
        # SQLAlchemy's cascade NULLs the cues again when the row is deleted
        db.session.flush()
        db.session.expire(extra["row"], ["Cues"])
        keeper["cues"] = extra["cues"]
        extra["cues"] = []
    for cue in extra["cues"]:
        db.delete(cue)
    db.delete(extra["row"])


def rekordbox_mode(args):
    db = rb_open_database(args.db)
    db_file = rb_locate_db_file(db, args.db)
    print(f"master.db: {db_file or 'NOT FOUND (use --db to set it explicitly)'}")

    entries = rb_load_entries(db, args.folder)
    groups = [(kind, *rb_keeper(g)) for kind, g in rb_find_groups(entries)]

    report = args.report or os.path.join(os.getcwd(), "rekordbox_duplicates.csv")
    n_extra = 0
    with open(report, "w", newline="", encoding="utf-8-sig") as fh:
        w = csv.writer(fh)
        w.writerow(["group", "kind", "role", "entry_id", "path", "cues",
                    "playlists", "plays", "rating", "created"])
        for gid, (kind, keeper, extras) in enumerate(groups, 1):
            for role, e in [("keep", keeper)] + [("remove", x) for x in extras]:
                w.writerow([gid, kind, role, e["id"], e["path"], len(e["cues"]),
                            len(e["playlists"]), e["plays"], e["rating"],
                            e["created"]])
            n_extra += len(extras)

    print(f"entries scanned  : {len(entries)}"
          + (f" (folder filter: {args.folder})" if args.folder else ""))
    print(f"duplicate sets   : {len(groups)}")
    print(f"entries to remove: {n_extra}")
    print(f"report           : {report}")
    for kind, keeper, extras in groups[:10]:
        print(f"  [{kind}] keep {os.path.basename(keeper['path'])} "
              f"(cues={len(keeper['cues'])}, playlists={len(keeper['playlists'])})"
              f" - remove {len(extras)} entr{'y' if len(extras) == 1 else 'ies'}")
    if len(groups) > 10:
        print(f"  ... and {len(groups) - 10} more sets")

    if not args.apply:
        print("\nDry run only. Re-run with --apply to remove the duplicate "
              "entries (playlist memberships move to the kept entry).")
        return
    if not groups:
        print("Nothing to do.")
        return
    if not db_file:
        sys.exit("Could not locate master.db to back it up. Pass it explicitly "
                 "with --db. Aborting for safety.")
    if not args.yes:
        ans = input("\nIs Rekordbox FULLY CLOSED? Type 'yes' to continue: ").strip().lower()
        if ans != "yes":
            sys.exit("Aborted. Close Rekordbox and run again.")

    backup_dir = args.backup_dir or os.path.join(os.getcwd(), "rekordbox_backups")
    try:
        dest = rb_backup_db(db_file, backup_dir)
    except Exception as e:  # noqa: BLE001
        sys.exit(f"Backup failed ({e}). Aborting without writing anything.")
    print(f"backed up master.db -> {dest}")

    removed = 0
    for _, keeper, extras in groups:
        for extra in extras:
            rb_merge_into_keeper(db, keeper, extra)
            removed += 1
    db.commit()
    print(f"\nremoved {removed} duplicate entries. Backup at: {dest}")


def main():
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("folder", nargs="?", default=None,
                    help="Folder to scan (with --rekordbox-db: only touch "
                         "entries whose stored path contains this string)")
    ap.add_argument("--recursive", action="store_true",
                    help="Recurse into sub-folders")
    ap.add_argument("--mode", choices=["exact", "song", "both"], default="both",
                    help="Which duplicates to detect (default: both)")
    ap.add_argument("--report", default=None,
                    help="Where to write the report CSV "
                         "(default: <folder>/duplicates.csv)")
    ap.add_argument("--move", metavar="DEST", default=None,
                    help="Move the extra copies into DEST (reversible)")
    ap.add_argument("--delete", action="store_true",
                    help="Delete the extra copies (dangerous; use --move instead)")
    ap.add_argument("--rekordbox-db", action="store_true",
                    help="Deduplicate entries inside the Rekordbox collection "
                         "instead of files on disk (dry run unless --apply)")
    ap.add_argument("--db", default=None,
                    help="Explicit path to master.db (only needed if "
                         "auto-detect fails; --rekordbox-db only)")
    ap.add_argument("--apply", action="store_true",
                    help="Actually write to the database (--rekordbox-db only)")
    ap.add_argument("--backup-dir", default=None,
                    help="Where to store the master.db backup "
                         "(default: ./rekordbox_backups; --rekordbox-db only)")
    ap.add_argument("--yes", action="store_true",
                    help="Skip the interactive 'Rekordbox is closed' prompt "
                         "(--rekordbox-db only)")
    args = ap.parse_args()

    if args.rekordbox_db:
        rekordbox_mode(args)
        return

    if not args.folder or not os.path.isdir(args.folder):
        sys.exit(f"Not a folder: {args.folder}")
    use_ffprobe = shutil.which("ffprobe") is not None

    infos = scan(args.folder, args.recursive, use_ffprobe)
    groups = []
    seen_paths = set()

    def add_groups(found, kind):
        for g in found:
            keeper, extras = choose(g)
            # avoid reporting the same file twice across exact+song passes
            fresh = [e for e in extras if e["path"] not in seen_paths]
            if not fresh:
                continue
            for e in fresh:
                seen_paths.add(e["path"])
            groups.append((kind, keeper, fresh))

    if args.mode in ("exact", "both"):
        add_groups(find_exact(infos), "exact")
    if args.mode in ("song", "both"):
        add_groups(find_same_song(infos), "same-song")

    report = args.report or os.path.join(args.folder, "duplicates.csv")
    n_extra = 0
    with open(report, "w", newline="", encoding="utf-8-sig") as fh:
        w = csv.writer(fh)
        w.writerow(["group", "kind", "role", "file", "artist", "title",
                    "ext", "size_bytes"])
        for gid, (kind, keeper, extras) in enumerate(groups, 1):
            for role, it in [("keep", keeper)] + [("duplicate", e) for e in extras]:
                w.writerow([gid, kind, role, it["path"], it["artist"],
                            it["title"], os.path.splitext(it["path"])[1].lower(),
                            it["size"]])
                if role == "duplicate":
                    n_extra += 1

    print(f"tag source     : {'ffprobe' if use_ffprobe else 'filenames only'}")
    print(f"files scanned  : {len(infos)}")
    print(f"duplicate sets : {len(groups)}")
    print(f"extra copies   : {n_extra}")
    print(f"report         : {report}")

    if not args.move and not args.delete:
        print("\nReport only. Use --move DEST to relocate the extra copies, "
              "or --delete to remove them.")
        return

    if args.move:
        os.makedirs(args.move, exist_ok=True)
    acted = 0
    for _, (kind, keeper, extras) in enumerate(groups):
        for e in extras:
            if args.delete and not args.move:
                os.remove(e["path"])
            else:
                dest = os.path.join(args.move, os.path.basename(e["path"]))
                root, ext = os.path.splitext(dest)
                n = 2
                while os.path.exists(dest):
                    dest = f"{root} ({n}){ext}"
                    n += 1
                shutil.move(e["path"], dest)
            acted += 1
    print(f"\n{'deleted' if (args.delete and not args.move) else 'moved'} "
          f"{acted} extra copies.")


if __name__ == "__main__":
    main()
