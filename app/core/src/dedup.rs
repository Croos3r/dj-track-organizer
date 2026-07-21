// SPDX-License-Identifier: GPL-3.0-only
//! Duplicate detection (byte-identical and same-song-across-formats) and safe
//! relocation of extras. Port of `skills/dedup-tracks/scripts/dedup_tracks.py`,
//! pinned by fixtures in `tests/dedup_parity.rs`.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::prelude::*;
use sha1::{Digest, Sha1};
use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

pub const AUDIO_EXT: [&str; 8] = [
    ".mp3", ".wav", ".aiff", ".aif", ".aifc", ".flac", ".m4a", ".ogg",
];

pub(crate) fn quality_rank(ext: &str) -> i32 {
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
pub(crate) fn norm_key(artist: &str, title: &str) -> String {
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

/// Which content hash to use when confirming byte-identical files. Both give
/// identical *grouping* (equal hash ⇒ equal bytes); Blake3 is much faster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Sha1,
    Blake3,
}

/// How exact-duplicate detection reads files. Neither knob changes which files
/// are grouped — only how much work it takes to decide.
#[derive(Clone, Copy, Debug)]
pub struct HashStrategy {
    pub algo: HashAlgo,
    /// Hash a small prefix first and only read the whole file when prefixes
    /// collide. Avoids reading entire multi-hundred-MB files that differ early.
    pub prefix_gate: bool,
    /// Worker threads for hashing (0 or 1 = sequential). Files are independent,
    /// so this scales with cores on an SSD.
    pub parallelism: usize,
}

impl Default for HashStrategy {
    /// Production default: fast hash, prefix-gated, all cores.
    fn default() -> Self {
        HashStrategy {
            algo: HashAlgo::Blake3,
            prefix_gate: true,
            parallelism: 0,
        }
    }
}

impl HashStrategy {
    /// The old behavior, kept as the benchmark baseline.
    pub fn baseline() -> Self {
        HashStrategy {
            algo: HashAlgo::Sha1,
            prefix_gate: false,
            parallelism: 1,
        }
    }
}

/// Bytes read by the last hashing pass — lets the benchmark show the I/O the
/// prefix gate avoids, independent of OS cache. Best-effort/global; only the
/// single-threaded benchmark reads it meaningfully.
pub static BYTES_HASHED: AtomicU64 = AtomicU64::new(0);

const PREFIX_LEN: usize = 64 * 1024;

fn hash_reader<R: Read>(mut r: R, algo: HashAlgo, limit: Option<usize>) -> std::io::Result<String> {
    let mut buf = vec![0u8; 1 << 20];
    let mut remaining = limit.unwrap_or(usize::MAX);
    let mut sha = (algo == HashAlgo::Sha1).then(Sha1::new);
    let mut b3 = (algo == HashAlgo::Blake3).then(blake3::Hasher::new);
    let mut read_total: u64 = 0;
    while remaining > 0 {
        let want = buf.len().min(remaining);
        let n = r.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        read_total += n as u64;
        remaining -= n;
        if let Some(h) = sha.as_mut() {
            h.update(&buf[..n]);
        }
        if let Some(h) = b3.as_mut() {
            h.update(&buf[..n]);
        }
    }
    BYTES_HASHED.fetch_add(read_total, Ordering::Relaxed);
    Ok(match (sha, b3) {
        (Some(h), _) => format!("{:x}", h.finalize()),
        (_, Some(h)) => h.finalize().to_hex().to_string(),
        _ => unreachable!(),
    })
}

/// Full-file content hash.
fn hash_full(path: &Path, algo: HashAlgo) -> std::io::Result<String> {
    hash_reader(std::fs::File::open(path)?, algo, None)
}

/// Hash of at most the first `PREFIX_LEN` bytes (prefixed with a tag so a
/// prefix hash can never collide with a full hash).
fn hash_prefix(path: &Path, algo: HashAlgo) -> std::io::Result<String> {
    Ok(format!(
        "p:{}",
        hash_reader(std::fs::File::open(path)?, algo, Some(PREFIX_LEN))?
    ))
}

fn ext_of(path: &Path) -> String {
    crate::normalize::splitext(&path.file_name().unwrap_or_default().to_string_lossy())
        .1
        .to_lowercase()
}

/// Python `keeper_score`: higher is better (lossless, then longer, then larger).
fn keeper_score(info: &FileInfo) -> (i32, f64, u64) {
    (
        quality_rank(&ext_of(&info.path)),
        info.dur.unwrap_or(0.0),
        info.size,
    )
}

fn has_audio_ext(name: &str) -> bool {
    let lower = name.to_lowercase();
    AUDIO_EXT.iter().any(|e| lower.ends_with(e))
}

/// Audio files in the oracle's visit order (a directory's files first, then its
/// subdirectories, names sorted).
pub fn collect_audio_paths(folder: &Path, recursive: bool) -> std::io::Result<Vec<PathBuf>> {
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
            for d in dirs.into_iter().rev() {
                stack.push(d);
            }
        }
    }
    Ok(paths)
}

