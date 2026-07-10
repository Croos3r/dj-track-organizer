#!/usr/bin/env python3
"""Write Artist and Title tags into audio files, derived from their filenames.

Supports WAV (RIFF INFO), MP3 (ID3v2.4) and AIFF/AIFC (ID3 chunk) with no
third-party dependencies and without re-encoding the audio. Existing cue
points and loop chunks on WAV/AIFF files are preserved.

Filename convention:  "Artist - Title.ext"  ->  artist / title
Everything after the first " - " (including a "(Mix)" suffix) becomes the title.
"""
import argparse
import os
import struct
import sys

SUPPORTED = (".wav", ".mp3", ".aiff", ".aif", ".aifc")


# --------------------------------------------------------------------------- #
# Filename parsing
# --------------------------------------------------------------------------- #
def parse_name(fn):
    base = os.path.splitext(fn)[0]
    if " - " in base:
        a, t = base.split(" - ", 1)
        return a.strip(), t.strip()
    return "", base.strip()


# --------------------------------------------------------------------------- #
# ID3v2.4 (used by MP3 directly, and inside the AIFF 'ID3 ' chunk)
# --------------------------------------------------------------------------- #
def _synchsafe(n):
    return bytes([(n >> 21) & 0x7F, (n >> 14) & 0x7F, (n >> 7) & 0x7F, n & 0x7F])


def _text_frame(fid, text):
    data = b"\x03" + text.encode("utf-8") + b"\x00"  # 0x03 = UTF-8
    return fid + _synchsafe(len(data)) + b"\x00\x00" + data


def build_id3(artist, title, pad_to=None):
    frames = _text_frame(b"TIT2", title)
    if artist:
        frames += _text_frame(b"TPE1", artist)
    total = len(frames) if pad_to is None else pad_to - 10
    padding = b"\x00" * (total - len(frames))
    return b"ID3\x04\x00\x00" + _synchsafe(total) + frames + padding


def _id3_len_at_front(head10):
    if head10[:3] != b"ID3":
        return 0
    size = ((head10[6] << 21) | (head10[7] << 14) |
            (head10[8] << 7) | head10[9])
    start = 10 + size
    if head10[5] & 0x10:  # footer present
        start += 10
    return start


# --------------------------------------------------------------------------- #
# MP3
# --------------------------------------------------------------------------- #
def write_mp3(path, artist, title):
    with open(path, "rb") as f:
        head = f.read(10)
    old_len = _id3_len_at_front(head)
    frames = build_id3(artist, title)
    if old_len >= len(frames):
        # existing tag has room: overwrite in place, no audio movement
        tag = build_id3(artist, title, pad_to=old_len)
        with open(path, "r+b") as f:
            f.seek(0)
            f.write(tag)
    else:
        with open(path, "rb") as f:
            buf = f.read()
        audio = buf[_id3_len_at_front(buf):]
        tmp = path + ".tmp"
        with open(tmp, "wb") as f:
            f.write(frames + audio)
        os.replace(tmp, path)


# --------------------------------------------------------------------------- #
# RIFF (WAV) and IFF (AIFF) shared chunk parsing
# --------------------------------------------------------------------------- #
def _read_chunks(path, big_endian):
    """Return (form_type, size_of_riff_field, filesize, [(id,pos,size)...])."""
    order = ">" if big_endian else "<"
    sz = os.path.getsize(path)
    chunks = []
    with open(path, "rb") as f:
        magic = f.read(4)
        form_size = struct.unpack(order + "I", f.read(4))[0]
        form_type = f.read(4)
        pos = 12
        while pos < sz - 8:
            f.seek(pos)
            cid = f.read(4)
            if len(cid) < 4:
                break
            csz = struct.unpack(order + "I", f.read(4))[0]
            chunks.append((cid, pos, csz))
            pos += 8 + csz + (csz & 1)
    return magic, form_type, form_size, sz, chunks


def _build_riff_info(artist, title):
    def sub(cid, val):
        b = val.encode("utf-8") + b"\x00"
        if len(b) & 1:
            b += b"\x00"
        return cid + struct.pack("<I", len(b)) + b
    body = b"INFO"
    if artist:
        body += sub(b"IART", artist)
    if title:
        body += sub(b"INAM", title)
    return b"LIST" + struct.pack("<I", len(body)) + body


