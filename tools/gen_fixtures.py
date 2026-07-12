#!/usr/bin/env python3
"""Generate parity fixtures for the Rust port (app/core) from the Python oracle.

The four skills scripts are the reference implementations. This tool runs them
on curated inputs and records the outputs as JSON/binary fixtures under
app/core/tests/fixtures/, which the Rust test suite asserts against.

Usage:  python tools/gen_fixtures.py [normalize|tagging|dedup|all]
"""
import importlib.util
import json
import os
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURES = os.path.join(REPO, "app", "core", "tests", "fixtures")


def load_skill(name, rel_path):
    spec = importlib.util.spec_from_file_location(name, os.path.join(REPO, rel_path))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def write_json(name, data):
    os.makedirs(FIXTURES, exist_ok=True)
    path = os.path.join(FIXTURES, name)
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(data, fh, ensure_ascii=False, indent=1)
    print(f"wrote {path}")


# --------------------------------------------------------------------------- #
# normalize
# --------------------------------------------------------------------------- #

# Curated inputs covering every branch of the normalizer. Grouped by intent;
# all are exercised with the default options and a subset with variations.
NORMALIZE_CASES = [
    # already-correct names (idempotence)
    "Angerfist - And Jesus Wept.wav",
    "Aida Arko, PRYDIE - Hard Disco (Extended Mix).mp3",
    "DJ Furax - Supersaw (The Dark Horror Remix).wav",
    "Alan Stivell, Manau - Tri Martolod x La Tribue de Dana (Sunhiausa Frenchcore Bootleg).wav",
    # numeric artist names must survive (regression: the '12 Inch' bug)
    "12 Inch - Dirtytech (Schranz Edit).wav",
    "13 Block, Niska - Tieks (Von Bikräv Remix).mp3",
    "3 Steps Ahead - In The Name Of Love (Original Mix).mp3",
    "3 Steps Ahead - Money In My Pocket.wav",
    "4.20 - Dream.wav",
    "2AT - Kremayera Rolling.wav",
    "220 BPM JUNKIE, Why T - MOOD SWINGS.mp3",
    "1luu, Aexhy - Moulin Rouge.wav",
    "113 - Princes de la Ville (Von Bikräv Edit).wav",
    # clearly numbered track prefixes must be stripped
    "01 Some Artist - Some Title.mp3",
    "07. Some Artist - Some Title.wav",
    "1. Some Artist - Some Title.mp3",
    "12_Some Artist - Some Title.mp3",
    "3 - Some Artist - Some Title.aiff",
    "09 - Some Artist - Some Title.mp3",
    # artist sorting (commas sorted, & groups intact, case-insensitive)
    "Zeta, Alpha - Together.mp3",
    "Insomniak, Drymk - 1312 (Drymk Remix).wav",
    "Bass D & King Matthew, Aexhy - Rave.wav",
    "delta, Charlie & Echo, bravo - Chain.mp3",
    "Noizer & Drokz - Ouwe Stijl.mp3",
    # feat. normalisation in artist and title
    "Artist ft Other - Song.mp3",
    "Artist ft. Other - Song.wav",
    "Artist Ft. Other - Song.mp3",
    "Artist featuring Other - Song.mp3",
    "Artist feats Other - Song.mp3",
    "Artist feat Other, Zed - Song.mp3",
    "Zed, Artist feat. Beta, Alpha - Song.mp3",
    "Artist - Song feat Other.mp3",
    "Artist - Song (feat. Other) (Remix).mp3",
    # slug filenames get title-cased
    "some-artist-some-title.mp3",
    "dj-gone-wild.mp3",
    "hardcore-4.20-anthem.mp3",
    # compilation "Artist - Album - NN Title"
    "Various - Thunderdome 96 - 01 Intro.mp3",
    "Neophyte - Best Of - 12 The Bike Song.mp3",
    # label codes dropped
    "Ilsa Gold - VCU017 - Up (Airwolf Paranoid Mix).mp3",
    # repeated parenthetical groups collapse
    "Artist - Title (Original Mix) (Original Mix).mp3",
    "Artist - Title (Extended Mix) (extended mix).mp3",
    "Artist - Title (Remix) (Original Mix).mp3",
    # doubled mix-suffix slug form
    "artist-title-original-mix-original-mix.mp3",
    # whitespace / underscores / control characters
    "Some_Artist_-_Some_Title.mp3",
    "  Artist   -   Title  .mp3",
    "Artist​ - Tit​le.mp3",
    "Artist - Title.mp3",
    # accents preserved
    "Sétaou - Prière Païenne (Über Edit).mp3",
    "Ünloco - Café del Mar.wav",
    # multi-dash and odd segment counts
    "Angerfist - Angerfist - Strange Man In Mask.wav",
    "A - B - C - D.mp3",
    "Artist - Title - 2020 Rework.mp3",
    # unbalanced / doubled parens kept as-is (sanitizer only strips illegal chars)
    "4 Tune Fairytales - My Little Fantasy (Bass D & King Matthew Remix)).wav",
    # no ' - ' separator -> needs manual naming
    "Mastering msbtwu.wav",
    "livesetrip.wav",
    # bare title with mix keyword, no artist
    "moonlight-sonata-hardcore-remix.mp3",
    # unicode dashes are not separators (only ' - ')
    "Artist — Title.mp3",
    # trailing dots / spaces sanitised away
    "Artist - Title....mp3",
    # mixed case mix keywords preserved verbatim
    "APY - SENTIMENT HARDCORE.wav",
    "ANILOMOGUH - TSR FRAPCORE.wav",
    # title keeps everything after first ' - '
    "Aiobahn +81, KOTOKO - Internet Yamero (Sunhiausa Tool).wav",
    "A$AP FERG, Lauwend - Work (LAUWEND HARD TECHNO REMIX) EXTENDED.wav",
    # extensions preserved / case-insensitive detection
    "Artist - Title.MP3",
    "Artist - Title.WaV",
    "Artist - Title.aif",
    "Artist - Title.aiff",
    "Artist - Title.flac",
    "Artist - Title.m4a",
    "Artist - Title.ogg",
]

