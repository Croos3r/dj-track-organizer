// SPDX-License-Identifier: GPL-3.0-only
//! Collection-entry dedup: Rust port vs the Python skill, run on identical
//! copies of a synthetic SQLCipher database (the real master.db is never
//! involved). The Python side opens its copy explicitly by path through
//! pyrekordbox (tools/rb_dedup_oracle.py); the final database states are
//! diffed table by table (IDs and links — USN/timestamps excluded, since the
//! two stacks do their bookkeeping differently).
//!
//! If python or pyrekordbox is unavailable the cross-check is skipped (the
//! pure-Rust assertions still run).

#![cfg(feature = "rekordbox")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use diesel::prelude::*;
use organizer_core::rekordbox::{
    apply_rb_dedup, find_rb_dup_groups, load_rb_entries, write_rb_dedup_report,
};
use rbox::masterdb::{establish_connection, MasterDb};

const DATE_OLD: &str = "2026-01-01 00:00:00.000 +00:00";
const DATE_NEW: &str = "2026-02-01 00:00:00.000 +00:00";

fn exec_all(conn: &mut diesel::SqliteConnection, sql: &str) {
    for stmt in sql.split(';') {
        let s: String = stmt
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        if s.trim().is_empty() {
            continue;
        }
        diesel::sql_query(s).execute(conn).unwrap();
    }
}

fn content_insert(id: &str, path: &str, created: &str, plays: i32, rating: i32) -> String {
    format!(
        "INSERT INTO djmdContent (ID, UUID, rb_data_status, rb_local_data_status, \
         rb_local_deleted, rb_local_synced, rb_local_usn, created_at, updated_at, \
         FolderPath, FileNameL, FileNameS, Title, FileType, ContentLink, MasterDBID, \
         DJPlayCount, Rating) \
         VALUES ('{id}', 'uuid-{id}', 0, 0, 0, 0, 1, '{created}', '{created}', \
         '{path}', '', '', 'T {id}', 0, 0, 'testdb-0001', {plays}, {rating});"
    )
}

fn seed_db(db_path: &Path) {
    let schema = include_str!("fixtures/synthetic_masterdb.sql");
    let mut conn = establish_connection(db_path.to_str().unwrap()).unwrap();
    exec_all(&mut conn, schema);
    let mut seeds = String::new();
    // same-file pair (case/slash variants). C2 carries more info -> keeper.
    seeds += &content_insert("C1", "D:/Music/Track/Alpha - One.wav", DATE_OLD, 0, 0);
    seeds += &content_insert("C2", "d:\\music\\track\\alpha - one.wav", DATE_NEW, 3, 0);
    // same-song pair across formats. C3 (.wav) wins on quality, has no cues;
    // C4's cues must transfer to it.
    seeds += &content_insert("C3", "D:/Music/Track/Beta - Two.wav", DATE_OLD, 0, 0);
    seeds += &content_insert("C4", "D:/Music/Track/Beta - Two.mp3", DATE_OLD, 0, 5);
    // unique entry and a streaming row (loader must skip the latter)
    seeds += &content_insert("C5", "D:/Music/Track/Gamma - Three.flac", DATE_OLD, 0, 0);
    seeds += &content_insert("C6", "spotify:track:xyz", DATE_OLD, 0, 0);
    // cues: C1 two, C2 one, C4 two
    for (cid, content) in [("Q1", "C1"), ("Q2", "C1"), ("Q3", "C2"), ("Q4", "C4"), ("Q5", "C4")] {
        seeds += &format!(
            "INSERT INTO djmdCue (ID, UUID, ContentID, created_at, updated_at, \
             rb_data_status, rb_local_data_status, rb_local_deleted, rb_local_synced, \
             InMsec, InFrame, InMpegFrame, InMpegAbs, OutMsec, OutFrame, OutMpegFrame, \
             OutMpegAbs, Kind, Color) \
             VALUES ('{cid}', 'uuid-{cid}', '{content}', '{DATE_OLD}', '{DATE_OLD}', \
             0, 0, 0, 0, 1000, 0, 0, 0, -1, 0, 0, 0, 0, -1);"
        );
    }
    // playlists: C1 in P1; C2 in P1 (duplicate membership) and P2; C3 in P2
    for (rid, content, playlist) in
        [("S1", "C1", "P1"), ("S2", "C2", "P1"), ("S3", "C2", "P2"), ("S4", "C3", "P2")]
    {
        seeds += &format!(
            "INSERT INTO djmdSongPlaylist (ID, UUID, PlaylistID, ContentID, TrackNo, created_at, \
             updated_at, rb_data_status, rb_local_data_status, rb_local_deleted, rb_local_synced) \
             VALUES ('{rid}', 'uuid-{rid}', '{playlist}', '{content}', 1, '{DATE_OLD}', \
             '{DATE_OLD}', 0, 0, 0, 0);"
        );
    }
    exec_all(&mut conn, &seeds);
}

