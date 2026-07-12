// SPDX-License-Identifier: GPL-3.0-only
//! Rekordbox relink tests against a synthetic SQLCipher master.db built
//! in-test through rbox's own connection (never the user's real database).
//!
//! NOTE: these tests use `allow_while_running` because they operate on a
//! throwaway db file; the production path refuses to write while Rekordbox
//! is open.

#![cfg(feature = "rekordbox")]

use std::path::{Path, PathBuf};

use diesel::RunQueryDsl;
use organizer_core::rekordbox::{
    apply_relink, backup_db, build_relink_plan, ApplyOptions, RelinkItem,
};
use rbox::masterdb::{establish_connection, MasterDb};

fn make_db(dir: &Path, tracks: &[(&str, &str)]) -> (PathBuf, MasterDb) {
    // create the encrypted db file with rbox's key handling + our schema
    let db_path = dir.join("master.db");
    let schema = include_str!("fixtures/synthetic_masterdb.sql");
    {
        let mut conn = establish_connection(db_path.to_str().unwrap()).unwrap();
        for stmt in schema.split(';') {
            let sql: String = stmt
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("
");
            if sql.trim().is_empty() {
                continue;
            }
            diesel::sql_query(sql).execute(&mut conn).unwrap();
        }
    }
    let mut db = MasterDb::new(&db_path).unwrap();
    db.set_unsafe_writes(true); // synthetic db; the user's Rekordbox may be open
    for (subdir, name) in tracks {
        let d = dir.join(subdir);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join(name);
        std::fs::write(&f, b"fake audio").unwrap();
        // rekordbox-style forward slashes
        let rb_path = f.to_string_lossy().replace('\\', "/");
        db.create_content(&rb_path).unwrap();
    }
    (db_path, db)
}

fn mapping(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn plan_matches_only_mapped_names_in_scope() {
    let td = tempfile::tempdir().unwrap();
    let (_p, mut db) = make_db(
        td.path(),
        &[
            ("Track", "Insomniak, Drymk - 1312 (Drymk Remix).wav"),
            ("Track", "Angerfist - And Jesus Wept.wav"),
            ("Other", "Insomniak, Drymk - 1312 (Drymk Remix).mp3"),
        ],
    );
    // rename the .wav; the identically-named .mp3 lives outside the folder filter
    let map = mapping(&[(
        "Insomniak, Drymk - 1312 (Drymk Remix).wav",
        "Drymk, Insomniak - 1312 (Drymk Remix).wav",
    )]);

    let plan = build_relink_plan(&mut db, &map, Some("Track")).unwrap();
    assert_eq!(plan.len(), 1);
    let item = &plan[0];
    assert_eq!(item.old_name, "Insomniak, Drymk - 1312 (Drymk Remix).wav");
    assert_eq!(item.new_name, "Drymk, Insomniak - 1312 (Drymk Remix).wav");
    assert!(item.new_path.ends_with("/Track/Drymk, Insomniak - 1312 (Drymk Remix).wav"));
    assert_eq!(item.title, "1312 (Drymk Remix)");
    assert_eq!(item.artist, "Drymk, Insomniak");

    // no filter: no-op mappings (new == old) still excluded
    let noop = mapping(&[("Angerfist - And Jesus Wept.wav", "Angerfist - And Jesus Wept.wav")]);
    assert!(build_relink_plan(&mut db, &noop, None).unwrap().is_empty());
}

#[test]
fn apply_updates_path_names_title_artist_and_usn_once_per_track() {
    let td = tempfile::tempdir().unwrap();
    let (_p, mut db) = make_db(
        td.path(),
        &[("Track", "Insomniak, Drymk - 1312 (Drymk Remix).wav")],
    );
    // the renamed file must exist on disk before relinking (pipeline order)
    let old = td.path().join("Track/Insomniak, Drymk - 1312 (Drymk Remix).wav");
    let new = td.path().join("Track/Drymk, Insomniak - 1312 (Drymk Remix).wav");
    std::fs::rename(&old, &new).unwrap();

    let map = mapping(&[(
        "Insomniak, Drymk - 1312 (Drymk Remix).wav",
        "Drymk, Insomniak - 1312 (Drymk Remix).wav",
    )]);
    let plan = build_relink_plan(&mut db, &map, None).unwrap();
    assert_eq!(plan.len(), 1);

    let usn_before = db.get_local_usn().unwrap();
    let changed = apply_relink(
        &mut db,
        &plan,
        ApplyOptions { allow_while_running: true, ..Default::default() },
    )
    .unwrap();
    assert_eq!(changed, 1);

    let c = db.get_content_by_id(&plan[0].content_id).unwrap().unwrap();
    assert!(c.folder_path.as_deref().unwrap().ends_with(
        "/Track/Drymk, Insomniak - 1312 (Drymk Remix).wav"
    ));
    assert_eq!(c.file_name_l.as_deref(), Some("Drymk, Insomniak - 1312 (Drymk Remix).wav"));
    assert_eq!(c.file_name_s.as_deref(), Some("Drymk, Insomniak - 1312 (Drymk Remix).wav"));
    assert_eq!(c.title.as_deref(), Some("1312 (Drymk Remix)"));

    // artist refresh linked a djmdArtist row
    let artist_id = c.artist_id.clone().expect("artist linked");
    let artist = db.get_artist_by_id(&artist_id).unwrap().unwrap();
    assert_eq!(artist.name, "Drymk, Insomniak");

    // full-row update bumped the local USN; artist maintenance adds its own
    let usn_after = db.get_local_usn().unwrap();
    assert!(usn_after > usn_before, "usn advanced ({usn_before} -> {usn_after})");
}

#[test]
fn apply_refuses_missing_target_file() {
    let td = tempfile::tempdir().unwrap();
    let (_p, mut db) =
        make_db(td.path(), &[("Track", "A - B.wav")]);
    // do NOT rename on disk: relink must fail (rbox validates the target path)
    let map = mapping(&[("A - B.wav", "B - A.wav")]);
    let plan = build_relink_plan(&mut db, &map, None).unwrap();
    assert_eq!(plan.len(), 1);
    let err = apply_relink(
        &mut db,
        &plan,
        ApplyOptions { allow_while_running: true, ..Default::default() },
    );
    assert!(err.is_err(), "applying without the renamed file on disk must fail");
}

#[test]
fn backup_copies_db_and_side_files_and_fails_loudly() {
    let td = tempfile::tempdir().unwrap();
    let db_file = td.path().join("master.db");
    std::fs::write(&db_file, b"db-bytes").unwrap();
    std::fs::write(td.path().join("master.db-wal"), b"wal").unwrap();
    let backup_dir = td.path().join("backups");

    let dest = backup_db(&db_file, &backup_dir).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"db-bytes");
    assert!(
        PathBuf::from(format!("{}-wal", dest.display())).exists(),
        "wal side file copied"
    );
    assert!(!PathBuf::from(format!("{}-shm", dest.display())).exists());

    // missing source db -> hard error, nothing written
    let missing = td.path().join("nope.db");
    assert!(backup_db(&missing, &backup_dir).is_err());
}

#[test]
fn relink_item_serializes_for_the_ui() {
    let item = RelinkItem {
        content_id: "42".into(),
        old_name: "a.wav".into(),
        new_name: "b.wav".into(),
        new_path: "D:/x/b.wav".into(),
        title: "b".into(),
        artist: "".into(),
    };
    let s = serde_json::to_string(&item).unwrap();
    assert!(s.contains("\"content_id\":\"42\""));
}