# Filenames containing characters Windows forbids can never appear as real
# files, but the same code path also normalises tag-derived names, so we keep
# them in the string-level fixture only.
NORMALIZE_STRING_ONLY_CASES = [
    'Artist - Ti:tle "quoted".mp3',
    "Artist - Title?*.mp3",
    # NB: no path separators here — Python's build_name basenames its input,
    # so "/" cases are path artifacts, not filename normalization behavior.
]

# (files, options) scenarios exercised through build_plan on a real directory:
# collision handling, already-correct counting, manual names.
NORMALIZE_PLAN_SCENARIOS = [
    {
        "name": "collision_suffixes",
        "files": [
            "Artist - Title.mp3",
            "Artist_-_Title.mp3",
            "Artist  -  Title.mp3",
            "Artist - Title (2).mp3",
        ],
    },
    {
        "name": "mixed_bag",
        "files": [
            "01 Some Artist - Some Title.mp3",
            "12 Inch - Dirtytech (Schranz Edit).wav",
            "Mastering msbtwu.wav",
            "Zeta, Alpha - Together.mp3",
            "Angerfist - And Jesus Wept.wav",
        ],
    },
    {
        "name": "swap_targets",
        "files": [
            "B, A - Song.mp3",
            "A, B - Song (2).mp3",
        ],
    },
]

OPTION_COMBOS = [
    # (source, alphabetical, keep_mix) — default first
    ("tags", True, True),
    ("filename", True, True),
    ("tags", False, True),
    ("tags", True, False),
]


def gen_normalize():
    norm = load_skill(
        "normalize_filenames",
        "skills/normalize-music-filenames/scripts/normalize_filenames.py",
    )
    norm.has_ffprobe = lambda: False  # determinism: filename parsing only

    name_cases = []
    for fn in NORMALIZE_CASES + NORMALIZE_STRING_ONLY_CASES:
        for source, alpha, keep in OPTION_COMBOS:
            new, origin = norm.build_name(fn, source, alpha, keep, False)
            name_cases.append(
                {
                    "file": fn,
                    "source": source,
                    "alphabetical": alpha,
                    "keep_mix": keep,
                    "new": new,
                    "origin": origin,
                }
            )
    write_json("normalize_build_name.json", name_cases)

    plan_cases = []
    for scen in NORMALIZE_PLAN_SCENARIOS:
        with tempfile.TemporaryDirectory() as td:
            for f in scen["files"]:
                open(os.path.join(td, f), "wb").close()
            rows, _ = norm.build_plan(td, "tags", True, True)
        plan_cases.append({"name": scen["name"], "files": scen["files"], "rows": rows})
    write_json("normalize_build_plan.json", plan_cases)