/// Build one `FileInfo` from a path and an optional (artist, title, dur), with
/// the filename fallback the oracle uses when tags are missing.
fn file_info(p: &Path, tags: Option<(String, String, Option<f64>)>) -> std::io::Result<FileInfo> {
    let (mut artist, mut title, dur) = tags.unwrap_or((String::new(), String::new(), None));
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
    let size = std::fs::metadata(p)?.len();
    Ok(FileInfo {
        path: p.to_path_buf(),
        artist,
        title,
        dur,
        size,
    })
}

/// Sequential scan (used by tests and callers that pass a stateful closure).
pub fn scan<F>(folder: &Path, recursive: bool, mut read_tags: F) -> std::io::Result<Vec<FileInfo>>
where
    F: FnMut(&Path) -> Option<(String, String, Option<f64>)>,
{
    let paths = collect_audio_paths(folder, recursive)?;
    paths.iter().map(|p| file_info(p, read_tags(p))).collect()
}

/// Parallel scan: reads embedded tags across `parallelism` threads while
/// preserving the sequential visit order. `parallelism == 1` (or a reader that
/// never touches disk) makes this equivalent to [`scan`].
pub fn scan_with(
    folder: &Path,
    recursive: bool,
    parallelism: usize,
    read_tags: impl Fn(&Path) -> Option<(String, String, Option<f64>)> + Sync,
) -> std::io::Result<Vec<FileInfo>> {
    let paths = collect_audio_paths(folder, recursive)?;
    if parallelism == 1 {
        return paths.iter().map(|p| file_info(p, read_tags(p))).collect();
    }
    run_in_pool(parallelism, || {
        paths
            .par_iter()
            .map(|p| file_info(p, read_tags(p)))
            .collect()
    })
}

/// Read embedded (artist, title) for many files in parallel, keyed by file
/// name. Files without readable tags are omitted (callers fall back to the
/// filename). Used to parallelize the normalize planning scan.
pub fn read_tags_by_name(
    paths: &[PathBuf],
    parallelism: usize,
) -> HashMap<String, (String, String)> {
    let read = |p: &PathBuf| {
        let name = p.file_name()?.to_string_lossy().into_owned();
        crate::tagging::read_tags(p).map(|(a, t)| (name, (a, t)))
    };
    let pairs: Vec<(String, (String, String))> = if parallelism == 1 {
        paths.iter().filter_map(read).collect()
    } else {
        run_in_pool(parallelism, || paths.par_iter().filter_map(read).collect())
    };
    pairs.into_iter().collect()
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
    order
        .into_iter()
        .map(|k| (k.clone(), map.remove(&k).unwrap()))
        .collect()
}

use crate::parallel::run_in_pool;

