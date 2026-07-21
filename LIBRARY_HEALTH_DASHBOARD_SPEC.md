# Library Health Dashboard — Implementation Spec

## Objective

Add a non-destructive Library Health dashboard to Track Organizer. It should scan the selected music folder and configured Rekordbox database, summarize actionable problems, and let the user open the existing review-gated workflows for each problem.

The first release is an inspection and navigation feature. Do not add automatic metadata lookup, network services, background watching, or destructive fixes beyond invoking existing reviewed operations.

## User outcome

After selecting a folder, the user can click **Check library** and quickly answer:

- Are there duplicate files or duplicate Rekordbox entries?
- Are any Rekordbox tracks missing from disk?
- Are filenames inconsistent or still needing manual naming?
- Is the configured Rekordbox database available and safe to inspect?

The dashboard must make it obvious that scanning does not modify files or `master.db`.

## Scope for this iteration

### Health checks

Implement these checks using existing Rust core functionality wherever possible:

1. **Filename health**
   - Count supported audio files.
   - Count files already matching the normalizer output.
   - Count files that the normalizer would rename.
   - Count files requiring manual review (`new == ""`).

2. **File duplicates**
   - Reuse the existing dedup scan.
   - Report duplicate groups and extra files.
   - Do not move or delete anything during a health scan.

3. **Rekordbox availability**
   - Report whether `master.db` is configured/found.
   - Report whether Rekordbox is currently running.
   - If the database is unavailable, show this as a warning rather than failing the entire dashboard.

4. **Rekordbox collection duplicates**
   - Reuse the existing collection dedup scan when a valid database is available.
   - Report duplicate collection groups and extra entries.

5. **Missing Rekordbox files**
   - For collection entries whose stored file path does not exist, report a count and a short sample.
   - Keep this check read-only.
   - If the current core has no suitable query, add a focused read-only core function rather than duplicating database parsing in TypeScript.

### UI

Add a dashboard section near the top of the current app, above the pipeline steps:

- Header: `Library health`
- A primary `Check library` button
- Last scan timestamp/status
- Overall score from 0–100, with a short label (`Healthy`, `Needs attention`, or `Critical`)
- Summary cards for: files scanned, rename issues, file duplicates, missing Rekordbox files, collection duplicates
- An issue list. Each issue contains severity, title, count, explanation, and an action button where an existing workflow can handle it.
- A compact scan progress state and a visible `Read-only scan — no changes will be made` note.

Actions should be:

- Rename issues → select the folder and run the existing Organize flow, or focus the relevant pipeline step.
- File duplicates → open the existing duplicate review flow.
- Collection duplicates → open the existing collection duplicate review flow.
- Missing files → show the sample paths and a clear `Repair workflow not available yet` state unless a safe repair workflow already exists.

Do not duplicate the full existing review modals in this feature. Extract or refactor shared scan/review entry points only as needed.

## API design

Add a Tauri command such as:

```rust
#[tauri::command]
async fn health_scan(folder: String, settings: Settings) -> Result<HealthReport, String>
```

Suggested serializable types:

```rust
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

struct RekordboxHealth {
    db_path: String,
    exists: bool,
    running: bool,
    missing_files: usize,
    missing_file_samples: Vec<String>,
    collection_duplicate_groups: usize,
    collection_duplicate_extras: usize,
}

struct HealthIssue {
    id: String,
    severity: String, // "info", "warning", "critical"
    title: String,
    count: usize,
    description: String,
}
```

The exact shape may be adjusted to match existing conventions, but keep the frontend independent of internal database models.

## Scoring

Use a transparent deterministic score. Start at 100 and subtract:

- 1 point per 20 files needing rename, capped at 20
- 2 points per 10 duplicate extras, capped at 25
- 2 points per 10 missing Rekordbox files, capped at 30
- 5 points if the database is unavailable when configured
- 1 point per collection duplicate group, capped at 15

Clamp to 0–100. The score is a communication aid, not a quality judgment; do not present it as an objective benchmark.

## Safety and behavior requirements

- Health scans must never rename, tag, move, delete, write CSV reports, or write to `master.db`.
- Keep existing review gates unchanged.
- Do not inspect `master.db` while Rekordbox is running if the existing code forbids it; return availability information instead.
- Partial results are preferable to total failure. For example, a folder scan should still render if Rekordbox is unavailable.
- Preserve the current mock bridge behavior used for frontend development.
- Maintain the existing UTF-8 text behavior and avoid `innerHTML` for user-controlled paths.

## Suggested implementation sequence

1. Inspect existing command registration, core scan APIs, and frontend bridge mocks.
2. Add Rust report types and a read-only `health_scan` command.
3. Add missing-file detection in the core/database layer if necessary, with focused tests.
4. Register the command and update the TypeScript bridge types/mock.
5. Build the dashboard UI and styles, reusing existing buttons, modal, chip, and progress conventions.
6. Add unit/integration coverage for scoring, partial Rekordbox availability, missing files, and duplicate counts.
7. Run `npm run build` and the relevant `cargo test` commands.

## Acceptance criteria

- Selecting a valid folder and clicking `Check library` renders a report without modifying the folder or database.
- The report includes all five health areas and handles missing/unconfigured Rekordbox gracefully.
- Counts match the existing scan implementations on synthetic fixtures.
- The UI clearly distinguishes information, warnings, and critical issues.
- Existing Organize behavior and review gates continue to work.
- No new network dependency is introduced.
- `npm run build` passes and Rust tests pass.

## Out of scope / follow-ups

- Automatic missing-file repair and confidence-ranked relinking
- Metadata/artwork enrichment
- Watch folders or scheduled scans
- Persistent scan history
- GitHub Releases auto-updater
