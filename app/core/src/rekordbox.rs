// SPDX-License-Identifier: GPL-3.0-only
//! Rekordbox collection relinking on top of the `rbox` crate.
//!
//! Port of `skills/rekordbox-sync/scripts/rekordbox_sync.py` with rbox doing
//! the database heavy lifting (SQLCipher key, USN bookkeeping, ANLZ path
//! rewrite, running-Rekordbox refusal). What stays ours: locating/overriding
//! the db path, the timestamped backup that must succeed before any write,
//! plan building from a rename mapping, and the per-track update sequence.
//!
//! Also ports the collection-entry dedup of
//! `skills/dedup-tracks/scripts/dedup_tracks.py --rekordbox-db` (bottom of
//! this file).
//!
//! Improvement over the Python tool: `refresh_artist` maintains the linked
//! djmdArtist table directly (rbox `update_content_artist`), replacing the
//! manual "Reload Tag" step in Rekordbox.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rbox::masterdb::MasterDb;

pub use rbox::is_rekordbox_running;

#[derive(Debug, thiserror::Error)]
pub enum RekordboxError {
    #[error("rekordbox database error: {0}")]
    Rbox(#[from] rbox::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not locate master.db — set its path explicitly in Settings")]
    DbNotFound,
    #[error("Rekordbox is running — close it fully before applying changes")]
    RekordboxRunning,
    #[error("backup failed ({0}); aborting without writing anything")]
    BackupFailed(String),
    #[error("track vanished from the database mid-apply: {0}")]
    ContentVanished(String),
}

pub type Result<T> = std::result::Result<T, RekordboxError>;

/// One track the plan will relink.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RelinkItem {
    pub content_id: String,
    pub old_name: String,
    pub new_name: String,
    pub new_path: String,
    pub title: String,
    pub artist: String,
}

/// Locate the default master.db via Rekordbox's own options file.
pub fn locate_db_path() -> Option<PathBuf> {
    rbox::RekordboxOptions::open().ok().map(|o| o.db_path)
}

/// Open the database at `path`, or the auto-located default.
pub fn open_db(path: Option<&Path>) -> Result<MasterDb> {
    let db = match path {
        Some(p) => MasterDb::new(p)?,
        None => MasterDb::open()?,
    };
    Ok(db)
}

fn split_rb_path(folder_path: &str) -> (&str, &str) {
    match folder_path.rfind(['/', '\\']) {
        Some(i) => (&folder_path[..i], &folder_path[i + 1..]),
        None => ("", folder_path),
    }
}

/// Python plan loop: match collection entries against the rename mapping by
/// current basename, optionally scoped to paths containing `folder_filter`.
pub fn build_relink_plan(
    db: &mut MasterDb,
    mapping: &[(String, String)],
    folder_filter: Option<&str>,
) -> Result<Vec<RelinkItem>> {
    let map: HashMap<&str, &str> =
        mapping.iter().map(|(o, n)| (o.as_str(), n.as_str())).collect();
    let mut plan = Vec::new();
    for c in db.get_contents()? {
        let folder_path = c.folder_path.clone().unwrap_or_default();
        if let Some(filter) = folder_filter {
            if !folder_path.contains(filter) {
                continue;
            }
        }
        let (dir, base) = split_rb_path(&folder_path);
        let cur_name = if !base.is_empty() {
            base.to_string()
        } else {
            c.file_name_l.clone().unwrap_or_default()
        };
        if let Some(&new_name) = map.get(cur_name.as_str()) {
            if new_name != cur_name {
                // Rekordbox stores forward slashes; keep the original directory.
                let new_path = format!("{}/{}", dir.replace('\\', "/"), new_name);
                let (artist, title) = crate::tagging::parse_name(new_name);
                plan.push(RelinkItem {
                    content_id: c.id.clone(),
                    old_name: cur_name,
                    new_name: new_name.to_string(),
                    new_path,
                    title,
                    artist,
                });
            }
        }
    }
    Ok(plan)
}