/// Hash each item's file, returning hashes in the SAME order as `items` (so the
/// downstream grouping stays deterministic and order-identical to the
/// sequential baseline). `parallelism == 1` runs a plain sequential loop.
fn hashes_in_order(
    items: &[FileInfo],
    parallelism: usize,
    hash: impl Fn(&Path) -> std::io::Result<String> + Sync,
) -> std::io::Result<Vec<String>> {
    if parallelism == 1 {
        items.iter().map(|it| hash(&it.path)).collect()
    } else {
        run_in_pool(parallelism, || {
            items.par_iter().map(|it| hash(&it.path)).collect()
        })
    }
}

/// First-seen-order grouping of `items` by `keys`, keeping only groups of >1.
fn group_ordered_by(items: &[FileInfo], keys: &[String]) -> Vec<Vec<FileInfo>> {
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, Vec<FileInfo>> = HashMap::new();
    for (it, k) in items.iter().zip(keys) {
        if !map.contains_key(k) {
            order.push(k.clone());
        }
        map.entry(k.clone()).or_default().push(it.clone());
    }
    order
        .into_iter()
        .filter_map(|k| {
            let g = map.remove(&k).unwrap();
            (g.len() > 1).then_some(g)
        })
        .collect()
}

/// Byte-identical file groups, grouped by size then content hash. Both the
/// prefix gate and the parallelism only affect *how* the identical-byte
/// decision is reached — the resulting groups and their order are the same as
/// the sequential full-hash baseline (see `dedup_strategies_agree` test).
pub fn find_exact_with(
    infos: &[FileInfo],
    strat: HashStrategy,
) -> std::io::Result<Vec<Vec<FileInfo>>> {
    let mut groups = Vec::new();
    for (_, items) in group_by(infos, |i| i.size) {
        if items.len() < 2 {
            continue;
        }
        // Optional prefix gate: drop items whose leading bytes are unique — a
        // unique prefix means unique content, so they can't be duplicates.
        let candidates: Vec<FileInfo> = if strat.prefix_gate {
            let pre = hashes_in_order(&items, strat.parallelism, |p| hash_prefix(p, strat.algo))?;
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for k in &pre {
                *counts.entry(k.as_str()).or_insert(0) += 1;
            }
            items
                .iter()
                .zip(&pre)
                .filter(|(_, k)| counts[k.as_str()] > 1)
                .map(|(it, _)| it.clone())
                .collect()
        } else {
            items.clone()
        };
        if candidates.len() < 2 {
            continue;
        }
        let full = hashes_in_order(&candidates, strat.parallelism, |p| hash_full(p, strat.algo))?;
        groups.extend(group_ordered_by(&candidates, &full));
    }
    Ok(groups)
}

