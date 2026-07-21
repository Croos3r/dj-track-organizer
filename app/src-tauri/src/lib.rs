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
    /// Worker threads for the file-heavy steps (tag reading/writing, dedup
    /// hashing). 0 = auto (all cores); 1 = sequential.
    pub max_threads: usize,
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
            max_threads: 0,
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
    std::fs::write(&p, serde_json::to_string_pretty(&settings).unwrap()).map_err(|e| e.to_string())
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

#[derive(Serialize)]
struct HealthReport {
    scanned_at: String,
    folder: String,
    score: u8,
    audio_files: usize,
    rename_issues: usize,
    manual_review: usize,
    file_duplicate_groups: usize,
    file_duplicate_extras: usize,
    rekordbox: Option<RekordboxHealth>,
    issues: Vec<HealthIssue>,
}

#[derive(Serialize)]
struct RekordboxHealth {
    db_path: String,
    exists: bool,
    running: bool,
    missing_files: usize,
    missing_file_samples: Vec<String>,
    collection_duplicate_groups: usize,
    collection_duplicate_extras: usize,
    inspection_error: Option<String>,
}

#[derive(Serialize)]
struct HealthIssue {
    id: String,
    severity: String,
    title: String,
    count: usize,
    description: String,
}

fn health_issue(
    id: &str,
    severity: &str,
    title: impl Into<String>,
    count: usize,
    description: impl Into<String>,
) -> HealthIssue {
    HealthIssue {
        id: id.to_string(),
        severity: severity.to_string(),
        title: title.into(),
        count,
        description: description.into(),
    }
}

