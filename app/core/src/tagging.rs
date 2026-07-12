// SPDX-License-Identifier: GPL-3.0-only
//! Byte-level Artist/Title tag writing for WAV (RIFF INFO), MP3 (ID3v2.4) and
//! AIFF (ID3 chunk), without re-encoding audio. Cue/loop chunks are preserved.
//!
//! Faithful port of `skills/tag-from-filename/scripts/tag_from_filename.py`,
//! pinned by byte-parity fixtures (see `tests/tagging_parity.rs`).
//!
//! Tag *reading* (used by normalize/dedup when preferring embedded tags) goes
//! through lofty instead of the oracle's optional ffprobe — the one deliberate
//! behavior difference of the port.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const SUPPORTED: [&str; 5] = [".wav", ".mp3", ".aiff", ".aif", ".aifc"];

#[derive(Debug, thiserror::Error)]
pub enum TagError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Format(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagStatus {
    Ok,
    SkipNoName,
    SkipExt,
}

/// Python `parse_name`: `"Artist - Title.ext"` -> (artist, title).
pub fn parse_name(file_name: &str) -> (String, String) {
    let (base, _ext) = crate::normalize::splitext(file_name);
    match base.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (String::new(), base.trim().to_string()),
    }
}

// --------------------------------------------------------------------------- #
// ID3v2.4 (MP3 directly, and inside the AIFF 'ID3 ' chunk)
// --------------------------------------------------------------------------- #

fn synchsafe(n: usize) -> [u8; 4] {
    [
        ((n >> 21) & 0x7F) as u8,
        ((n >> 14) & 0x7F) as u8,
        ((n >> 7) & 0x7F) as u8,
        (n & 0x7F) as u8,
    ]
}

fn text_frame(fid: &[u8; 4], text: &str) -> Vec<u8> {
    let mut data = vec![0x03u8]; // UTF-8
    data.extend_from_slice(text.as_bytes());
    data.push(0);
    let mut out = fid.to_vec();
    out.extend_from_slice(&synchsafe(data.len()));
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&data);
    out
}

fn build_id3(artist: &str, title: &str, pad_to: Option<usize>) -> Vec<u8> {
    let mut frames = text_frame(b"TIT2", title);
    if !artist.is_empty() {
        frames.extend_from_slice(&text_frame(b"TPE1", artist));
    }
    let total = match pad_to {
        None => frames.len(),
        Some(p) => p - 10,
    };
    let mut out = b"ID3\x04\x00\x00".to_vec();
    out.extend_from_slice(&synchsafe(total));
    out.extend_from_slice(&frames);
    out.resize(10 + total, 0);
    out
}

fn id3_len_at_front(head10: &[u8]) -> usize {
    if head10.len() < 10 || &head10[..3] != b"ID3" {
        return 0;
    }
    let size = ((head10[6] as usize) << 21)
        | ((head10[7] as usize) << 14)
        | ((head10[8] as usize) << 7)
        | head10[9] as usize;
    let mut start = 10 + size;
    if head10[5] & 0x10 != 0 {
        start += 10; // footer present
    }
    start
}

// --------------------------------------------------------------------------- #
// MP3
// --------------------------------------------------------------------------- #

fn write_mp3(path: &Path, artist: &str, title: &str) -> Result<(), TagError> {
    let mut head = [0u8; 10];
    {
        let mut f = std::fs::File::open(path)?;
        let n = f.read(&mut head)?;
        if n < 10 {
            head[n..].fill(0);
        }
    }
    let old_len = id3_len_at_front(&head);
    let frames = build_id3(artist, title, None);
    if old_len >= frames.len() {
        // existing tag has room: overwrite in place, no audio movement
        let tag = build_id3(artist, title, Some(old_len));
        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&tag)?;
    } else {
        let buf = std::fs::read(path)?;
        let audio = &buf[id3_len_at_front(&buf).min(buf.len())..];
        let tmp = path.with_file_name(format!(
            "{}.tmp",
            path.file_name().unwrap().to_string_lossy()
        ));
        let mut out = frames;
        out.extend_from_slice(audio);
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(())
}

// --------------------------------------------------------------------------- #
// RIFF (WAV) and IFF (AIFF) shared chunk parsing
// --------------------------------------------------------------------------- #

#[derive(Clone, Copy, Debug)]
struct Chunk {
    id: [u8; 4],
    pos: u64,
    size: u32,
}

struct IffFile {
    magic: [u8; 4],
    form_type: [u8; 4],
    file_size: u64,
    chunks: Vec<Chunk>,
}