/// Timestamped backup of master.db (plus -wal/-shm side files) into
/// `backup_dir`. Any failure aborts the whole apply.
pub fn backup_db(db_file: &Path, backup_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(backup_dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dest = backup_dir.join(format!("master.db.{stamp}.bak"));
    std::fs::copy(db_file, &dest)?;
    for ext in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{}", db_file.display(), ext));
        if side.exists() {
            std::fs::copy(&side, PathBuf::from(format!("{}{}", dest.display(), ext)))?;
        }
    }
    Ok(dest)
}

#[derive(Clone, Copy, Debug)]
pub struct ApplyOptions {
    /// Set the Title column from the new filename.
    pub set_title: bool,
    /// Maintain the linked artist table from the new filename (replaces the
    /// manual "Reload Tag" step).
    pub refresh_artist: bool,
    /// Tests only: write to a synthetic db even while Rekordbox runs.
    pub allow_while_running: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        ApplyOptions { set_title: true, refresh_artist: true, allow_while_running: false }
    }
}

/// Apply the relink plan. The caller must have backed up the database first
/// (`backup_db`) — this function refuses to run while Rekordbox is open.
///
/// Per track: `update_content_path` (validates the file exists, rewrites the
/// ANLZ analysis paths, updates FolderPath), then one full-row update carrying
/// FileNameL/FileNameS/Title, which is also what bumps `rb_local_usn` and
/// `updated_at` exactly once per track.
pub fn apply_relink(db: &mut MasterDb, plan: &[RelinkItem], opts: ApplyOptions) -> Result<usize> {
    if opts.allow_while_running {
        db.set_unsafe_writes(true);
    } else if is_rekordbox_running() {
        return Err(RekordboxError::RekordboxRunning);
    }
    let mut changed = 0;
    for item in plan {
        // the renamed file must already exist on disk (the pipeline renames first)
        if !Path::new(&item.new_path).is_file() {
            return Err(RekordboxError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("renamed file not found on disk: {}", item.new_path),
            )));
        }
        // update_content_path also rewrites the ANLZ analysis paths, but needs
        // the library's `share` dir; fall back to a plain path update without it.
        let path_updated = match db.update_content_path(&item.content_id, &item.new_path) {
            Ok(_) => true,
            Err(rbox::Error::Database(msg)) if msg.contains("Share dir not set") => false,
            Err(e) => return Err(e.into()),
        };
        let mut c = db
            .get_content_by_id(&item.content_id)?
            .ok_or_else(|| RekordboxError::ContentVanished(item.content_id.clone()))?;
        if !path_updated {
            c.folder_path = Some(item.new_path.replace('\\', "/"));
        }
        c.file_name_l = Some(item.new_name.clone());
        c.file_name_s = Some(item.new_name.clone());
        if opts.set_title && !item.title.is_empty() {
            c.title = Some(item.title.clone());
        }
        db.update_content(&c)?;
        if opts.refresh_artist && !item.artist.is_empty() {
            db.update_content_artist(&item.content_id, &item.artist)?;
        }
        changed += 1;
    }
    Ok(changed)
}

// --------------------------------------------------------------------------- #
// Collection-entry dedup (port of the Python tool's --rekordbox-db mode)
// --------------------------------------------------------------------------- #

/// One collection entry with the user data the keeper choice weighs.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RbEntry {
    pub id: String,
    pub path: String,
    pub ext: String,
    pub title: String,
    pub created: String,
    /// Cue row IDs belonging to this entry.
    pub cue_ids: Vec<String>,
    /// (songPlaylist row ID, playlist ID) memberships.
    pub playlist_rows: Vec<(String, String)>,
    pub plays: i32,
    pub rating: i32,
    pub comment: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RbDupGroup {
    /// "same-file" | "same-song"
    pub kind: String,
    pub keeper: RbEntry,
    pub extras: Vec<RbEntry>,
}