# --------------------------------------------------------------------------- #
# csv artifacts: byte-exact expectations for rename_plan/rename_rollback
# --------------------------------------------------------------------------- #

CSV_PLAN_ROWS = [
    # (old, new, source) — includes comma and quote cases
    ("Insomniak, Drymk - 1312 (Drymk Remix).wav", "Drymk, Insomniak - 1312 (Drymk Remix).wav", "filename"),
    ("01 Some Artist - Some Title.mp3", "Some Artist - Some Title.mp3", "filename"),
    ('Artist - Ti"tle.mp3', "Artist - Title.mp3", "tags"),
    ("Sétaou - Prière Païenne.mp3", "Sétaou - Prière Païenne (Über Edit).mp3", "mixed"),
]


def gen_csvio():
    import csv as _csv
    import io

    buf = io.BytesIO()
    text = io.TextIOWrapper(buf, encoding="utf-8-sig", newline="")
    w = _csv.writer(text)
    w.writerow(["OLD name", "NEW name", "from"])
    for old, new, src in CSV_PLAN_ROWS:
        w.writerow([old, new, src])
    text.flush()
    os.makedirs(FIXTURES, exist_ok=True)
    with open(os.path.join(FIXTURES, "csv_plan_expected.csv"), "wb") as fh:
        fh.write(buf.getvalue())

    buf = io.BytesIO()
    text = io.TextIOWrapper(buf, encoding="utf-8-sig", newline="")
    w = _csv.writer(text)
    w.writerow(["current_name", "restore_to"])
    for old, new, _ in CSV_PLAN_ROWS:
        w.writerow([new, old])
    text.flush()
    with open(os.path.join(FIXTURES, "csv_rollback_expected.csv"), "wb") as fh:
        fh.write(buf.getvalue())
    print("wrote csv_plan_expected.csv / csv_rollback_expected.csv")


# --------------------------------------------------------------------------- #
# tagging / dedup fixtures are added in their phases
# --------------------------------------------------------------------------- #


import struct


def _chunk(cid, body):
    out = cid + struct.pack("<I", len(body)) + body
    if len(body) & 1:
        out += b"\x00"
    return out


def _chunk_be(cid, body):
    out = cid + struct.pack(">I", len(body)) + body
    if len(body) & 1:
        out += b"\x00"
    return out


def _wav(chunks):
    body = b"WAVE" + b"".join(chunks)
    return b"RIFF" + struct.pack("<I", len(body)) + body


def _aiff(chunks):
    body = b"AIFF" + b"".join(chunks)
    return b"FORM" + struct.pack(">I", len(body)) + body


def _fmt():
    return _chunk(b"fmt ", struct.pack("<HHIIHH", 1, 2, 44100, 264600, 6, 24))


def _riff_info(pairs):
    body = b"INFO"
    for cid, val in pairs:
        b = val.encode("utf-8") + b"\x00"
        if len(b) & 1:
            b += b"\x00"
        body += cid + struct.pack("<I", len(b)) + b
    return _chunk(b"LIST", body)


def _smpl():
    return _chunk(b"smpl", struct.pack("<9I", 0, 0, 22676, 60, 0, 0, 0, 1, 0) + struct.pack("<6I", 2, 4, 0, 0xDC_FF_A7, 0, 0))


def _id3_tag(artist, title, pad=0):
    def synchsafe(n):
        return bytes([(n >> 21) & 0x7F, (n >> 14) & 0x7F, (n >> 7) & 0x7F, n & 0x7F])

    def frame(fid, text):
        data = b"\x03" + text.encode("utf-8") + b"\x00"
        return fid + synchsafe(len(data)) + b"\x00\x00" + data

    frames = frame(b"TIT2", title)
    if artist:
        frames += frame(b"TPE1", artist)
    total = len(frames) + pad
    return b"ID3\x04\x00\x00" + synchsafe(total) + frames + b"\x00" * pad