def write_wav(path, artist, title):
    magic, _, _, sz, ch = _read_chunks(path, big_endian=False)
    if magic != b"RIFF" or not ch:
        raise ValueError("not a RIFF/WAV file")
    data_idx = next((i for i, c in enumerate(ch) if c[0] == b"data"), None)
    if data_idx is None:
        raise ValueError("no data chunk")

    info = _build_riff_info(artist, title)
    tail_chunks = ch[data_idx + 1:]
    # Fast path: metadata lives only after the audio -> rewrite just the tail.
    data = ch[data_idx]
    tail_start = data[1] + 8 + data[2] + (data[2] & 1)
    keep = b""
    with open(path, "rb") as f:
        for cid, pos, csz in tail_chunks:
            f.seek(pos)
            raw = f.read(8 + csz + (csz & 1))
            if cid == b"id3 ":            # drop stale ID3, we standardise on INFO
                continue
            if cid == b"LIST":
                f.seek(pos + 8)
                if f.read(4) == b"INFO" or csz < 4:  # drop old/empty INFO
                    continue
            keep += raw
    keep += info
    with open(path, "r+b") as f:
        f.truncate(tail_start)
        f.seek(0, 2)
        f.write(keep)
        new_size = f.tell()
        f.seek(4)
        f.write(struct.pack("<I", new_size - 8))


def write_aiff(path, artist, title):
    magic, form_type, _, sz, ch = _read_chunks(path, big_endian=True)
    if magic != b"FORM":
        raise ValueError("not an AIFF file")
    id3 = build_id3(artist, title)
    chunk = b"ID3 " + struct.pack(">I", len(id3)) + id3
    if len(id3) & 1:
        chunk += b"\x00"

    # Drop any existing trailing ID3 chunk, then append the fresh one.
    end = sz
    if ch and ch[-1][0] == b"ID3 ":
        end = ch[-1][1]
    # If an ID3 chunk sits earlier in the file, fall back to a full rewrite.
    if any(c[0] == b"ID3 " for c in ch[:-1]):
        _aiff_full_rewrite(path, artist, title)
        return
    with open(path, "r+b") as f:
        f.truncate(end)
        f.seek(0, 2)
        f.write(chunk)
        new_size = f.tell()
        f.seek(4)
        f.write(struct.pack(">I", new_size - 8))


def _aiff_full_rewrite(path, artist, title):
    magic, form_type, _, sz, ch = _read_chunks(path, big_endian=True)
    id3 = build_id3(artist, title)
    parts = [b"FORM", b"\x00\x00\x00\x00", form_type]
    with open(path, "rb") as f:
        for cid, pos, csz in ch:
            if cid == b"ID3 ":
                continue
            f.seek(pos)
            parts.append(f.read(8 + csz + (csz & 1)))
    chunk = b"ID3 " + struct.pack(">I", len(id3)) + id3
    if len(id3) & 1:
        chunk += b"\x00"
    parts.append(chunk)
    blob = b"".join(parts)
    blob = blob[:4] + struct.pack(">I", len(blob) - 8) + blob[8:]
    tmp = path + ".tmp"
    with open(tmp, "wb") as f:
        f.write(blob)
    os.replace(tmp, path)


# --------------------------------------------------------------------------- #
# Dispatch
# --------------------------------------------------------------------------- #
def tag_file(path):
    artist, title = parse_name(os.path.basename(path))
    if not title:
        return "skip-noname"
    ext = os.path.splitext(path)[1].lower()
    if ext == ".mp3":
        write_mp3(path, artist, title)
    elif ext == ".wav":
        write_wav(path, artist, title)
    elif ext in (".aiff", ".aif", ".aifc"):
        write_aiff(path, artist, title)
    else:
        return "skip-ext"
    return "ok"


def iter_files(folder, recursive):
    if recursive:
        for root, _, files in os.walk(folder):
            for f in files:
                yield os.path.join(root, f)
    else:
        for f in sorted(os.listdir(folder)):
            p = os.path.join(folder, f)
            if os.path.isfile(p):
                yield p


def main():
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("folder", help="Folder containing the audio files")
    ap.add_argument("--dry-run", action="store_true",
                    help="Show what would be written without modifying files")
    ap.add_argument("--recursive", action="store_true",
                    help="Recurse into sub-folders")
    args = ap.parse_args()

    if not os.path.isdir(args.folder):
        sys.exit(f"Not a folder: {args.folder}")

    ok = noname = errs = 0
    error_list = []
    for path in iter_files(args.folder, args.recursive):
        if not path.lower().endswith(SUPPORTED):
            continue
        artist, title = parse_name(os.path.basename(path))
        if not title:
            noname += 1
            continue
        if args.dry_run:
            print(f"{os.path.basename(path):60}  ->  artist={artist!r} title={title!r}")
            ok += 1
            continue
        try:
            tag_file(path)
            ok += 1
        except Exception as e:  # noqa: BLE001 - report and continue
            errs += 1
            error_list.append((os.path.basename(path), str(e)))

    print(f"\n{'would tag' if args.dry_run else 'tagged'} : {ok}")
    print(f"skipped (no ' - ' in name) : {noname}")
    if errs:
        print(f"errors : {errs}")
        for fn, e in error_list[:20]:
            print(f"  {fn}: {e}")


if __name__ == "__main__":
    main()