#[tauri::command]
async fn health_scan(folder: String, settings: Settings) -> Result<HealthReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = PathBuf::from(&folder);
        let opts = norm_options(&settings);
        let use_tags = settings.prefer_tags;
        let tags: std::collections::HashMap<String, (String, String)> = if use_tags {
            let paths = dedup::collect_audio_paths(&dir, false).map_err(|e| e.to_string())?;
            dedup::read_tags_by_name(&paths, settings.max_threads)
        } else {
            std::collections::HashMap::new()
        };
        let rows = normalize::build_plan(&dir, move |name| tags.get(name).cloned(), &opts)
            .map_err(|e| e.to_string())?;
        let audio_files = rows.len();
        let rename_issues = rows.iter().filter(|r| !r.new.is_empty() && r.new != r.old).count();
        let manual_review = rows.iter().filter(|r| r.new.is_empty()).count();

        // This is the same scan used by the existing duplicate workflow, but
        // deliberately omits its report writer so a health scan stays read-only.
        let infos = dedup::scan_with(&dir, false, settings.max_threads, |path| {
            if use_tags {
                tagging::read_tags(path).map(|(artist, title)| (artist, title, None))
            } else {
                None
            }
        })
        .map_err(|e| e.to_string())?;
        let file_groups = dedup::find_duplicates_with(
            &infos,
            dedup::Mode::Both,
            dedup::HashStrategy { parallelism: settings.max_threads, ..Default::default() },
        )
        .map_err(|e| e.to_string())?;
        let file_duplicate_groups = file_groups.len();
        let file_duplicate_extras = file_groups.iter().map(|g| g.extras.len()).sum();

        let running = rekordbox::is_rekordbox_running();
        let db_path = settings
            .master_db
            .as_ref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(rekordbox::locate_db_path);
        let db_display_path = db_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| settings.master_db.clone())
            .unwrap_or_default();
        let db_exists = db_path.is_some();
        let mut rekordbox = RekordboxHealth {
            db_path: db_display_path,
            exists: db_exists,
            running,
            missing_files: 0,
            missing_file_samples: Vec::new(),
            collection_duplicate_groups: 0,
            collection_duplicate_extras: 0,
            inspection_error: None,
        };

        if db_exists && !running {
            let db_file = db_path.as_ref().expect("db_exists implies a path");
            match rekordbox::open_db(Some(db_file)) {
                Ok(mut db) => {
                    let folder_name = Path::new(&folder)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned());
                    match rekordbox::load_rb_entries(&mut db, folder_name.as_deref()) {
                        Ok(entries) => {
                            let (missing, samples) =
                                organizer_core::health::missing_rekordbox_files(&entries, 5);
                            let collection_groups = rekordbox::find_rb_dup_groups(&entries);
                            rekordbox.missing_files = missing;
                            rekordbox.missing_file_samples = samples;
                            rekordbox.collection_duplicate_groups = collection_groups.len();
                            rekordbox.collection_duplicate_extras = collection_groups
                                .iter()
                                .map(|group| group.extras.len())
                                .sum();
                        }
                        Err(error) => rekordbox.inspection_error = Some(error.to_string()),
                    }
                }
                Err(error) => rekordbox.inspection_error = Some(error.to_string()),
            }
        }

        let database_unavailable = settings.master_db.is_some() && !db_exists;
        let score = organizer_core::health::score(
            rename_issues,
            file_duplicate_extras,
            rekordbox.missing_files,
            database_unavailable,
            rekordbox.collection_duplicate_groups,
        );
        let mut issues = Vec::new();
        if rename_issues > 0 {
            issues.push(health_issue(
                "rename-issues",
                "warning",
                "Files need renaming",
                rename_issues,
                "The existing Organize flow can review these filename changes before applying them.",
            ));
        }
        if manual_review > 0 {
            issues.push(health_issue(
                "manual-review",
                "warning",
                "Files need manual naming",
                manual_review,
                "The normalizer could not confidently derive both an artist and a title.",
            ));
        }
        if file_duplicate_groups > 0 {
            issues.push(health_issue(
                "file-duplicates",
                "warning",
                "Duplicate files found",
                file_duplicate_extras,
                format!(
                    "{} duplicate group{} found. Review the existing duplicate workflow before moving anything.",
                    file_duplicate_groups,
                    if file_duplicate_groups == 1 { "" } else { "s" }
                ),
            ));
        }
        if rekordbox.missing_files > 0 {
            issues.push(health_issue(
                "missing-rekordbox-files",
                "critical",
                "Rekordbox files missing from disk",
                rekordbox.missing_files,
                "Some collection entries point to files that are no longer present. A repair workflow is not available yet.",
            ));
        }
        if rekordbox.collection_duplicate_groups > 0 {
            issues.push(health_issue(
                "collection-duplicates",
                "warning",
                "Duplicate Rekordbox entries found",
                rekordbox.collection_duplicate_extras,
                "Review the existing collection cleanup workflow before changing master.db.",
            ));
        }
        if !rekordbox.exists {
            issues.push(health_issue(
                "rekordbox-unavailable",
                "warning",
                if settings.master_db.is_some() {
                    "Configured Rekordbox database unavailable"
                } else {
                    "Rekordbox database not configured"
                },
                1,
                if settings.master_db.is_some() {
                    "The configured master.db was not found. Folder health was still scanned."
                } else {
                    "Set a master.db path in Settings to include collection checks."
                },
            ));
        } else if rekordbox.running {
            issues.push(health_issue(
                "rekordbox-running",
                "info",
                "Rekordbox is running",
                1,
                "Collection checks were skipped to avoid inspecting master.db while Rekordbox is open.",
            ));
        } else if let Some(error) = &rekordbox.inspection_error {
            issues.push(health_issue(
                "rekordbox-inspection",
                "warning",
                "Rekordbox database could not be inspected",
                1,
                format!("Folder health was still scanned. Database error: {error}"),
            ));
        }
        if issues.is_empty() {
            issues.push(health_issue(
                "library-healthy",
                "info",
                "No issues found",
                0,
                "The selected library passed the available read-only checks.",
            ));
        }

        Ok(HealthReport {
            scanned_at: chrono::Local::now().to_rfc3339(),
            folder,
            score,
            audio_files,
            rename_issues,
            manual_review,
            file_duplicate_groups,
            file_duplicate_extras,
            rekordbox: Some(rekordbox),
            issues,
        })
    })
    .await
    .map_err(|error| error.to_string())?
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
        // Pre-read embedded tags in parallel (the only I/O in planning), keyed
        // by filename, then let build_plan look them up. Skipped entirely when
        // the user prefers filenames, which makes the scan pure CPU.
        let tags: std::collections::HashMap<String, (String, String)> = if use_tags {
            let paths = dedup::collect_audio_paths(&dir, false).map_err(|e| e.to_string())?;
            dedup::read_tags_by_name(&paths, settings.max_threads)
        } else {
            std::collections::HashMap::new()
        };
        let rows = normalize::build_plan(&dir, move |name| tags.get(name).cloned(), &opts)
            .map_err(|e| e.to_string())?;
        let total = rows.len();
        let to_rename = rows
            .iter()
            .filter(|r| !r.new.is_empty() && r.new != r.old)
            .count();
        let already = rows.iter().filter(|r| r.new == r.old).count();
        let manual: Vec<String> = rows
            .iter()
            .filter(|r| r.new.is_empty())
            .map(|r| r.old.clone())
            .collect();
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
async fn tag_folder(
    app: AppHandle,
    folder: String,
    settings: Settings,
) -> Result<TagResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = PathBuf::from(&folder);
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.path())
            .filter(|p| {
                let lower = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                tagging::SUPPORTED.iter().any(|ext| lower.ends_with(ext))
            })
            .collect();
        files.sort();
        let total = files.len();

        // Tag across worker threads (files are independent); each write retries
        // transient locks internally. Progress is emitted from the workers.
        let done = AtomicUsize::new(0);
        let results = tagging::tag_files(&files, settings.max_threads, || {
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(16) || n == total {
                let _ = app.emit(
                    "tag-progress",
                    TagProgress {
                        done: n,
                        total,
                        file: String::new(),
                    },
                );
            }
        });

        let mut tagged = 0;
        let mut skipped = 0;
        let mut errors = Vec::new();
        for (path, result) in results {
            match result {
                Ok(tagging::TagStatus::Ok) => tagged += 1,
                Ok(_) => skipped += 1,
                Err(e) => errors.push((
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    e.to_string(),
                )),
            }
        }
        Ok(TagResult {
            tagged,
            skipped,
            errors,
        })
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
    .unwrap_or(RekordboxStatus {
        running: true,
        db_path: None,
    })
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
        let backup = rekordbox::backup_db(&db_path, &backup_dir)
            .map_err(|e| format!("backup failed ({e}); nothing was written"))?;
        let mut db = rekordbox::open_db(Some(&db_path)).map_err(|e| e.to_string())?;
        let opts = rekordbox::ApplyOptions {
            set_title: settings.set_title,
            refresh_artist: settings.refresh_artist,
            allow_while_running: false,
        };
        let changed = rekordbox::apply_relink(&mut db, &plan, opts).map_err(|e| e.to_string())?;
        Ok(RelinkResult {
            changed,
            backup_path: backup.to_string_lossy().into_owned(),
        })
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
        Ok(RbDedupResult {
            removed,
            backup_path: backup.to_string_lossy().into_owned(),
        })
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
        // Parallel scan (tag reads) + parallel/smart hashing inside
        // find_duplicates. Both preserve the exact output of the sequential
        // baseline (see dedup::dedup_strategies_agree and the oracle parity).
        let infos = dedup::scan_with(&dir, false, settings.max_threads, |p| {
            if use_tags {
                tagging::read_tags(p).map(|(a, t)| (a, t, None))
            } else {
                None
            }
        })
        .map_err(|e| e.to_string())?;
        let scanned = infos.len();
        let strat = dedup::HashStrategy {
            parallelism: settings.max_threads,
            ..Default::default()
        };
        let groups = dedup::find_duplicates_with(&infos, dedup::Mode::Both, strat)
            .map_err(|e| e.to_string())?;
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
            return Ok(MoveResult {
                moved: 0,
                dest: dest.to_string_lossy().into_owned(),
            });
        }
        let group = dedup::DupGroup {
            kind: dedup::DupKind::SameSong,
            keeper: infos[0].clone(), // placeholder; only extras are moved
            extras: infos,
        };
        let moved = dedup::move_extras(&[group], &dest).map_err(|e| e.to_string())?;
        Ok(MoveResult {
            moved: moved.len(),
            dest: dest.to_string_lossy().into_owned(),
        })
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
            health_scan,
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
