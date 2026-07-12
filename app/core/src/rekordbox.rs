// SPDX-License-Identifier: GPL-3.0-only
//! Rekordbox collection relinking on top of the `rbox` crate.
//!
//! Port of `skills/rekordbox-sync/scripts/rekordbox_sync.py` with rbox doing
//! the database heavy lifting (SQLCipher key, USN bookkeeping, ANLZ path
//! rewrite, running-Rekordbox refusal). What stays ours: locating/overriding
//! the db path, the timestamped backup that must succeed before any write,
//! plan building from a rename mapping, and the per-track update sequence.
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
