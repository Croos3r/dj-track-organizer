// SPDX-License-Identifier: GPL-3.0-only
//! Duplicate detection (byte-identical and same-song-across-formats) and safe
//! relocation of extras. Port of `skills/dedup-tracks/scripts/dedup_tracks.py`,
//! pinned by fixtures in `tests/dedup_parity.rs`.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

pub const AUDIO_EXT: [&str; 8] =
    [".mp3", ".wav", ".aiff", ".aif", ".aifc", ".flac", ".m4a", ".ogg"];

fn quality_rank(ext: &str) -> i32 {
    match ext {
        ".wav" | ".aiff" | ".aif" | ".aifc" | ".flac" => 3,
        ".m4a" | ".ogg" | ".mp3" => 1,
        _ => 0,
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct FileInfo {
    pub path: PathBuf,
    pub artist: String,
    pub title: String,
    pub dur: Option<f64>,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum DupKind {
    Exact,
    SameSong,
}

impl DupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DupKind::Exact => "exact",
            DupKind::SameSong => "same-song",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DupGroup {
    pub kind: DupKind,
    pub keeper: FileInfo,
    pub extras: Vec<FileInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Exact,
    Song,
    Both,
}

/// Python `strip_accents` + `norm_key`: normalised `artist|title` key.
fn norm_key(artist: &str, title: &str) -> String {
    fn clean(x: &str) -> String {
        let stripped: String = x
            .nfkd()
            .filter(|c| c.general_category() != GeneralCategory::NonspacingMark)
            .collect();
        let lower = stripped.to_lowercase();
        let mut out = String::with_capacity(lower.len());
        let mut in_junk = false;
        for c in lower.chars() {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                out.push(c);
                in_junk = false;
            } else if !in_junk {
                out.push(' ');
                in_junk = true;
            }
        }
        out.trim().to_string()
    }
    format!("{}|{}", clean(artist), clean(title))
}

fn sha1_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn ext_of(path: &Path) -> String {
    crate::normalize::splitext(&path.file_name().unwrap_or_default().to_string_lossy())
        .1
        .to_lowercase()
}

/// Python `keeper_score`: higher is better (lossless, then longer, then larger).
fn keeper_score(info: &FileInfo) -> (i32, f64, u64) {
    (quality_rank(&ext_of(&info.path)), info.dur.unwrap_or(0.0), info.size)
}

fn has_audio_ext(name: &str) -> bool {
    let lower = name.to_lowercase();
    AUDIO_EXT.iter().any(|e| lower.ends_with(e))
}

/// Directory scan in the oracle's visit order (files of a dir first, then its
/// subdirectories, names sorted).
pub fn scan<F>(folder: &Path, recursive: bool, mut read_tags: F) -> std::io::Result<Vec<FileInfo>>
where
    F: FnMut(&Path) -> Option<(String, String, Option<f64>)>,
{
    fn sorted_entries(dir: &Path) -> std::io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                dirs.push(e.path());
            } else {
                files.push(e.path());
            }
        }
        files.sort();
        dirs.sort();
        Ok((files, dirs))
    }

    let mut paths = Vec::new();
    let mut stack = vec![folder.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let (files, dirs) = sorted_entries(&dir)?;
        paths.extend(files.into_iter().filter(|p| {
            p.file_name()
                .map(|n| has_audio_ext(&n.to_string_lossy()))
                .unwrap_or(false)
        }));
        if recursive {
            // depth-first, preserving sorted order (stack is LIFO)
            for d in dirs.into_iter().rev() {
                stack.push(d);
            }
        }
    }

    let mut infos = Vec::new();
    for p in paths {
        let (mut artist, mut title, dur) =
            read_tags(&p).unwrap_or((String::new(), String::new(), None));
        if artist.is_empty() || title.is_empty() {
            let (fa, ft) =
                crate::tagging::parse_name(&p.file_name().unwrap_or_default().to_string_lossy());
            if artist.is_empty() {
                artist = fa;
            }
            if title.is_empty() {
                title = ft;
            }
        }
        let size = std::fs::metadata(&p)?.len();
        infos.push(FileInfo { path: p, artist, title, dur, size });
    }
    Ok(infos)
}

/// Ordered grouping helper (Python dict preserves insertion order).
fn group_by<K: std::hash::Hash + Eq + Clone>(
    items: &[FileInfo],
    key: impl Fn(&FileInfo) -> K,
) -> Vec<(K, Vec<FileInfo>)> {
    let mut order: Vec<K> = Vec::new();
    let mut map: HashMap<K, Vec<FileInfo>> = HashMap::new();
    for it in items {
        let k = key(it);
        if !map.contains_key(&k) {
            order.push(k.clone());
        }
        map.entry(k).or_default().push(it.clone());
    }
    order.into_iter().map(|k| (k.clone(), map.remove(&k).unwrap())).collect()
}

fn find_exact(infos: &[FileInfo]) -> std::io::Result<Vec<Vec<FileInfo>>> {
    let mut groups = Vec::new();
    for (_, items) in group_by(infos, |i| i.size) {
        if items.len() < 2 {
            continue;
        }
        let mut hashed = Vec::new();
        for it in items {
            let h = sha1_file(&it.path)?;
            hashed.push((h, it));
        }
        let keys: Vec<FileInfo> = hashed.iter().map(|(_, i)| i.clone()).collect();
        let hs: Vec<String> = hashed.iter().map(|(h, _)| h.clone()).collect();
        let mut order: Vec<String> = Vec::new();
        let mut map: HashMap<String, Vec<FileInfo>> = HashMap::new();
        for (h, it) in hs.into_iter().zip(keys.into_iter()) {
            if !map.contains_key(&h) {
                order.push(h.clone());
            }
            map.entry(h).or_default().push(it);
        }
        for h in order {
            let dups = map.remove(&h).unwrap();
            if dups.len() > 1 {
                groups.push(dups);
            }
        }
    }
    Ok(groups)
}

