// SPDX-License-Identifier: GPL-3.0-only
//! Speedup benchmark for a realistic, never-sorted library.
//!
//! Generates a throwaway corpus (messy names, mixed formats, realistic sizes,
//! injected exact + same-song + same-size duplicates) in a temp dir, then times
//! each speedup feature individually and together, and prints a comparison.
//!
//! Run (skip the Rekordbox toolchain, use release for real numbers):
//!   cargo run --release --no-default-features --example bench
//! Tunables:  BENCH_FILES=800  BENCH_MAXKB=3072  cargo run ...
//!
//! Every hashing variant is asserted to produce identical duplicate groups, so
//! this doubles as a differential correctness check on top of the timings.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use organizer_core::dedup::{
    collect_audio_paths, find_exact_with, scan_with, FileInfo, HashAlgo, HashStrategy, BYTES_HASHED,
};
use organizer_core::tagging;

fn env_num(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Deterministic per-file pseudo-random byte stream (xorshift64).
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        // splitmix64 finalizer so adjacent seeds diverge completely — a plain
        // `seed | 1` would make seed and seed+1 produce identical streams,
        // silently turning "unique" corpus files into duplicates.
        let mut z = seed.wrapping_add(0x9E3779B97F4A7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        Rng((z ^ (z >> 31)) | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let b = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
    }
}

fn wav_bytes(body_len: usize, seed: u64) -> Vec<u8> {
    let mut body = vec![0u8; body_len];
    Rng::new(seed).fill(&mut body);
    let mut out = Vec::with_capacity(44 + body_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + body_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // stereo
    out.extend_from_slice(&44100u32.to_le_bytes());
    out.extend_from_slice(&176400u32.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(body_len as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn mp3_bytes(body_len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(body_len + 4);
    out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x64]); // MPEG-1 L3 frame header-ish
    let mut body = vec![0u8; body_len];
    Rng::new(seed).fill(&mut body);
    out.extend_from_slice(&body);
    out
}

fn aiff_bytes(body_len: usize, seed: u64) -> Vec<u8> {
    let mut body = vec![0u8; body_len];
    Rng::new(seed).fill(&mut body);
    let comm = {
        let mut c = Vec::new();
        c.extend_from_slice(b"COMM");
        c.extend_from_slice(&18u32.to_be_bytes());
        c.extend_from_slice(&2u16.to_be_bytes());
        c.extend_from_slice(&((body_len / 4) as u32).to_be_bytes());
        c.extend_from_slice(&16u16.to_be_bytes());
        c.extend_from_slice(&[0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0]);
        c
    };
    let ssnd_len = 8 + body_len;
    let mut out = Vec::new();
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&((4 + comm.len() + 8 + ssnd_len) as u32).to_be_bytes());
    out.extend_from_slice(b"AIFF");
    out.extend_from_slice(&comm);
    out.extend_from_slice(b"SSND");
    out.extend_from_slice(&(ssnd_len as u32).to_be_bytes());
    out.extend_from_slice(&[0u8; 8]);
    out.extend_from_slice(&body);
    out
}

const ARTISTS: &[&str] = &[
    "Angerfist",
    "3 Steps Ahead",
    "12 Inch",
    "DJ Furax",
    "Neophyte",
    "Von Bikräv",
    "Aexhy",
    "Sétaou",
    "Alignment",
    "I Hate Models",
    "Zatox",
    "APY",
    "Drymk",
    "Insomniak",
];
const TITLES: &[&str] = &[
    "Dream",
    "Gangster",
    "Supersaw",
    "Rave",
    "Moulin Rouge",
    "Prière Païenne",
    "Terror",
    "Impact",
    "Hardcore",
    "Bass D",
    "Money In My Pocket",
    "The Tunnel",
    "1312",
    "Vitesse",
];
const MIXES: &[&str] = &[
    "",
    "",
    " (Original Mix)",
    " (Extended Mix)",
    " (Von Bikräv Remix)",
    " (Edit)",
];

/// A messy, never-normalized filename (track-number prefixes, mixed case,
/// underscores, feat variants, occasional missing " - ").
fn messy_name(rng: &mut Rng, artist: &str, title: &str, mix: &str, ext: &str) -> String {
    let r = rng.next_u64();
    let base = match r % 6 {
        0 => format!("{:02} {} - {}{}", (r >> 8) % 20 + 1, artist, title, mix),
        1 => format!(
            "{}_-_{}{}",
            artist.replace(' ', "_"),
            title.replace(' ', "_"),
            mix
        ),
        2 => format!(
            "{} - {} feat {}{}",
            artist,
            title,
            ARTISTS[(r as usize >> 4) % ARTISTS.len()],
            mix
        ),
        3 => format!(
            "{} - {}{}",
            artist.to_uppercase(),
            title.to_uppercase(),
            mix
        ),
        4 => format!("{}{}", title, mix), // no " - " -> needs manual name
        _ => format!("{} - {}{}", artist, title, mix),
    };
    format!("{base}{ext}")
}

struct Corpus {
    dir: tempfile::TempDir,
    /// pristine byte snapshots so destructive steps can restore between runs
    files: Vec<(PathBuf, Vec<u8>)>,
}

fn build_corpus(n: usize, max_kb: usize) -> std::io::Result<Corpus> {
    let dir = tempfile::tempdir()?;
    let mut rng = Rng::new(0xC0FFEE);
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut used_names = std::collections::HashSet::new();

    let mut make = |rng: &mut Rng, name_seed: u64, size: usize, content_seed: u64| {
        let a = ARTISTS[(name_seed as usize) % ARTISTS.len()];
        let t = TITLES[(name_seed as usize >> 4) % TITLES.len()];
        let m = MIXES[(name_seed as usize >> 8) % MIXES.len()];
        let (ext, bytes): (&str, Vec<u8>) = match rng.next_u64() % 100 {
            0..=54 => (".mp3", mp3_bytes(size, content_seed)),
            55..=84 => (".wav", wav_bytes(size, content_seed)),
            _ => (".aif", aiff_bytes(size, content_seed)),
        };
        let mut name = messy_name(rng, a, t, m, ext);
        while !used_names.insert(name.clone()) {
            name = format!(
                "{} {}{}",
                &name[..name.len() - ext.len()],
                rng.next_u64() % 999,
                ext
            );
        }
        (name, bytes)
    };

    // main body: unique-ish files, realistic size spread
    for i in 0..n {
        let size = 64 * 1024 + (rng.next_u64() as usize % (max_kb.saturating_sub(64) * 1024 + 1));
        let (name, bytes) = make(&mut rng, i as u64 * 2654435761, size, i as u64 + 1);
        files.push((dir.path().join(name), bytes));
    }
    // ~8% exact duplicates: identical bytes under a different messy name
    for i in 0..(n / 12).max(1) {
        let src = &files[(rng.next_u64() as usize) % files.len()];
        let bytes = src.1.clone();
        let ext = src
            .0
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let mut name = messy_name(
            &mut rng,
            ARTISTS[i % ARTISTS.len()],
            TITLES[i % TITLES.len()],
            "",
            &ext,
        );
        while !used_names.insert(name.clone()) {
            name = format!("dup{}_{}", i, name);
        }
        files.push((dir.path().join(name), bytes));
    }
    // same-size collision cluster (different content from byte 0): the prefix
    // gate's best case — coincidental equal sizes that differ immediately.
    let cluster_size = max_kb * 1024;
    for i in 0..(n / 15).max(2) {
        let bytes = mp3_bytes(cluster_size, 0xA5A5_0000 + i as u64);
        let name = format!("Cluster Artist {i} - Same Size {i}.mp3");
        used_names.insert(name.clone());
        files.push((dir.path().join(name), bytes));
    }

    let mut total = 0u64;
    for (p, b) in &files {
        std::fs::write(p, b)?;
        total += b.len() as u64;
    }
    println!(
        "corpus: {} files, {:.0} MB, in {}",
        files.len(),
        total as f64 / 1e6,
        dir.path().display()
    );
    Ok(Corpus { dir, files })
}

impl Corpus {
    fn scan(&self) -> Vec<FileInfo> {
        // filenames-only info is enough for exact-hash grouping
        scan_with(self.dir.path(), false, 1, |_| None).unwrap()
    }
    fn restore(&self) {
        for (p, b) in &self.files {
            std::fs::write(p, b).unwrap();
        }
    }
    fn warm_cache(&self) {
        for (p, _) in &self.files {
            let _ = std::fs::read(p);
        }
    }
}

fn secs(d: std::time::Duration) -> f64 {
    d.as_secs_f64()
}

fn main() {
    let n = env_num("BENCH_FILES", 600);
    let max_kb = env_num("BENCH_MAXKB", 3072);
    let cores = std::thread::available_parallelism()
        .map(|c| c.get())
        .unwrap_or(1);
    println!("=== Track Organizer speedup benchmark ===");
    println!("cores available: {cores}\n");

    let corpus = build_corpus(n, max_kb).expect("build corpus");
    let infos = corpus.scan();
    corpus.warm_cache(); // time algorithm/threading, not cache-warming order

    // ---- Exact-duplicate hashing matrix ---------------------------------- //
    println!("\n[1] Exact-duplicate hashing (find_exact)");
    println!("    warm-cache wall time isolates CPU/threads; bytes-read shows the");
    println!("    I/O a cold, larger-than-RAM library would actually pay.\n");
    let variants: &[(&str, HashStrategy)] = &[
        ("baseline  SHA1 · full   · seq ", HashStrategy::baseline()),
        (
            "+rayon    SHA1 · full   · auto",
            HashStrategy {
                parallelism: 0,
                ..HashStrategy::baseline()
            },
        ),
        (
            "+smart    blake3 · prefix · seq ",
            HashStrategy {
                algo: HashAlgo::Blake3,
                prefix_gate: true,
                parallelism: 1,
            },
        ),
        ("+both     blake3 · prefix · auto", HashStrategy::default()),
    ];
    let mut baseline_groups: Option<Vec<Vec<PathBuf>>> = None;
    let mut base_time = 0f64;
    let mut base_bytes = 0u64;
    println!(
        "    {:<34}  {:>9}  {:>9}  {:>10}  {:>8}",
        "strategy", "time", "speedup", "read", "io-save"
    );
    for (label, strat) in variants {
        // best of 2 warm runs
        let mut best = f64::MAX;
        let mut bytes = 0u64;
        let mut groups = Vec::new();
        for _ in 0..2 {
            BYTES_HASHED.store(0, Ordering::Relaxed);
            let t = Instant::now();
            let g = find_exact_with(&infos, *strat).unwrap();
            best = best.min(secs(t.elapsed()));
            bytes = BYTES_HASHED.load(Ordering::Relaxed);
            groups = g
                .iter()
                .map(|grp| grp.iter().map(|f| f.path.clone()).collect())
                .collect();
        }
        if baseline_groups.is_none() {
            baseline_groups = Some(groups.clone());
            base_time = best;
            base_bytes = bytes;
        } else {
            assert_eq!(
                &groups,
                baseline_groups.as_ref().unwrap(),
                "{label} changed the groups!"
            );
        }
        println!(
            "    {:<34}  {:>7.3}s  {:>8.2}x  {:>8.0}MB  {:>7.1}x",
            label,
            best,
            base_time / best,
            bytes as f64 / 1e6,
            base_bytes as f64 / bytes.max(1) as f64,
        );
    }
    let dup_groups = baseline_groups.unwrap();
    println!(
        "    (identical output across all four; {} exact-dup groups found)",
        dup_groups.len()
    );

    // ---- Scan: parallel tag reads ---------------------------------------- //
    println!("\n[2] Library scan / tag read (scan_with)");
    for &par in &[1usize, 0usize] {
        let mut best = f64::MAX;
        for _ in 0..2 {
            let t = Instant::now();
            let got = scan_with(corpus.dir.path(), false, par, |p| {
                tagging::read_tags(p).map(|(a, t)| (a, t, None))
            })
            .unwrap();
            best = best.min(secs(t.elapsed()));
            assert_eq!(got.len(), infos.len());
        }
        let tag = if par == 1 {
            "sequential"
        } else {
            "parallel (auto)"
        };
        println!("    {:<16}  {:>7.3}s", tag, best);
    }

    // ---- Tagging: parallel writes (destructive; restore between) --------- //
    println!("\n[3] Tag writing (tag_files) — destructive, corpus restored between runs");
    let paths: Vec<PathBuf> = collect_audio_paths(corpus.dir.path(), false).unwrap();
    let mut tag_base = 0f64;
    for &par in &[1usize, 0usize] {
        corpus.restore();
        let t = Instant::now();
        let results = tagging::tag_files(&paths, par, || {});
        let elapsed = secs(t.elapsed());
        let ok = results
            .iter()
            .filter(|(_, r)| matches!(r, Ok(tagging::TagStatus::Ok)))
            .count();
        if par == 1 {
            tag_base = elapsed;
            println!(
                "    {:<16}  {:>7.3}s  {:>8}       ({ok} tagged)",
                "sequential", elapsed, "1.00x"
            );
        } else {
            println!(
                "    {:<16}  {:>7.3}s  {:>7.2}x       ({ok} tagged)",
                "parallel (auto)",
                elapsed,
                tag_base / elapsed
            );
        }
    }
    corpus.restore();

    println!("\nDone. (corpus auto-deleted)");
    drop(corpus); // explicit: temp dir removed here
}