def _mp3_audio():
    # fake MPEG frames: anything not starting with b"ID3"
    return b"\xff\xfb\x90\x64" + bytes(range(256)) * 3


TAGGING_CASES = [
    # (fixture name, file name on disk -> derives artist/title, input bytes)
    ("wav_plain", "Söme Ärtist - Title (Extended Mix).wav",
     lambda: _wav([_fmt(), _chunk(b"data", b"\x01\x02\x03\x04\x05\x06")])),
    ("wav_junk_smpl", "Artist - With Smpl.wav",
     lambda: _wav([_chunk(b"JUNK", b"\x00" * 28), _fmt(),
                   _chunk(b"data", b"\xaa" * 101),  # odd size -> pad byte
                   _smpl()])),
    ("wav_old_info_id3", "New Artist - New Title (VIP).wav",
     lambda: _wav([_fmt(), _chunk(b"data", b"\xbb" * 64), _smpl(),
                   _riff_info([(b"IART", "Old Guy"), (b"INAM", "Old Song")]),
                   _chunk(b"id3 ", _id3_tag("Old Guy", "Old Song"))])),
    ("wav_info_before_data", "Furax Style - Beatport Layout.wav",
     lambda: _wav([_fmt(),
                   _riff_info([(b"IART", "Shop Artist"), (b"ICMT", "Purchased at Beatport.com"),
                               (b"INAM", "Shop Title")]),
                   _chunk(b"data", b"\xcc" * 32), _smpl()])),
    ("wav_empty_info_tail", "Artist - Empty Info.wav",
     lambda: _wav([_fmt(), _chunk(b"data", b"\xdd" * 10), _chunk(b"LIST", b"IN")])),
    ("wav_no_artist", "justatitle.wav",
     lambda: _wav([_fmt(), _chunk(b"data", b"\xee" * 8)])),
    ("mp3_no_id3", "Artist - Fresh.mp3", lambda: _mp3_audio()),
    ("mp3_small_id3", "Artist With A Longer Name - And A Long Title (Extended Club Mix).mp3",
     lambda: _id3_tag("x", "y") + _mp3_audio()),
    ("mp3_big_id3", "A - B.mp3", lambda: _id3_tag("Old Artist", "Old Title", pad=512) + _mp3_audio()),
    ("mp3_id3_footer", "A - B (Remix).mp3",
     lambda: (lambda t: t[:5] + bytes([t[5] | 0x10]) + t[6:] + b"3DI" + t[3:10])(
         _id3_tag("Old", "Old", pad=256)) + _mp3_audio()),
    ("aiff_plain", "Artist - Aiff Title.aiff",
     lambda: _aiff([_chunk_be(b"COMM", struct.pack(">hIh", 2, 100, 24) + b"\x40\x0e\xac\x44\x00\x00\x00\x00\x00\x00"),
                    _chunk_be(b"SSND", b"\x00" * 8 + b"\x11" * 100)])),
    ("aiff_trailing_id3", "Artist - Replace Trailing.aif",
     lambda: _aiff([_chunk_be(b"COMM", b"\x00" * 18),
                    _chunk_be(b"SSND", b"\x00" * 8 + b"\x22" * 33),  # odd -> pad
                    _chunk_be(b"ID3 ", _id3_tag("Old", "Old"))])),
    ("aiff_mid_id3", "Artist - Full Rewrite (Flip).aif",
     lambda: _aiff([_chunk_be(b"COMM", b"\x00" * 18),
                    _chunk_be(b"ID3 ", _id3_tag("Old", "Old")),
                    _chunk_be(b"SSND", b"\x00" * 8 + b"\x33" * 40)])),
]


