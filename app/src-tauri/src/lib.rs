// SPDX-License-Identifier: GPL-3.0-only
//! Thin Tauri command layer over organizer-core. Long operations run on
//! blocking threads and report progress through window events.

use std::path::{Path, PathBuf};

use organizer_core::{csvio, dedup, normalize, rekordbox, tagging};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

// --------------------------------------------------------------------------- #
// Settings
// --------------------------------------------------------------------------- #

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub last_folder: Option<String>,
    pub master_db: Option<String>,
    pub backup_dir: Option<String>,
    pub duplicates_dir: Option<String>,
    pub alphabetical_artists: bool,
    pub prefer_tags: bool,
    pub set_title: bool,
    pub refresh_artist: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            last_folder: None,
            master_db: None,
            backup_dir: None,
            duplicates_dir: None,
            alphabetical_artists: true,
            prefer_tags: true,
            set_title: true,
            refresh_artist: true,
        }
    }
}

fn settings_path(app: &AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    dir.join("settings.json")
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Settings {
    std::fs::read_to_string(settings_path(&app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    let p = settings_path(&app);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(&settings).unwrap())
        .map_err(|e| e.to_string())
}

// --------------------------------------------------------------------------- #
// Folder picking
// --------------------------------------------------------------------------- #

#[tauri::command]
async fn pick_folder(initial: Option<String>) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dlg = rfd::FileDialog::new().set_title("Choose your track folder");
        if let Some(dir) = initial.filter(|d| Path::new(d).is_dir()) {
            dlg = dlg.set_directory(dir);
        }
        dlg.pick_folder().map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .ok()
    .flatten()
}

#[tauri::command]
fn reveal_path(path: String) {
    let _ = std::process::Command::new("explorer")
        .arg(if Path::new(&path).is_dir() {
            path.clone()
        } else {
            format!("/select,{path}")
        })
        .spawn();
}

// --------------------------------------------------------------------------- #
// Step 1: normalize
// --------------------------------------------------------------------------- #

#[derive(Serialize)]
struct ScanResult {
    rows: Vec<normalize::PlanRow>,
    total: usize,
    to_rename: usize,
    already_correct: usize,
    manual: Vec<String>,
    used_tags: bool,
}

fn norm_options(s: &Settings) -> normalize::Options {
    normalize::Options {
        source: if s.prefer_tags {
            normalize::Source::Tags
        } else {
            normalize::Source::Filename
        },
        alphabetical: s.alphabetical_artists,
        keep_mix: true,
    }
}

#[tauri::command]
async fn scan_plan(folder: String, settings: Settings) -> Result<ScanResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let opts = norm_options(&settings);
        let dir = PathBuf::from(&folder);
        let use_tags = settings.prefer_tags;
        let tag_dir = dir.clone();
        let rows = normalize::build_plan(
            &dir,
            move |name| {
                if use_tags {
                    tagging::read_tags(&tag_dir.join(name))
                } else {
                    None
                }
            },
            &opts,
        )
        .map_err(|e| e.to_string())?;
        let total = rows.len();
        let to_rename = rows.iter().filter(|r| !r.new.is_empty() && r.new != r.old).count();
        let already = rows.iter().filter(|r| r.new == r.old).count();
        let manual: Vec<String> =
            rows.iter().filter(|r| r.new.is_empty()).map(|r| r.old.clone()).collect();
        Ok(ScanResult {
            rows,
            total,
            to_rename,
            already_correct: already,
            manual,
            used_tags: use_tags,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
struct RenameResult {
    renamed: usize,
    skipped: usize,
    rollback_path: String,
    plan_path: String,
}

#[tauri::command]
async fn apply_renames(
    folder: String,
    rows: Vec<normalize::PlanRow>,
) -> Result<RenameResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = PathBuf::from(&folder);
        let changes: Vec<(String, String)> = rows
            .iter()
            .filter(|r| !r.new.is_empty() && r.new != r.old)
            .map(|r| (r.old.clone(), r.new.clone()))
            .collect();
        let plan_path = dir.join("rename_plan.csv");
        csvio::write_plan_csv(&plan_path, &rows).map_err(|e| e.to_string())?;
        let outcome = normalize::two_phase_rename(&dir, &changes).map_err(|e| e.to_string())?;
        let rollback_path = dir.join("rename_rollback.csv");
        csvio::write_rollback_csv(&rollback_path, &outcome.done).map_err(|e| e.to_string())?;
        Ok(RenameResult {
            renamed: outcome.done.len(),
            skipped: outcome.skipped.len(),
            rollback_path: rollback_path.to_string_lossy().into_owned(),
            plan_path: plan_path.to_string_lossy().into_owned(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// --------------------------------------------------------------------------- #
// Step 2: tag from filename
// --------------------------------------------------------------------------- #

#[derive(Clone, Serialize)]
struct TagProgress {
    done: usize,
    total: usize,
    file: String,
}

#[derive(Serialize)]
struct TagResult {
    tagged: usize,
    skipped: usize,
    errors: Vec<(String, String)>,
}

#[tauri::command]
async fn tag_folder(app: AppHandle, folder: String) -> Result<TagResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = PathBuf::from(&folder);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.path())
            .filter(|p| {
                let lower = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                tagging::SUPPORTED.iter().any(|ext| lower.ends_with(ext))
            })
            .collect();
        files.sort();
        let total = files.len();
        let mut tagged = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        for (i, path) in files.iter().enumerate() {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            // tag writing is idempotent, so a transient lock (antivirus / the
            // Windows search indexer briefly holding the file) is worth a short
            // retry before we report it as an error.
            let mut result = tagging::tag_file(path);
            for delay in [50u64, 120, 300] {
                match &result {
                    Err(tagging::TagError::Io(e))
                        if matches!(e.raw_os_error(), Some(32) | Some(33)) =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(delay));
                        result = tagging::tag_file(path);
                    }
                    _ => break,
                }
            }
            match result {
                Ok(tagging::TagStatus::Ok) => tagged += 1,
                Ok(_) => skipped += 1,
                Err(e) => errors.push((name.clone(), e.to_string())),
            }
            if i % 10 == 0 || i + 1 == total {
                let _ = app.emit("tag-progress", TagProgress { done: i + 1, total, file: name });
            }
        }
        Ok(TagResult { tagged, skipped, errors })
    })
    .await
    .map_err(|e| e.to_string())?
}

