// SPDX-License-Identifier: GPL-3.0-only
//! Byte-parity tests: the Rust tagger must produce bit-identical files to the
//! Python oracle (skills/tag-from-filename) on the committed fixtures.

use std::path::PathBuf;

use organizer_core::tagging::{parse_name, read_tags, tag_file, TagStatus};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    file: String,
    artist: String,
    title: String,
    status: String,
    changed: bool,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn cases() -> Vec<Case> {
    let manifest = std::fs::read_to_string(fixtures_dir().join("tagging_manifest.json")).unwrap();
    serde_json::from_str(&manifest).unwrap()
}

#[test]
fn parse_name_matches_python_oracle() {
    for c in cases() {
        let (a, t) = parse_name(&c.file);
        assert_eq!((a, t), (c.artist.clone(), c.title.clone()), "case {}", c.name);
    }
}

/// Cases where the Rust port deliberately deviates from the Python oracle.
/// wav_info_before_data: the oracle leaves a stale pre-`data` INFO chunk in
/// place and appends a second one; readers keep showing the stale chunk. The
/// port rewrites the file so exactly one INFO remains (asserted below).
const DEVIATIONS: [&str; 1] = ["wav_info_before_data"];

fn count_info_chunks(bytes: &[u8]) -> usize {
    // count LIST chunks whose form type is INFO
    let mut n = 0;
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size =
            u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]])
                as usize;
        if id == b"LIST" && bytes.len() >= pos + 12 && &bytes[pos + 8..pos + 12] == b"INFO" {
            n += 1;
        }
        pos += 8 + size + (size & 1);
    }
    n
}

#[test]
fn tagged_bytes_match_python_oracle() {
    let dir = fixtures_dir().join("tagging");
    let td = tempfile::tempdir().unwrap();
    for c in cases() {
        let input = std::fs::read(dir.join(format!("{}.in", c.name))).unwrap();
        let expected = std::fs::read(dir.join(format!("{}.out", c.name))).unwrap();
        let path = td.path().join(&c.file);
        std::fs::write(&path, &input).unwrap();

        let status = tag_file(&path).unwrap();
        let want_status = match c.status.as_str() {
            "ok" => TagStatus::Ok,
            "skip-noname" => TagStatus::SkipNoName,
            other => panic!("unexpected oracle status {other}"),
        };
        assert_eq!(status, want_status, "status for {}", c.name);

        let got = std::fs::read(&path).unwrap();
        if DEVIATIONS.contains(&c.name.as_str()) {
            // documented deviation: assert semantics instead of bytes
            assert_eq!(count_info_chunks(&got), 1, "{}: exactly one INFO chunk", c.name);
            assert_eq!(count_info_chunks(&expected), 2, "{}: oracle wrote two INFOs", c.name);
            // audio and non-metadata chunks survive
            assert!(
                got.windows(32).any(|w| w == [0xCC; 32]),
                "{}: audio bytes lost",
                c.name
            );
            assert!(
                got.windows(4).any(|w| w == *b"smpl"),
                "{}: smpl chunk lost",
                c.name
            );
            continue;
        }
        assert_eq!(
            got.len(),
            expected.len(),
            "{}: output length differs (got {}, want {})",
            c.name,
            got.len(),
            expected.len()
        );
        assert!(got == expected, "{}: output bytes differ from oracle", c.name);
        assert_eq!(got != input, c.changed, "{}: changed flag mismatch", c.name);
    }
}

#[test]
fn lofty_reads_back_what_we_write() {
    // Cross-compat: the app reads embedded tags with lofty; it must see the
    // artist/title OUR writer produced (WAV INFO, MP3 ID3v2.4, AIFF ID3) —
    // including on the Beatport-layout case where the oracle left stale data.
    let dir = fixtures_dir().join("tagging");
    let td = tempfile::tempdir().unwrap();
    for c in cases() {
        if c.artist.is_empty() {
            continue; // lofty read of artist-less INFO not interesting here
        }
        let input = std::fs::read(dir.join(format!("{}.in", c.name))).unwrap();
        let path = td.path().join(&c.file);
        std::fs::write(&path, &input).unwrap();
        tag_file(&path).unwrap();
        match read_tags(&path) {
            Some((artist, title)) => {
                assert_eq!(artist, c.artist, "{}: artist read-back", c.name);
                assert_eq!(title, c.title, "{}: title read-back", c.name);
            }
            None => panic!("{}: lofty could not read tags back", c.name),
        }
    }
}
