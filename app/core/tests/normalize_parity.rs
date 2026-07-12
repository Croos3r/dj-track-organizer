// SPDX-License-Identifier: GPL-3.0-only
//! Parity tests: the Rust normalizer must reproduce the Python oracle
//! (skills/normalize-music-filenames) on the committed fixtures.

use organizer_core::normalize::{build_name, build_plan_for_names, Options, Source};
use serde::Deserialize;

#[derive(Deserialize)]
struct NameCase {
    file: String,
    source: String,
    alphabetical: bool,
    keep_mix: bool,
    new: String,
    origin: String,
}

#[derive(Deserialize)]
struct PlanRow {
    old: String,
    new: String,
    source: String,
}

#[derive(Deserialize)]
struct PlanCase {
    name: String,
    files: Vec<String>,
    rows: Vec<PlanRow>,
}

fn options(source: &str, alphabetical: bool, keep_mix: bool) -> Options {
    Options {
        source: match source {
            "filename" => Source::Filename,
            _ => Source::Tags,
        },
        alphabetical,
        keep_mix,
    }
}

#[test]
fn build_name_matches_python_oracle() {
    let data = include_str!("fixtures/normalize_build_name.json");
    let cases: Vec<NameCase> = serde_json::from_str(data).unwrap();
    assert!(cases.len() > 200, "fixture set unexpectedly small");
    let mut failures = Vec::new();
    for c in &cases {
        let opts = options(&c.source, c.alphabetical, c.keep_mix);
        // Oracle fixtures were generated without ffprobe: no embedded tags.
        let (new, origin) = build_name(&c.file, None, &opts);
        if new != c.new || origin != c.origin {
            failures.push(format!(
                "{:?} (src={} alpha={} keep={}): got ({:?}, {}) want ({:?}, {})",
                c.file, c.source, c.alphabetical, c.keep_mix, new, origin, c.new, c.origin
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} / {} mismatches:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn build_plan_matches_python_oracle() {
    let data = include_str!("fixtures/normalize_build_plan.json");
    let cases: Vec<PlanCase> = serde_json::from_str(data).unwrap();
    for c in &cases {
        let opts = options("tags", true, true);
        let rows = build_plan_for_names(&c.files, |_| None, &opts);
        let got: Vec<(String, String, String)> = rows
            .iter()
            .map(|r| (r.old.clone(), r.new.clone(), r.origin.clone()))
            .collect();
        let want: Vec<(String, String, String)> = c
            .rows
            .iter()
            .map(|r| (r.old.clone(), r.new.clone(), r.source.clone()))
            .collect();
        assert_eq!(got, want, "scenario {:?}", c.name);
    }
}

#[test]
fn already_normalized_names_are_idempotent() {
    let data = include_str!("fixtures/normalize_build_name.json");
    let cases: Vec<NameCase> = serde_json::from_str(data).unwrap();
    for c in cases.iter().filter(|c| !c.new.is_empty()) {
        let opts = options(&c.source, c.alphabetical, c.keep_mix);
        let (again, _) = build_name(&c.new, None, &opts);
        assert_eq!(
            again, c.new,
            "not idempotent: {:?} normalized to {:?} which re-normalizes to {:?}",
            c.file, c.new, again
        );
    }
}