// --------------------------------------------------------------------------- #
// Step 3: Rekordbox
// --------------------------------------------------------------------------- #

#[derive(Serialize)]
struct RekordboxStatus {
    running: bool,
    db_path: Option<String>,
}

#[tauri::command]
async fn rekordbox_status(settings: Settings) -> RekordboxStatus {
    tauri::async_runtime::spawn_blocking(move || {
        let db_path = settings
            .master_db
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(rekordbox::locate_db_path);
        RekordboxStatus {
            running: rekordbox::is_rekordbox_running(),
            db_path: db_path.map(|p| p.to_string_lossy().into_owned()),
        }
    })
    .await
    .unwrap_or(RekordboxStatus { running: true, db_path: None })
}

fn resolve_db(settings: &Settings) -> Result<PathBuf, String> {
    settings
        .master_db
        .as_ref()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(rekordbox::locate_db_path)
        .ok_or_else(|| "could not locate master.db — set its path in Settings".to_string())
}

#[tauri::command]
async fn rekordbox_plan(
    settings: Settings,
    mapping: Vec<(String, String)>,
    folder_filter: Option<String>,
) -> Result<Vec<rekordbox::RelinkItem>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db_path = resolve_db(&settings)?;
        let mut db = rekordbox::open_db(Some(&db_path)).map_err(|e| e.to_string())?;
        rekordbox::build_relink_plan(&mut db, &mapping, folder_filter.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
struct RelinkResult {
    changed: usize,
    backup_path: String,
}

#[tauri::command]
async fn rekordbox_apply(
    settings: Settings,
    plan: Vec<rekordbox::RelinkItem>,
) -> Result<RelinkResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if rekordbox::is_rekordbox_running() {
            return Err("Rekordbox is running — close it fully first".to_string());
        }
        let db_path = resolve_db(&settings)?;
        let backup_dir = settings
            .backup_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| db_path.parent().unwrap().join("rekordbox_backups"));
        let backup =
            rekordbox::backup_db(&db_path, &backup_dir).map_err(|e| {
                format!("backup failed ({e}); nothing was written")
            })?;
        let mut db = rekordbox::open_db(Some(&db_path)).map_err(|e| e.to_string())?;
        let opts = rekordbox::ApplyOptions {
            set_title: settings.set_title,
            refresh_artist: settings.refresh_artist,
            allow_while_running: false,
        };
        let changed =
            rekordbox::apply_relink(&mut db, &plan, opts).map_err(|e| e.to_string())?;
        Ok(RelinkResult { changed, backup_path: backup.to_string_lossy().into_owned() })
    })
    .await
    .map_err(|e| e.to_string())?
}

// --------------------------------------------------------------------------- #
// Step 3b: Rekordbox collection cleanup (duplicate entries in master.db)
// --------------------------------------------------------------------------- #

#[derive(Serialize)]
struct RbDedupScan {
    groups: Vec<rekordbox::RbDupGroup>,
    entries: usize,
    extras: usize,
    report_path: String,
}