/// Python `rb_path_key`: canonical form of a stored path so case / slash
/// variants collide (NFC + lowercase + backslashes + collapse `.` and `..`).
fn rb_path_key(p: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let nfc: String = p.nfc().collect();
    let lower = nfc.to_lowercase();
    let mut parts: Vec<&str> = Vec::new();
    for seg in lower.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.len() > 1 || (parts.len() == 1 && !parts[0].ends_with(':')) {
                    parts.pop();
                }
            }
            s => parts.push(s),
        }
    }
    parts.join("\\")
}

/// Python `rb_info_score`: how much user data an entry carries.
fn rb_info_score(e: &RbEntry) -> i64 {
    e.cue_ids.len() as i64 * 3
        + e.playlist_rows.len() as i64 * 2
        + e.plays as i64
        + i64::from(e.rating != 0)
        + i64::from(!e.comment.is_empty())
}

/// Python `rb_keeper`: best quality first; ties by most info, then oldest.
fn rb_keeper(mut group: Vec<RbEntry>) -> (RbEntry, Vec<RbEntry>) {
    group.sort_by(|a, b| {
        let ka = (-crate::dedup::quality_rank(&a.ext), -rb_info_score(a));
        let kb = (-crate::dedup::quality_rank(&b.ext), -rb_info_score(b));
        ka.cmp(&kb)
            .then_with(|| a.created.cmp(&b.created))
            .then_with(|| a.id.cmp(&b.id))
    });
    let keeper = group.remove(0);
    (keeper, group)
}

fn rbox_db_err(e: diesel::result::Error) -> RekordboxError {
    RekordboxError::Rbox(rbox::Error::Database(e.to_string()))
}

fn pool_err<E: std::fmt::Display>(e: E) -> RekordboxError {
    RekordboxError::Rbox(rbox::Error::Database(format!("connection pool: {e}")))
}

/// Python `rb_load_entries`: one entry per collection row, scoped by an
/// optional case-insensitive path filter; streaming/spotify rows skipped.
pub fn load_rb_entries(db: &mut MasterDb, folder_filter: Option<&str>) -> Result<Vec<RbEntry>> {
    use rbox::masterdb::models::{DjmdCue, DjmdSongPlaylist};
    use rbox::model_traits::Model;

    let mut sp_by_content: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut cues_by_content: HashMap<String, Vec<String>> = HashMap::new();
    {
        let mut conn = db.pool.get().map_err(pool_err)?;
        for sp in DjmdSongPlaylist::all(&mut conn).map_err(rbox_db_err)? {
            sp_by_content
                .entry(sp.content_id.clone())
                .or_default()
                .push((sp.id.clone(), sp.playlist_id.clone()));
        }
        for cue in DjmdCue::all(&mut conn).map_err(rbox_db_err)? {
            cues_by_content
                .entry(cue.content_id.clone())
                .or_default()
                .push(cue.id.clone());
        }
    }

    let mut entries = Vec::new();
    for c in db.get_contents()? {
        let path = c.folder_path.clone().unwrap_or_default();
        if path.is_empty() || path.starts_with("spotify:") {
            continue;
        }
        if let Some(filter) = folder_filter {
            if !path.to_lowercase().contains(&filter.to_lowercase()) {
                continue;
            }
        }
        let (_, base) = split_rb_path(&path);
        let ext = crate::normalize::splitext(base).1.to_lowercase();
        entries.push(RbEntry {
            id: c.id.clone(),
            ext,
            title: c.title.clone().unwrap_or_default(),
            created: c.created_at.format("%Y-%m-%d %H:%M:%S%.6f%:z").to_string(),
            cue_ids: cues_by_content.remove(&c.id).unwrap_or_default(),
            playlist_rows: sp_by_content.remove(&c.id).unwrap_or_default(),
            plays: c.dj_play_count.unwrap_or(0),
            rating: c.rating.unwrap_or(0),
            comment: c.commnt.clone().unwrap_or_default().trim().to_string(),
            path,
        });
    }
    Ok(entries)
}