fn read_chunks(path: &Path, big_endian: bool) -> Result<IffFile, TagError> {
    let file_size = std::fs::metadata(path)?.len();
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head)
        .map_err(|_| TagError::Format("file too small".into()))?;
    let magic = [head[0], head[1], head[2], head[3]];
    let form_type = [head[8], head[9], head[10], head[11]];
    let mut chunks = Vec::new();
    let mut pos: u64 = 12;
    while file_size >= 8 && pos < file_size - 8 {
        f.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 8];
        let n = f.read(&mut hdr)?;
        if n < 8 {
            break;
        }
        let id = [hdr[0], hdr[1], hdr[2], hdr[3]];
        let size = if big_endian {
            u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]])
        } else {
            u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]])
        };
        chunks.push(Chunk { id, pos, size });
        pos += 8 + size as u64 + (size & 1) as u64;
    }
    Ok(IffFile { magic, form_type, file_size, chunks })
}

/// Read a chunk's raw bytes (header + body + pad), tolerating truncation like
/// Python's `f.read(n)` does.
fn read_raw(f: &mut std::fs::File, c: &Chunk) -> Result<Vec<u8>, TagError> {
    f.seek(SeekFrom::Start(c.pos))?;
    let want = 8 + c.size as usize + (c.size & 1) as usize;
    let mut buf = Vec::with_capacity(want);
    f.take(want as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn build_riff_info(artist: &str, title: &str) -> Vec<u8> {
    fn sub(cid: &[u8; 4], val: &str) -> Vec<u8> {
        let mut b = val.as_bytes().to_vec();
        b.push(0);
        if b.len() & 1 == 1 {
            b.push(0);
        }
        let mut out = cid.to_vec();
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(&b);
        out
    }
    let mut body = b"INFO".to_vec();
    if !artist.is_empty() {
        body.extend_from_slice(&sub(b"IART", artist));
    }
    if !title.is_empty() {
        body.extend_from_slice(&sub(b"INAM", title));
    }
    let mut out = b"LIST".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Is this chunk stale metadata our fresh INFO replaces? (`id3 ` chunks and
/// LIST-INFO / short LIST chunks.)
fn is_stale_meta(f: &mut std::fs::File, c: &Chunk) -> Result<bool, TagError> {
    if &c.id == b"id3 " {
        return Ok(true);
    }
    if &c.id == b"LIST" {
        let mut four = [0u8; 4];
        f.seek(SeekFrom::Start(c.pos + 8))?;
        let n = f.read(&mut four)?;
        return Ok((n == 4 && &four == b"INFO") || c.size < 4);
    }
    Ok(false)
}

fn write_wav(path: &Path, artist: &str, title: &str) -> Result<(), TagError> {
    let iff = read_chunks(path, false)?;
    if &iff.magic != b"RIFF" || iff.chunks.is_empty() {
        return Err(TagError::Format("not a RIFF/WAV file".into()));
    }
    let data_idx = iff
        .chunks
        .iter()
        .position(|c| &c.id == b"data")
        .ok_or_else(|| TagError::Format("no data chunk".into()))?;

    // Deviation from the Python oracle: if stale INFO/id3 metadata sits BEFORE
    // the data chunk (Beatport-style layout), the oracle leaves it there and
    // appends a second INFO after the audio — readers (lofty, Rekordbox) then
    // keep showing the stale first chunk. We rewrite the whole file instead so
    // exactly one INFO chunk remains. Covered by tests/tagging_parity.rs.
    {
        let mut f = std::fs::File::open(path)?;
        let mut has_pre_data_meta = false;
        for c in &iff.chunks[..data_idx] {
            if is_stale_meta(&mut f, c)? {
                has_pre_data_meta = true;
                break;
            }
        }
        if has_pre_data_meta {
            return wav_full_rewrite(path, artist, title);
        }
    }

    let info = build_riff_info(artist, title);
    let data = iff.chunks[data_idx];
    // Fast path: metadata lives only after the audio -> rewrite just the tail.
    let tail_start = data.pos + 8 + data.size as u64 + (data.size & 1) as u64;
    let mut keep = Vec::new();
    {
        let mut f = std::fs::File::open(path)?;
        for c in &iff.chunks[data_idx + 1..] {
            if is_stale_meta(&mut f, c)? {
                continue; // drop stale ID3 and old/empty INFO
            }
            keep.extend_from_slice(&read_raw(&mut f, c)?);
        }
    }
    keep.extend_from_slice(&info);
    let f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    f.set_len(tail_start)?;
    let mut f = f;
    f.seek(SeekFrom::End(0))?;
    f.write_all(&keep)?;
    let new_size = f.stream_position()?;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&((new_size - 8) as u32).to_le_bytes())?;
    Ok(())
}

/// Rewrite the whole WAV keeping every chunk except stale metadata, appending
/// one fresh INFO at the end (see the deviation note in `write_wav`).
fn wav_full_rewrite(path: &Path, artist: &str, title: &str) -> Result<(), TagError> {
    let iff = read_chunks(path, false)?;
    let mut blob = b"RIFF\x00\x00\x00\x00WAVE".to_vec();
    {
        let mut f = std::fs::File::open(path)?;
        for c in &iff.chunks {
            if is_stale_meta(&mut f, c)? {
                continue;
            }
            blob.extend_from_slice(&read_raw(&mut f, c)?);
        }
    }
    blob.extend_from_slice(&build_riff_info(artist, title));
    let size = ((blob.len() - 8) as u32).to_le_bytes();
    blob[4..8].copy_from_slice(&size);
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&tmp, blob)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn write_aiff(path: &Path, artist: &str, title: &str) -> Result<(), TagError> {
    let iff = read_chunks(path, true)?;
    if &iff.magic != b"FORM" {
        return Err(TagError::Format("not an AIFF file".into()));
    }
    let id3 = build_id3(artist, title, None);
    let mut chunk = b"ID3 ".to_vec();
    chunk.extend_from_slice(&(id3.len() as u32).to_be_bytes());
    chunk.extend_from_slice(&id3);
    if id3.len() & 1 == 1 {
        chunk.push(0);
    }

    // Drop any existing trailing ID3 chunk, then append the fresh one.
    let mut end = iff.file_size;
    if let Some(last) = iff.chunks.last() {
        if &last.id == b"ID3 " {
            end = last.pos;
        }
    }
    // If an ID3 chunk sits earlier in the file, fall back to a full rewrite.
    let n = iff.chunks.len();
    if n > 0 && iff.chunks[..n - 1].iter().any(|c| &c.id == b"ID3 ") {
        return aiff_full_rewrite(path, artist, title);
    }
    let f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    f.set_len(end)?;
    let mut f = f;
    f.seek(SeekFrom::End(0))?;
    f.write_all(&chunk)?;
    let new_size = f.stream_position()?;
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&((new_size - 8) as u32).to_be_bytes())?;
    Ok(())
}

fn aiff_full_rewrite(path: &Path, artist: &str, title: &str) -> Result<(), TagError> {
    let iff = read_chunks(path, true)?;
    let id3 = build_id3(artist, title, None);
    let mut blob = b"FORM\x00\x00\x00\x00".to_vec();
    blob.extend_from_slice(&iff.form_type);
    {
        let mut f = std::fs::File::open(path)?;
        for c in &iff.chunks {
            if &c.id == b"ID3 " {
                continue;
            }
            blob.extend_from_slice(&read_raw(&mut f, c)?);
        }
    }
    blob.extend_from_slice(b"ID3 ");
    blob.extend_from_slice(&(id3.len() as u32).to_be_bytes());
    blob.extend_from_slice(&id3);
    if id3.len() & 1 == 1 {
        blob.push(0);
    }
    let size = ((blob.len() - 8) as u32).to_be_bytes();
    blob[4..8].copy_from_slice(&size);
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&tmp, blob)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// --------------------------------------------------------------------------- #
// Dispatch
// --------------------------------------------------------------------------- #

/// Python `tag_file`: derive Artist/Title from the file's name and write them.
pub fn tag_file(path: &Path) -> Result<TagStatus, TagError> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (artist, title) = parse_name(&file_name);
    if title.is_empty() {
        return Ok(TagStatus::SkipNoName);
    }
    let ext = crate::normalize::splitext(&file_name).1.to_lowercase();
    match ext.as_str() {
        ".mp3" => write_mp3(path, &artist, &title)?,
        ".wav" => write_wav(path, &artist, &title)?,
        ".aiff" | ".aif" | ".aifc" => write_aiff(path, &artist, &title)?,
        _ => return Ok(TagStatus::SkipExt),
    }
    Ok(TagStatus::Ok)
}

/// Embedded (artist, title) via lofty; `None` when the file has no readable
/// tag. Replaces the Python oracle's optional ffprobe dependency.
pub fn read_tags(path: &Path) -> Option<(String, String)> {
    use lofty::config::ParseOptions;
    use lofty::file::TaggedFileExt;
    use lofty::probe::Probe;
    use lofty::tag::{Accessor, ItemKey};
    // Tags only: skipping audio properties is faster when scanning a large
    // library and tolerates streams lofty's property reader rejects.
    let tagged = Probe::open(path)
        .ok()?
        .options(ParseOptions::new().read_properties(false))
        .read()
        .ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let artist = tag
        .artist()
        .map(|c| c.into_owned())
        .or_else(|| tag.get_string(&ItemKey::TrackArtist).map(str::to_string))
        .unwrap_or_default();
    let title = tag
        .title()
        .map(|c| c.into_owned())
        .or_else(|| tag.get_string(&ItemKey::TrackTitle).map(str::to_string))
        .unwrap_or_default();
    if artist.is_empty() && title.is_empty() {
        None
    } else {
        Some((artist, title))
    }
}
