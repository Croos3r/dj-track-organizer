#!/usr/bin/env python3
"""Normalize music filenames to a consistent "Artist - Title (Mix)" scheme.

Reads embedded tags with ffprobe when available and falls back to parsing the
existing filename. Produces a dry-run plan by default; pass --apply to rename.

No third-party dependencies. ffprobe (from ffmpeg) is optional but recommended.
"""
import argparse
import csv
import json
import os
import re
import shutil
import subprocess
import sys
import time
import unicodedata
from collections import Counter

AUDIO_EXT = (".mp3", ".wav", ".aiff", ".aif", ".flac", ".m4a", ".ogg")
MIX_KW = (r"(mix|remix|edit|bootleg|rework|version|vip|flip|radio|extended|"
          r"original|cut|remaster|dub|refix|mashup|remake|anthem|instrumental)")
ILLEGAL = r'[\\/:*?"<>|]'


def strip_junk(s):
    """Drop control/format characters (zero-width spaces etc.), normalise spaces."""
    s = s.replace(" ", " ")
    s = "".join(c for c in s if unicodedata.category(c)[0] != "C")
    return re.sub(r"\s+", " ", s).strip()


def clean_ws(s):
    return strip_junk(s.replace("_", " "))


def std_feat(s):
    """Unify featuring / ft / ft. into a single 'feat.' form."""
    s = re.sub(r"\b(feat(?:uring|s)?|ft)\b\.?", "feat.", s, flags=re.I)
    s = re.sub(r"\s*\bfeat\.\s*", " feat. ", s, flags=re.I)
    return re.sub(r"\s+", " ", s).strip()


def _sort_names(s):
    return ", ".join(sorted([p.strip() for p in s.split(",") if p.strip()],
                            key=str.lower))


def sort_artists(a, alphabetical=True):
    """Sort comma-separated artists. '&' groups stay intact as one act name."""
    a = std_feat(a)
    if not alphabetical:
        return a
    m = re.search(r"\s+feat\.\s+", a, flags=re.I)
    if m:
        return _sort_names(a[:m.start()]) + " feat. " + _sort_names(a[m.end():])
    return _sort_names(a)


def dedupe_parens(t):
    """Remove repeated parenthetical groups, e.g. '(Original Mix) (Original Mix)'."""
    seen, out = set(), []
    for p in re.split(r"(\([^)]*\))", t):
        if p.startswith("(") and p.endswith(")"):
            k = re.sub(r"[^a-z0-9]", "", p.lower())
            if k and k in seen:
                continue
            seen.add(k)
        out.append(p)
    return re.sub(r"\s+", " ", "".join(out)).strip()


def sanitize(name):
    return strip_junk(re.sub(ILLEGAL, "", name)).rstrip(". ")


def titlecase_slug(s):
    out = " ".join(w.capitalize() for w in s.replace("-", " ").split())
    return re.sub(r"\bDj\b", "DJ", out)


def is_slug(b):
    return bool(re.match(r"^[a-z0-9]+(-[a-z0-9.]+)+$", b))


def parse_from_filename(fn):
    """Best-effort (artist, title, confident) from a filename without good tags."""
    base = strip_junk(re.sub(r"\.[^.]+$", "", fn))
    base = re.sub(r"^\d{1,2}[\s_.\-]+", "", base)  # leading track number
    base = re.sub(r"(-(?:original|extended|radio)-mix)\1", r"\1", base, flags=re.I)
    if is_slug(base):
        return None, titlecase_slug(base), False
    # drop label codes like -VCU017- inside compilation names
    b = re.sub(r"\s*-\s*[A-Z]{2,6}\d{2,4}\s*-\s*", " - ", base)
    segs = [re.sub(r"^-+\s*|\s*-+$", "", s).strip() for s in b.split(" - ")]
    segs = [s for s in segs if s]
    # compilation: "Artist - Album - NN Track Title"
    if len(segs) >= 3 and re.match(r"^\d{1,2}\s+\S", segs[-1]):
        return segs[0], re.sub(r"^\d{1,2}\s+", "", segs[-1]), True
    if " - " in base:
        a, t = base.split(" - ", 1)
        return a.strip(), t.strip(), True
    return None, base.strip(), False


def read_tags(path):
    try:
        r = subprocess.run(
            ["ffprobe", "-v", "quiet", "-print_format", "json",
             "-show_format", path],
            capture_output=True, text=True, timeout=15)
        tags = json.loads(r.stdout).get("format", {}).get("tags", {})
        low = {k.lower(): v for k, v in tags.items()}
        return low.get("artist", ""), low.get("title", "")
    except Exception:
        return "", ""


def add_missing_mix(title, fn_title):
    """If the tag title has no mix suffix but the filename does, keep it."""
    if not fn_title or re.search(r"\([^)]*" + MIX_KW + r"[^)]*\)", title, flags=re.I):
        return title
    for p in re.findall(r"\([^)]*\)", fn_title):
        if re.search(MIX_KW, p, flags=re.I):
            title = (title + " " + p).strip()
    return title


def has_ffprobe():
    return shutil.which("ffprobe") is not None