/// Python `rb_find_groups` + `rb_keeper`: same-file groups first, then
/// same-song (parsed from the stored filename) across the remaining entries.
pub fn find_rb_dup_groups(entries: &[RbEntry]) -> Vec<RbDupGroup> {
    let mut raw_groups: Vec<(String, Vec<RbEntry>)> = Vec::new();
    let mut grouped_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();

    let mut order: Vec<String> = Vec::new();
    let mut by_path: HashMap<String, Vec<&RbEntry>> = HashMap::new();
    for e in entries {
        let k = rb_path_key(&e.path);
        if !by_path.contains_key(&k) {
            order.push(k.clone());
        }
        by_path.entry(k).or_default().push(e);
    }
    for k in &order {
        let items = &by_path[k];
        if items.len() > 1 {
            grouped_ids.extend(items.iter().map(|e| e.id.as_str()));
            raw_groups
                .push(("same-file".into(), items.iter().map(|e| (*e).clone()).collect()));
        }
    }

    let mut sorder: Vec<String> = Vec::new();
    let mut by_song: HashMap<String, Vec<&RbEntry>> = HashMap::new();
    for e in entries {
        if grouped_ids.contains(e.id.as_str()) {
            continue;
        }
        let (_, base) = split_rb_path(&e.path);
        let (a, t) = crate::tagging::parse_name(base);
        if t.is_empty() {
            continue;
        }
        let k = crate::dedup::norm_key(&a, &t);
        if !by_song.contains_key(&k) {
            sorder.push(k.clone());
        }
        by_song.entry(k).or_default().push(e);
    }
    for k in &sorder {
        let items = &by_song[k];
        let distinct_paths: std::collections::HashSet<String> =
            items.iter().map(|e| rb_path_key(&e.path)).collect();
        if items.len() > 1 && distinct_paths.len() > 1 {
            raw_groups
                .push(("same-song".into(), items.iter().map(|e| (*e).clone()).collect()));
        }
    }

    raw_groups
        .into_iter()
        .map(|(kind, g)| {
            let (keeper, extras) = rb_keeper(g);
            RbDupGroup { kind, keeper, extras }
        })
        .collect()
}