fn find_same_song(infos: &[FileInfo]) -> Vec<Vec<FileInfo>> {
    let with_title: Vec<FileInfo> =
        infos.iter().filter(|i| !i.title.is_empty()).cloned().collect();
    group_by(&with_title, |i| norm_key(&i.artist, &i.title))
        .into_iter()
        .filter_map(|(_, items)| (items.len() > 1).then_some(items))
        .collect()
}

/// Python `choose`: stable sort descending by keeper score.
fn choose(mut group: Vec<FileInfo>) -> (FileInfo, Vec<FileInfo>) {
    group.sort_by(|a, b| {
        let (ra, da, sa) = keeper_score(a);
        let (rb, db, sb) = keeper_score(b);
        (rb, sb)
            .cmp(&(ra, sa))
            .then(db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal))
    });
    let keeper = group.remove(0);
    (keeper, group)
}

/// Python main-loop logic: exact groups first, then same-song, never reporting
/// the same extra twice.
pub fn find_duplicates(infos: &[FileInfo], mode: Mode) -> std::io::Result<Vec<DupGroup>> {
    let mut groups = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut add = |found: Vec<Vec<FileInfo>>, kind: DupKind, groups: &mut Vec<DupGroup>| {
        for g in found {
            let (keeper, extras) = choose(g);
            let fresh: Vec<FileInfo> =
                extras.into_iter().filter(|e| !seen.contains(&e.path)).collect();
            if fresh.is_empty() {
                continue;
            }
            for e in &fresh {
                seen.insert(e.path.clone());
            }
            groups.push(DupGroup { kind, keeper, extras: fresh });
        }
    };
    if matches!(mode, Mode::Exact | Mode::Both) {
        add(find_exact(infos)?, DupKind::Exact, &mut groups);
    }
    if matches!(mode, Mode::Song | Mode::Both) {
        add(find_same_song(infos), DupKind::SameSong, &mut groups);
    }
    Ok(groups)
}

/// duplicates.csv, byte-compatible with the oracle's report.
pub fn write_report(path: &Path, groups: &[DupGroup]) -> std::io::Result<usize> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(b"\xef\xbb\xbf")?;
    let mut w = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .quote_style(csv::QuoteStyle::Necessary)
        .from_writer(file);
    w.write_record(["group", "kind", "role", "file", "artist", "title", "ext", "size_bytes"])?;
    let mut extras = 0usize;
    for (gid, g) in groups.iter().enumerate() {
        let mut rows: Vec<(&str, &FileInfo)> = vec![("keep", &g.keeper)];
        rows.extend(g.extras.iter().map(|e| ("duplicate", e)));
        for (role, it) in rows {
            w.write_record([
                (gid + 1).to_string().as_str(),
                g.kind.as_str(),
                role,
                &it.path.to_string_lossy(),
                &it.artist,
                &it.title,
                &ext_of(&it.path),
                it.size.to_string().as_str(),
            ])?;
            if role == "duplicate" {
                extras += 1;
            }
        }
    }
    w.flush()?;
    Ok(extras)
}

/// Move the extra copies into `dest` (created if needed), suffixing colliding
/// basenames with " (2)", " (3)", … like the oracle.
pub fn move_extras(groups: &[DupGroup], dest: &Path) -> std::io::Result<Vec<(PathBuf, PathBuf)>> {
    std::fs::create_dir_all(dest)?;
    let mut moved = Vec::new();
    for g in groups {
        for e in &g.extras {
            let base = e.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let mut target = dest.join(&base);
            let mut n = 2;
            while target.exists() {
                let (root, ext) = crate::normalize::splitext(&base);
                target = dest.join(format!("{root} ({n}){ext}"));
                n += 1;
            }
            match std::fs::rename(&e.path, &target) {
                Ok(()) => {}
                Err(_) => {
                    // cross-volume fallback, like shutil.move
                    std::fs::copy(&e.path, &target)?;
                    std::fs::remove_file(&e.path)?;
                }
            }
            moved.push((e.path.clone(), target));
        }
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_key_folds_accents_and_punctuation() {
        assert_eq!(norm_key("Àrtist", "Söng!"), "artist|song");
        assert_eq!(norm_key("A B", "T (Original Mix)"), "a b|t original mix");
        assert_eq!(norm_key("", "x"), "|x");
    }

    #[test]
    fn move_extras_suffixes_collisions() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("lib");
        let dest = td.path().join("dupes");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("A - B.mp3"), b"one").unwrap();
        std::fs::write(src.join("sub/A - B.mp3"), b"two").unwrap();
        let mk = |p: PathBuf, size: u64| FileInfo {
            path: p,
            artist: "A".into(),
            title: "B".into(),
            dur: None,
            size,
        };
        let groups = vec![DupGroup {
            kind: DupKind::SameSong,
            keeper: mk(src.join("keeper.wav"), 9),
            extras: vec![mk(src.join("A - B.mp3"), 3), mk(src.join("sub/A - B.mp3"), 3)],
        }];
        let moved = move_extras(&groups, &dest).unwrap();
        assert_eq!(moved.len(), 2);
        assert!(dest.join("A - B.mp3").exists());
        assert!(dest.join("A - B (2).mp3").exists());
        assert!(!src.join("A - B.mp3").exists());
    }
}
