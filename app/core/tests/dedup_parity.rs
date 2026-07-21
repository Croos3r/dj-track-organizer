// SPDX-License-Identifier: GPL-3.0-only
//! Parity tests: the Rust dedup must reproduce the Python oracle
//! (skills/dedup-tracks, filesystem mode) on the committed scenarios.

use std::path::PathBuf;

use organizer_core::dedup::{find_duplicates, scan, write_report, Mode};
use serde::Deserialize;

#[derive(Deserialize)]
struct Scenario {
    name: String,
    recursive: bool,
    /// [relative path, content marker, size]
    files: Vec<(String, String, u64)>,
    stdout: String,
    report: String,
}

fn scenarios() -> Vec<Scenario> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dedup_scenarios.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn stdout_count(stdout: &str, label: &str) -> usize {
    stdout
        .lines()
        .find(|l| l.starts_with(label))
        .and_then(|l| l.split(':').nth(1))
        .map(|v| v.trim().parse().unwrap())
        .unwrap_or_else(|| panic!("no `{label}` line in oracle stdout"))
}

#[test]
fn dedup_matches_python_oracle() {
    for s in scenarios() {
        let td = tempfile::tempdir().unwrap();
        for (rel, marker, size) in &s.files {
            let p = td
                .path()
                .join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let content: Vec<u8> = marker.bytes().cycle().take(*size as usize).collect();
            std::fs::write(p, content).unwrap();
        }

        let infos = scan(td.path(), s.recursive, |_| None).unwrap();
        assert_eq!(
            infos.len(),
            stdout_count(&s.stdout, "files scanned"),
            "{}: scanned count",
            s.name
        );

        let groups = find_duplicates(&infos, Mode::Both).unwrap();
        assert_eq!(
            groups.len(),
            stdout_count(&s.stdout, "duplicate sets"),
            "{}: group count",
            s.name
        );

        let report_path = td.path().join("duplicates.csv");
        let extras = write_report(&report_path, &groups).unwrap();
        assert_eq!(
            extras,
            stdout_count(&s.stdout, "extra copies"),
            "{}: extras count",
            s.name
        );

        let bytes = std::fs::read(&report_path).unwrap();
        let text = String::from_utf8(
            bytes
                .strip_prefix(b"\xef\xbb\xbf".as_slice())
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        let root = td.path().to_string_lossy().into_owned();
        let normalized = text.replace(&root, "<ROOT>").replace('\\', "/");
        assert_eq!(
            normalized, s.report,
            "{}: report differs from oracle",
            s.name
        );
    }
}