fn find_same_song(infos: &[FileInfo]) -> Vec<Vec<FileInfo>> {
    let with_title: Vec<FileInfo> = infos
        .iter()
        .filter(|i| !i.title.is_empty())
        .cloned()
        .collect();
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
/// the same extra twice. Uses the production hashing default (fast + parallel).
pub fn find_duplicates(infos: &[FileInfo], mode: Mode) -> std::io::Result<Vec<DupGroup>> {
    find_duplicates_with(infos, mode, HashStrategy::default())
}

/// As [`find_duplicates`], with an explicit hashing strategy (used by the
/// benchmark to compare baseline vs. parallel vs. smart hashing). The output is
/// identical across strategies; only the work to produce it differs.
pub fn find_duplicates_with(
    infos: &[FileInfo],
    mode: Mode,
    strat: HashStrategy,
) -> std::io::Result<Vec<DupGroup>> {
    let mut groups = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut add = |found: Vec<Vec<FileInfo>>, kind: DupKind, groups: &mut Vec<DupGroup>| {
        for g in found {
            let (keeper, extras) = choose(g);
            let fresh: Vec<FileInfo> = extras
                .into_iter()
                .filter(|e| !seen.contains(&e.path))
                .collect();
            if fresh.is_empty() {
                continue;
            }
            for e in &fresh {
                seen.insert(e.path.clone());
            }
            groups.push(DupGroup {
                kind,
                keeper,
                extras: fresh,
            });
        }
    };
    if matches!(mode, Mode::Exact | Mode::Both) {
        add(find_exact_with(infos, strat)?, DupKind::Exact, &mut groups);
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
    w.write_record([
        "group",
        "kind",
        "role",
        "file",
        "artist",
        "title",
        "ext",
        "size_bytes",
    ])?;
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
            let base = e
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut target = dest.join(&base);
            let mut n = 2;
            while target.exists() {
                let (root, ext) = crate::normalize::splitext(&base);
                target = dest.join(format!("{root} ({n}){ext}"));
                n += 1;
            }
            match crate::retry::on_lock(|| std::fs::rename(&e.path, &target)) {
                Ok(()) => {}
                Err(_) => {
                    // cross-volume fallback, like shutil.move; each step also
                    // retries a transient lock (antivirus / search indexer)
                    crate::retry::on_lock(|| std::fs::copy(&e.path, &target).map(|_| ()))?;
                    crate::retry::on_lock(|| std::fs::remove_file(&e.path))?;
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
            extras: vec![
                mk(src.join("A - B.mp3"), 3),
                mk(src.join("sub/A - B.mp3"), 3),
            ],
        }];
        let moved = move_extras(&groups, &dest).unwrap();
        assert_eq!(moved.len(), 2);
        assert!(dest.join("A - B.mp3").exists());
        assert!(dest.join("A - B (2).mp3").exists());
        assert!(!src.join("A - B.mp3").exists());
    }

    /// Every hashing strategy must yield byte-identical duplicate groups (and
    /// order) — that is what lets the benchmark swap strategies freely and what
    /// keeps parity with the Python oracle. Includes the case the prefix gate
    /// targets: same size, same 64 KB prefix, different tail.
    #[test]
    fn dedup_strategies_agree() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        let big = 200 * 1024; // > PREFIX_LEN so prefix-gating is exercised
        let write = |name: &str, content: Vec<u8>| std::fs::write(dir.join(name), content).unwrap();

        // exact duplicates (same bytes)
        write("Alpha - One.wav", vec![1u8; big]);
        write("Alpha - One (copy).wav", vec![1u8; big]);
        // same size + same prefix, different tail -> NOT duplicates
        let mut a = vec![7u8; big];
        let mut b = vec![7u8; big];
        a[big - 1] = 0xAA;
        b[big - 1] = 0xBB;
        write("Beta - Two.wav", a);
        write("Beta - Two (alt).wav", b);
        // same size, different prefix -> NOT duplicates
        write("Gamma - Three.mp3", vec![3u8; 500]);
        write("Delta - Four.mp3", vec![4u8; 500]);
        // unique
        write("Solo - Track.flac", vec![9u8; 123]);

        let infos = scan(dir, false, |_| None).unwrap();
        let strategies = [
            HashStrategy::baseline(),
            HashStrategy {
                algo: HashAlgo::Sha1,
                prefix_gate: false,
                parallelism: 4,
            },
            HashStrategy {
                algo: HashAlgo::Blake3,
                prefix_gate: true,
                parallelism: 1,
            },
            HashStrategy {
                algo: HashAlgo::Blake3,
                prefix_gate: true,
                parallelism: 4,
            },
            HashStrategy::default(),
        ];
        let as_paths = |gs: &[Vec<FileInfo>]| -> Vec<Vec<PathBuf>> {
            gs.iter()
                .map(|g| g.iter().map(|f| f.path.clone()).collect())
                .collect()
        };
        let baseline = as_paths(&find_exact_with(&infos, strategies[0]).unwrap());
        // exactly the one true exact-duplicate pair
        assert_eq!(baseline.len(), 1, "one exact-dup group expected");
        assert_eq!(baseline[0].len(), 2);
        for s in &strategies[1..] {
            let got = as_paths(&find_exact_with(&infos, *s).unwrap());
            assert_eq!(got, baseline, "strategy {s:?} disagreed with the baseline");
        }
    }
}