/// Everything semantically relevant, ordered: contents, cue links, playlist links.
fn dump_state(db_path: &Path) -> (BTreeMap<String, String>, BTreeMap<String, String>, BTreeMap<String, (String, String)>) {
    #[derive(QueryableByName)]
    struct Row3 {
        #[diesel(sql_type = diesel::sql_types::Text)]
        a: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        b: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        c: String,
    }
    let mut conn = establish_connection(db_path.to_str().unwrap()).unwrap();
    let contents: Vec<Row3> = diesel::sql_query(
        "SELECT ID as a, COALESCE(FolderPath,'') as b, COALESCE(Title,'') as c FROM djmdContent ORDER BY ID",
    )
    .load(&mut conn)
    .unwrap();
    let cues: Vec<Row3> = diesel::sql_query(
        "SELECT ID as a, COALESCE(ContentID,'') as b, '' as c FROM djmdCue ORDER BY ID",
    )
    .load(&mut conn)
    .unwrap();
    let sps: Vec<Row3> = diesel::sql_query(
        "SELECT ID as a, COALESCE(ContentID,'') as b, COALESCE(PlaylistID,'') as c FROM djmdSongPlaylist ORDER BY ID",
    )
    .load(&mut conn)
    .unwrap();
    (
        contents.into_iter().map(|r| (r.a, r.b)).collect(),
        cues.into_iter().map(|r| (r.a, r.b)).collect(),
        sps.into_iter().map(|r| (r.a, (r.b, r.c))).collect(),
    )
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

#[test]
fn collection_dedup_matches_python_oracle() {
    let td = tempfile::tempdir().unwrap();
    let rust_db = td.path().join("rust-master.db");
    seed_db(&rust_db);
    let py_db = td.path().join("py-master.db");
    std::fs::copy(&rust_db, &py_db).unwrap();

    // ---- Rust side ----------------------------------------------------- //
    let mut db = MasterDb::new(&rust_db).unwrap();
    let entries = load_rb_entries(&mut db, None).unwrap();
    assert_eq!(entries.len(), 6, "spotify row skipped, SEED1 counted");
    let groups = find_rb_dup_groups(&entries);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].kind, "same-file");
    assert_eq!(groups[0].keeper.id, "C2", "more user data wins the same-file tie");
    assert_eq!(groups[1].kind, "same-song");
    assert_eq!(groups[1].keeper.id, "C3", "lossless wins the same-song group");

    let report = td.path().join("rekordbox_duplicates.csv");
    let extras = write_rb_dedup_report(&report, &groups).unwrap();
    assert_eq!(extras, 2);

    let removed = apply_rb_dedup(&mut db, &groups, true).unwrap();
    assert_eq!(removed, 2);
    drop(db);

    let (contents, cues, sps) = dump_state(&rust_db);
    // C1 and C4 gone; C2, C3, C5, C6 remain
    assert_eq!(
        contents.keys().cloned().collect::<Vec<_>>(),
        vec!["C2", "C3", "C5", "C6", "SEED1"]
    );
    // C1's cues deleted (keeper C2 had one); C4's cues repointed to C3
    assert_eq!(cues.get("Q3").map(String::as_str), Some("C2"));
    assert_eq!(cues.get("Q4").map(String::as_str), Some("C3"));
    assert_eq!(cues.get("Q5").map(String::as_str), Some("C3"));
    assert!(!cues.contains_key("Q1") && !cues.contains_key("Q2"));
    // C1's P1 membership dropped (C2 already in P1); everything else intact
    assert!(!sps.contains_key("S1"));
    assert_eq!(sps.get("S2"), Some(&("C2".to_string(), "P1".to_string())));
    assert_eq!(sps.get("S3"), Some(&("C2".to_string(), "P2".to_string())));
    assert_eq!(sps.get("S4"), Some(&("C3".to_string(), "P2".to_string())));

    // ---- Python oracle on the identical copy ---------------------------- //
    let oracle = repo_root().join("tools").join("rb_dedup_oracle.py");
    let out = std::process::Command::new("python")
        .arg(&oracle)
        .arg(&py_db)
        .arg("--apply")
        .output();
    let out = match out {
        Ok(o) => o,
        Err(_) => {
            eprintln!("skipping cross-check: python not runnable");
            return;
        }
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        if stderr.contains("pyrekordbox is not installed") {
            eprintln!("skipping cross-check: pyrekordbox not installed");
            return;
        }
        panic!("python oracle failed:\n{stderr}");
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("oracle printed JSON");
    assert_eq!(parsed["entries"], 6);
    assert_eq!(parsed["removed"], 2);
    let summary = parsed["summary"].as_array().unwrap();
    let rust_summary: Vec<(String, String, Vec<String>)> = groups
        .iter()
        .map(|g| {
            let mut extras: Vec<String> = g.extras.iter().map(|e| e.path.clone()).collect();
            extras.sort();
            (g.kind.clone(), g.keeper.path.clone(), extras)
        })
        .collect();
    let py_summary: Vec<(String, String, Vec<String>)> = summary
        .iter()
        .map(|s| {
            (
                s["kind"].as_str().unwrap().to_string(),
                s["keeper"].as_str().unwrap().to_string(),
                s["extras"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect(),
            )
        })
        .collect();
    assert_eq!(rust_summary, py_summary, "group/keeper choices differ from the Python oracle");

    // ---- final database states must match ------------------------------- //
    let py_state = dump_state(&py_db);
    let rust_state = dump_state(&rust_db);
    assert_eq!(rust_state, py_state, "post-apply database state differs from the Python oracle");
}