/// Python `rb_merge_into_keeper` + apply loop: move each extra's playlist
/// memberships onto the keeper (dropping ones the keeper already has), carry
/// the cues over only when the keeper has none, then delete the extra row.
///
/// The caller must have backed up the database first; this refuses to run
/// while Rekordbox is open unless `allow_while_running` (tests) is set.
pub fn apply_rb_dedup(
    db: &mut MasterDb,
    groups: &[RbDupGroup],
    allow_while_running: bool,
) -> Result<usize> {
    use diesel::prelude::*;
    use rbox::masterdb::schema::{djmdContent, djmdCue, djmdSongPlaylist};

    if allow_while_running {
        db.set_unsafe_writes(true);
    } else if is_rekordbox_running() {
        return Err(RekordboxError::RekordboxRunning);
    }
    let mut conn = db.pool.get().map_err(pool_err)?;

    let mut removed = 0;
    for g in groups {
        let mut keeper_playlists: std::collections::HashSet<String> =
            g.keeper.playlist_rows.iter().map(|(_, pl)| pl.clone()).collect();
        let mut keeper_has_cues = !g.keeper.cue_ids.is_empty();
        for extra in &g.extras {
            for (row_id, playlist_id) in &extra.playlist_rows {
                if keeper_playlists.contains(playlist_id.as_str()) {
                    diesel::delete(djmdSongPlaylist::table.find(row_id))
                        .execute(&mut conn)
                        .map_err(rbox_db_err)?;
                } else {
                    diesel::update(djmdSongPlaylist::table.find(row_id))
                        .set(djmdSongPlaylist::content_id.eq(&g.keeper.id))
                        .execute(&mut conn)
                        .map_err(rbox_db_err)?;
                    keeper_playlists.insert(playlist_id.clone());
                }
            }
            if !keeper_has_cues && !extra.cue_ids.is_empty() {
                diesel::update(djmdCue::table.filter(djmdCue::content_id.eq(&extra.id)))
                    .set(djmdCue::content_id.eq(&g.keeper.id))
                    .execute(&mut conn)
                    .map_err(rbox_db_err)?;
                keeper_has_cues = true;
            } else {
                diesel::delete(djmdCue::table.filter(djmdCue::content_id.eq(&extra.id)))
                    .execute(&mut conn)
                    .map_err(rbox_db_err)?;
            }
            diesel::delete(djmdContent::table.find(&extra.id))
                .execute(&mut conn)
                .map_err(rbox_db_err)?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// rekordbox_duplicates.csv, same columns as the Python tool.
pub fn write_rb_dedup_report(path: &Path, groups: &[RbDupGroup]) -> std::io::Result<usize> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(b"\xef\xbb\xbf")?;
    let mut w = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .quote_style(csv::QuoteStyle::Necessary)
        .from_writer(file);
    w.write_record([
        "group", "kind", "role", "entry_id", "path", "cues", "playlists", "plays", "rating",
        "created",
    ])?;
    let mut extras = 0;
    for (gid, g) in groups.iter().enumerate() {
        let mut rows: Vec<(&str, &RbEntry)> = vec![("keep", &g.keeper)];
        rows.extend(g.extras.iter().map(|e| ("remove", e)));
        for (role, e) in rows {
            w.write_record([
                (gid + 1).to_string().as_str(),
                &g.kind,
                role,
                &e.id,
                &e.path,
                e.cue_ids.len().to_string().as_str(),
                e.playlist_rows.len().to_string().as_str(),
                e.plays.to_string().as_str(),
                e.rating.to_string().as_str(),
                &e.created,
            ])?;
            if role == "remove" {
                extras += 1;
            }
        }
    }
    w.flush()?;
    Ok(extras)
}

#[cfg(test)]
mod rb_dedup_tests {
    use super::*;

    fn entry(id: &str, path: &str) -> RbEntry {
        let (_, base) = split_rb_path(path);
        RbEntry {
            id: id.into(),
            path: path.into(),
            ext: crate::normalize::splitext(base).1.to_lowercase(),
            title: String::new(),
            created: format!("2026-01-0{} 00:00:00.000000+00:00", id.len().min(9)),
            cue_ids: vec![],
            playlist_rows: vec![],
            plays: 0,
            rating: 0,
            comment: String::new(),
        }
    }

    #[test]
    fn path_key_folds_case_slashes_and_dots() {
        assert_eq!(rb_path_key("D:/Music/Track/A.wav"), rb_path_key("d:\\music\\TRACK\\a.wav"));
        assert_eq!(rb_path_key("D:/Music/./Track/../Track/A.wav"), rb_path_key("D:/Music/Track/A.wav"));
        assert_ne!(rb_path_key("D:/Music/Track/A.wav"), rb_path_key("D:/Music/Track/B.wav"));
    }

    #[test]
    fn same_file_beats_same_song_and_scores_decide_keeper() {
        let mut a = entry("1", "D:/T/Artist - Song.wav");
        let b = entry("2", "d:\\t\\artist - song.WAV"); // same file, case variant
        let mut c = entry("3", "D:/T/Artist - Song.mp3"); // same song, other file
        a.cue_ids = vec!["c1".into(), "c2".into()]; // more info -> keeper
        c.plays = 5;
        let groups = find_rb_dup_groups(&[a.clone(), b.clone(), c.clone()]);
        assert_eq!(groups.len(), 1, "same-song group needs 2 distinct paths outside same-file");
        assert_eq!(groups[0].kind, "same-file");
        assert_eq!(groups[0].keeper.id, "1", "entry with cues wins the tie");
        assert_eq!(groups[0].extras.len(), 1);
    }

    #[test]
    fn same_song_needs_distinct_paths_and_quality_wins() {
        let wav = entry("1", "D:/T/Artist - Song.wav");
        let mp3 = entry("2", "D:/T/Artist - Song.mp3");
        let other = entry("3", "D:/T/Other - Track.mp3");
        let groups = find_rb_dup_groups(&[mp3.clone(), wav.clone(), other]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, "same-song");
        assert_eq!(groups[0].keeper.id, "1", "lossless wins regardless of order");
    }
}