def gen_tagging():
    tag = load_skill(
        "tag_from_filename", "skills/tag-from-filename/scripts/tag_from_filename.py"
    )
    out_dir = os.path.join(FIXTURES, "tagging")
    os.makedirs(out_dir, exist_ok=True)
    manifest = []
    for name, disk_name, make in TAGGING_CASES:
        data = make()
        with open(os.path.join(out_dir, f"{name}.in"), "wb") as fh:
            fh.write(data)
        with tempfile.TemporaryDirectory() as td:
            p = os.path.join(td, disk_name)
            with open(p, "wb") as fh:
                fh.write(data)
            status = tag.tag_file(p)
            with open(p, "rb") as fh:
                out = fh.read()
        with open(os.path.join(out_dir, f"{name}.out"), "wb") as fh:
            fh.write(out)
        artist, title = tag.parse_name(disk_name)
        manifest.append(
            {"name": name, "file": disk_name, "artist": artist, "title": title,
             "status": status, "changed": out != data}
        )
        print(f"  {name}: {status} ({len(data)} -> {len(out)} bytes)")
    write_json("tagging_manifest.json", manifest)


DEDUP_SCENARIOS = [
    {
        "name": "exact_dupes",
        "recursive": False,
        # (relative path, content marker, size) — same marker+size = same bytes
        "files": [
            ("Alpha - One.wav", b"A", 400),
            ("Alpha - One (2).wav", b"A", 400),      # byte-identical to One
            ("Beta - Two.wav", b"B", 400),            # same size, different bytes
            ("Gamma - Three.mp3", b"C", 100),
        ],
    },
    {
        "name": "same_song_formats",
        "recursive": False,
        "files": [
            ("Artist - Song.wav", b"W", 900),
            ("Artist - Song.mp3", b"M", 300),          # same song, lossy -> extra
            ("Àrtist - Söng!.flac", b"F", 500),        # accents/punct fold to same key
            ("Artist - Song (Original Mix).mp3", b"O", 300),  # different mix: kept
            ("Artist - Song (Extended Mix).mp3", b"E", 300),  # different mix: kept
        ],
    },
    {
        "name": "keeper_by_size",
        "recursive": False,
        "files": [
            ("X - Y.mp3", b"S", 100),
            ("X - Y (rip).mp3", b"L", 5000),           # larger same-rank... different key
            ("Z - Q.mp3", b"1", 100),
            ("Z - Q (2).mp3", b"2", 100),              # same size, different bytes, diff key
            ("Q - Same.mp3", b"s", 200),
            ("Q - Same (3).mp3", b"s", 200),           # exact dupes, mp3
        ],
    },
    {
        "name": "recursive_overlap",
        "recursive": True,
        "files": [
            ("X - Y.wav", b"R", 600),
            ("sub/X - Y.wav", b"R", 600),              # identical AND same song
            ("sub/deeper/Other - Track.mp3", b"T", 150),
        ],
    },
]


def gen_dedup():
    import subprocess

    script = os.path.join(REPO, "skills", "dedup-tracks", "scripts", "dedup_tracks.py")
    out = []
    for scen in DEDUP_SCENARIOS:
        with tempfile.TemporaryDirectory() as td:
            for rel, marker, size in scen["files"]:
                p = os.path.join(td, rel.replace("/", os.sep))
                os.makedirs(os.path.dirname(p), exist_ok=True)
                with open(p, "wb") as fh:
                    fh.write((marker * size)[:size])
            report = os.path.join(td, "duplicates.csv")
            args = [sys.executable, script, td, "--report", report]
            if scen["recursive"]:
                args.append("--recursive")
            env = dict(os.environ, PATH="")  # hide ffprobe if present: filenames only
            r = subprocess.run(args, capture_output=True, text=True, env=env)
            assert r.returncode == 0, r.stderr
            with open(report, "rb") as fh:
                csv_bytes = fh.read()
        # normalise the temp root out of the CSV so fixtures are portable
        norm = csv_bytes.decode("utf-8-sig").replace(td, "<ROOT>").replace("\\", "/")
        out.append(
            {
                "name": scen["name"],
                "recursive": scen["recursive"],
                "files": [[rel, marker.decode(), size] for rel, marker, size in scen["files"]],
                "stdout": r.stdout.replace(td, "<ROOT>").replace("\\", "/"),
                "report": norm,
            }
        )
        print(f"  {scen['name']}: ok")
    write_json("dedup_scenarios.json", out)


if __name__ == "__main__":
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    if which in ("normalize", "all"):
        gen_normalize()
    if which in ("csvio", "all"):
        gen_csvio()
    if which in ("tagging", "all"):
        gen_tagging()
    if which in ("dedup", "all"):
        gen_dedup()
