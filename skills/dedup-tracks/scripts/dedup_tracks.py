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


def main():
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("folder", help="Folder to scan")
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
    args = ap.parse_args()

    if not os.path.isdir(args.folder):
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