def build_name(path, source, alphabetical, keep_mix, use_ffprobe):
    fn = os.path.basename(path)
    ext = os.path.splitext(fn)[1].lower()
    art = tit = ""
    if use_ffprobe:
        art, tit = read_tags(path)
        art, tit = clean_ws(art), clean_ws(tit)
    fa, ft, _ = parse_from_filename(fn)

    if source == "filename":
        A, T = (fa or art or ""), (ft or tit or "")
        origin = "filename"
    else:  # tags preferred
        if art and tit:
            A, T = art, (add_missing_mix(tit, ft) if keep_mix else tit)
            origin = "tags"
        else:
            A, T = (art or fa or ""), (tit or ft or "")
            origin = "mixed" if (art or tit) else "filename"

    if A:
        A = sort_artists(A, alphabetical)
    T = std_feat(T)
    if A and T:
        return sanitize(dedupe_parens(f"{A} - {T}")) + ext, origin
    return "", "manual"


def build_plan(folder, source, alphabetical, keep_mix):
    use_ffprobe = has_ffprobe()
    files = sorted(f for f in os.listdir(folder)
                   if f.lower().endswith(AUDIO_EXT)
                   and os.path.isfile(os.path.join(folder, f)))
    rows = []
    for fn in files:
        new, origin = build_name(os.path.join(folder, fn), source,
                                 alphabetical, keep_mix, use_ffprobe)
        rows.append({"old": fn, "new": new, "source": origin})

    # resolve duplicate target names with " (2)", " (3)" suffixes
    counts = Counter(r["new"].lower() for r in rows if r["new"])
    dups = {k for k, v in counts.items() if v > 1}
    seen = {}
    for r in rows:
        if r["new"] and r["new"].lower() in dups:
            k = r["new"].lower()
            seen[k] = seen.get(k, 0) + 1
            if seen[k] > 1:
                root, ext = os.path.splitext(r["new"])
                r["new"] = f"{root} ({seen[k]}){ext}"
    return rows, use_ffprobe


def two_phase_rename(folder, changes):
    """Rename via temp names first so no target can overwrite another file."""
    tmp = []
    for i, (old, new) in enumerate(changes):
        src = os.path.join(folder, old)
        t = os.path.join(folder, f".__norm_{i}_{int(time.time())}.tmp")
        os.rename(src, t)
        tmp.append((t, new, old))
    done, skipped = [], []
    for t, new, old in tmp:
        dst = os.path.join(folder, new)
        if os.path.exists(dst):
            os.rename(t, os.path.join(folder, old))
            skipped.append((old, new))
        else:
            os.rename(t, dst)
            done.append((old, new))
    return done, skipped


def main():
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("folder", help="Folder containing the audio files")
    ap.add_argument("--apply", action="store_true",
                    help="Actually rename (default is a dry run)")
    ap.add_argument("--source", choices=["tags", "filename"], default="tags",
                    help="Preferred source of truth on conflict (default: tags)")
    ap.add_argument("--no-alphabetical", action="store_true",
                    help="Keep the original artist order instead of sorting")
    ap.add_argument("--drop-mix", action="store_true",
                    help="Do not re-attach a mix suffix found only in the filename")
    ap.add_argument("--plan", default=None,
                    help="Where to write the plan CSV "
                         "(default: <folder>/rename_plan.csv)")
    args = ap.parse_args()

    if not os.path.isdir(args.folder):
        sys.exit(f"Not a folder: {args.folder}")

    rows, used = build_plan(args.folder, args.source,
                            not args.no_alphabetical, not args.drop_mix)
    changes = [(r["old"], r["new"]) for r in rows
               if r["new"] and r["new"] != r["old"]]
    manual = [r for r in rows if not r["new"]]
    same = sum(1 for r in rows if r["new"] == r["old"])

    plan_path = args.plan or os.path.join(args.folder, "rename_plan.csv")
    with open(plan_path, "w", newline="", encoding="utf-8-sig") as fh:
        w = csv.writer(fh)
        w.writerow(["OLD name", "NEW name", "from"])
        for r in rows:
            if r["new"] and r["new"] != r["old"]:
                w.writerow([r["old"], r["new"], r["source"]])

    src_label = "ffprobe embedded tags" if used else "filenames only (ffprobe not found)"
    print(f"tag source        : {src_label}")
    print(f"total audio files : {len(rows)}")
    print(f"to rename         : {len(changes)}")
    print(f"already correct   : {same}")
    print(f"needs manual name : {len(manual)}")
    print(f"plan written to   : {plan_path}")

    if not args.apply:
        print("\nDry run only. Review the plan, then re-run with --apply.")
        return

    done, skipped = two_phase_rename(args.folder, changes)
    rb = os.path.join(args.folder, "rename_rollback.csv")
    with open(rb, "w", newline="", encoding="utf-8-sig") as fh:
        w = csv.writer(fh)
        w.writerow(["current_name", "restore_to"])
        for old, new in done:
            w.writerow([new, old])
    print(f"\nrenamed  : {len(done)}")
    print(f"skipped  : {len(skipped)} (target already existed)")
    print(f"rollback : {rb}")
    if manual:
        print(f"\n{len(manual)} file(s) left untouched "
              f"(could not derive Artist - Title):")
        for r in manual:
            print(f"  {r['old']}")


if __name__ == "__main__":
    main()