#[tauri::command]
async fn rekordbox_dedup_scan(folder: String, settings: Settings) -> Result<RbDedupScan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db_path = resolve_db(&settings)?;
        let mut db = rekordbox::open_db(Some(&db_path)).map_err(|e| e.to_string())?;
        // scope to entries whose stored path mentions the library folder name
        let folder_name = Path::new(&folder)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let entries = rekordbox::load_rb_entries(&mut db, folder_name.as_deref())
            .map_err(|e| e.to_string())?;
        let n = entries.len();
        let groups = rekordbox::find_rb_dup_groups(&entries);
        let report_path = PathBuf::from(&folder).join("rekordbox_duplicates.csv");
        let extras =
            rekordbox::write_rb_dedup_report(&report_path, &groups).map_err(|e| e.to_string())?;
        Ok(RbDedupScan {
            groups,
            entries: n,
            extras,
            report_path: report_path.to_string_lossy().into_owned(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
struct RbDedupResult {
    removed: usize,
    backup_path: String,
}

#[tauri::command]
async fn rekordbox_dedup_apply(
    settings: Settings,
    groups: Vec<rekordbox::RbDupGroup>,
) -> Result<RbDedupResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if rekordbox::is_rekordbox_running() {
            return Err("Rekordbox is running — close it fully first".to_string());
        }
        let db_path = resolve_db(&settings)?;
        let backup_dir = settings
            .backup_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| db_path.parent().unwrap().join("rekordbox_backups"));
        let backup = rekordbox::backup_db(&db_path, &backup_dir)
            .map_err(|e| format!("backup failed ({e}); nothing was written"))?;
        let mut db = rekordbox::open_db(Some(&db_path)).map_err(|e| e.to_string())?;
        let removed =
            rekordbox::apply_rb_dedup(&mut db, &groups, false).map_err(|e| e.to_string())?;
        Ok(RbDedupResult { removed, backup_path: backup.to_string_lossy().into_owned() })
    })
    .await
    .map_err(|e| e.to_string())?
}

// --------------------------------------------------------------------------- #
// Step 4: dedup
// --------------------------------------------------------------------------- #

#[derive(Serialize)]
struct DedupScan {
    groups: Vec<dedup::DupGroup>,
    report_path: String,
    extras: usize,
    scanned: usize,
}

#[tauri::command]
async fn dedup_scan(folder: String, settings: Settings) -> Result<DedupScan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = PathBuf::from(&folder);
        let use_tags = settings.prefer_tags;
        let infos = dedup::scan(&dir, false, |p| {
            if use_tags {
                tagging::read_tags(p).map(|(a, t)| (a, t, None))
            } else {
                None
            }
        })
        .map_err(|e| e.to_string())?;
        let scanned = infos.len();
        let groups = dedup::find_duplicates(&infos, dedup::Mode::Both).map_err(|e| e.to_string())?;
        let report_path = dir.join("duplicates.csv");
        let extras = dedup::write_report(&report_path, &groups).map_err(|e| e.to_string())?;
        Ok(DedupScan {
            groups,
            report_path: report_path.to_string_lossy().into_owned(),
            extras,
            scanned,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
struct MoveResult {
    moved: usize,
    dest: String,
}

#[tauri::command]
async fn dedup_move(
    folder: String,
    settings: Settings,
    extras: Vec<String>,
) -> Result<MoveResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dest = settings
            .duplicates_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&folder).join("_duplicates"));
        // one synthetic group carrying exactly the extras the user ticked
        let infos: Vec<dedup::FileInfo> = extras
            .iter()
            .map(|p| {
                let path = PathBuf::from(p);
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                dedup::FileInfo {
                    path,
                    artist: String::new(),
                    title: String::new(),
                    dur: None,
                    size,
                }
            })
            .collect();
        if infos.is_empty() {
            return Ok(MoveResult { moved: 0, dest: dest.to_string_lossy().into_owned() });
        }
        let group = dedup::DupGroup {
            kind: dedup::DupKind::SameSong,
            keeper: infos[0].clone(), // placeholder; only extras are moved
            extras: infos,
        };
        let moved = dedup::move_extras(&[group], &dest).map_err(|e| e.to_string())?;
        Ok(MoveResult { moved: moved.len(), dest: dest.to_string_lossy().into_owned() })
    })
    .await
    .map_err(|e| e.to_string())?
}

// --------------------------------------------------------------------------- #

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            pick_folder,
            reveal_path,
            scan_plan,
            apply_renames,
            tag_folder,
            rekordbox_status,
            rekordbox_plan,
            rekordbox_apply,
            rekordbox_dedup_scan,
            rekordbox_dedup_apply,
            dedup_scan,
            dedup_move
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
