// SPDX-License-Identifier: GPL-3.0-only
// Track Organizer UI: one-click pipeline with review gates before every
// destructive step (renames, Rekordbox writes, duplicate moves).

import { invoke, listen } from "./bridge";

// ---- types mirrored from the Rust command layer --------------------------- //

interface PlanRow { old: string; new: string; origin: string }
interface ScanResult {
  rows: PlanRow[]; total: number; to_rename: number;
  already_correct: number; manual: string[]; used_tags: boolean;
}
interface RenameResult { renamed: number; skipped: number; rollback_path: string; plan_path: string }
interface TagResult { tagged: number; skipped: number; errors: [string, string][] }
interface RekordboxStatus { running: boolean; db_path: string | null }
interface RelinkItem {
  content_id: string; old_name: string; new_name: string;
  new_path: string; title: string; artist: string;
}
interface RelinkResult { changed: number; backup_path: string }
interface FileInfo { path: string; artist: string; title: string; dur: number | null; size: number }
interface DupGroup { kind: string; keeper: FileInfo; extras: FileInfo[] }
interface DedupScan { groups: DupGroup[]; report_path: string; extras: number; scanned: number }
interface MoveResult { moved: number; dest: string }
interface RbEntry {
  id: string; path: string; ext: string; title: string; created: string;
  cue_ids: string[]; playlist_rows: [string, string][];
  plays: number; rating: number; comment: string;
}
interface RbDupGroup { kind: string; keeper: RbEntry; extras: RbEntry[] }
interface RbDedupScan { groups: RbDupGroup[]; entries: number; extras: number; report_path: string }
interface RbDedupResult { removed: number; backup_path: string }
interface Settings {
  last_folder: string | null; master_db: string | null;
  backup_dir: string | null; duplicates_dir: string | null;
  alphabetical_artists: boolean; prefer_tags: boolean;
  set_title: boolean; refresh_artist: boolean;
  max_threads: number;
}

// ---- tiny dom helpers ------------------------------------------------------ //

const $ = <T extends HTMLElement>(sel: string) => document.querySelector(sel) as T;
const el = (tag: string, cls?: string, text?: string) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};
const esc = (s: string) => s; // textContent everywhere; no innerHTML with data

let settings: Settings;
let folder: string | null = null;

// ---- step chrome ----------------------------------------------------------- //

type StepId = "normalize" | "tag" | "rekordbox" | "dedup" | "rbdedup";
const stepEl = (id: StepId) => $(`#step-${id}`);

function setStep(id: StepId, state: "pending" | "running" | "done" | "skipped" | "error",
                 chip: string, detail = "") {
  const s = stepEl(id);
  s.classList.remove("running", "done", "skipped", "error");
  if (state !== "pending") s.classList.add(state);
  const chipEl = s.querySelector(".chip") as HTMLElement;
  chipEl.textContent = chip;
  chipEl.className = "chip " + ({ running: "run", done: "ok", error: "err", skipped: "", pending: "pending" }[state] ?? "");
  (s.querySelector(".step-detail") as HTMLElement).textContent = detail;
}

function resetSteps() {
  (["normalize", "tag", "rekordbox", "dedup", "rbdedup"] as StepId[]).forEach((id) =>
    setStep(id, "pending", "waiting"));
  $("#summary").classList.add("hidden");
  $("#summary").textContent = "";
}

// ---- modal ----------------------------------------------------------------- //

interface ModalChoice { ok: boolean; skip: boolean }

function showModal(opts: {
  title: string; sub: string; okLabel: string;
  body?: HTMLElement; banner?: HTMLElement; allowSkip?: boolean;
}): Promise<ModalChoice> {
  return new Promise((resolve) => {
    $("#modal-title").textContent = opts.title;
    $("#modal-sub").textContent = opts.sub;
    const body = $("#modal-body");
    body.textContent = "";
    if (opts.banner) body.parentElement!.insertBefore(opts.banner, body);
    if (opts.body) body.appendChild(opts.body);
    const ok = $("#modal-ok") as HTMLButtonElement;
    ok.textContent = opts.okLabel;
    const skip = $("#modal-skip") as HTMLButtonElement;
    skip.classList.toggle("hidden", opts.allowSkip === false);
    $("#modal").classList.remove("hidden");

    const done = (choice: ModalChoice) => {
      $("#modal").classList.add("hidden");
      opts.banner?.remove();
      ok.onclick = skip.onclick = ($("#modal-cancel") as HTMLButtonElement).onclick = null;
      resolve(choice);
    };
    ok.onclick = () => done({ ok: true, skip: false });
    skip.onclick = () => done({ ok: false, skip: true });
    ($("#modal-cancel") as HTMLButtonElement).onclick = () => done({ ok: false, skip: false });
  });
}

// ---- pipeline steps --------------------------------------------------------- //

async function stepNormalize(): Promise<{ mapping: [string, string][]; aborted: boolean }> {
  setStep("normalize", "running", "scanning…");
  const scan = await invoke<ScanResult>("scan_plan", { folder, settings });
  const manualNote = scan.manual.length ? `, ${scan.manual.length} need a manual name` : "";

  if (scan.to_rename === 0) {
    setStep("normalize", "done", "clean", `${scan.total} files already normalized${manualNote}`);
    return { mapping: [], aborted: false };
  }

  const table = el("table");
  const thead = el("thead");
  thead.appendChild(el("tr"));
  ["Current name", "", "New name"].forEach((h) => thead.firstChild!.appendChild(el("th", "", h)));
  table.appendChild(thead);
  const tbody = el("tbody");
  for (const r of scan.rows.filter((r) => r.new && r.new !== r.old)) {
    const tr = el("tr");
    const o = el("td", "mono old-name", esc(r.old));
    const a = el("td", "arrow", "→");
    const n = el("td", "mono", esc(r.new));
    tr.append(o, a, n);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);

  const choice = await showModal({
    title: `Rename ${scan.to_rename} file${scan.to_rename > 1 ? "s" : ""}?`,
    sub: `${scan.total} files scanned — ${scan.already_correct} already correct${manualNote}. ` +
         `A rollback CSV is written so this can be undone.`,
    okLabel: `Rename ${scan.to_rename}`,
    body: table,
  });
  if (!choice.ok && !choice.skip) return { mapping: [], aborted: true };
  if (choice.skip) {
    setStep("normalize", "skipped", "skipped", "no files were renamed");
    return { mapping: [], aborted: false };
  }

  setStep("normalize", "running", "renaming…");
  const res = await invoke<RenameResult>("apply_renames", { folder, rows: scan.rows });
  const skippedNote = res.skipped ? `, ${res.skipped} skipped (target existed)` : "";
  setStep("normalize", "done", `${res.renamed} renamed`,
    `${res.renamed} files renamed${skippedNote}${manualNote} — rollback: rename_rollback.csv`);
  const mapping: [string, string][] = scan.rows
    .filter((r) => r.new && r.new !== r.old)
    .map((r) => [r.old, r.new]);
  return { mapping, aborted: false };
}

async function stepTag(): Promise<boolean> {
  setStep("tag", "running", "tagging…");
  const prog = stepEl("tag").querySelector(".progress") as HTMLElement;
  prog.classList.remove("hidden");
  const bar = prog.querySelector(".bar") as HTMLElement;
  const txt = prog.querySelector(".progress-text") as HTMLElement;
  const unlisten = await listen<{ done: number; total: number; file: string }>(
    "tag-progress",
    (e: { payload: { done: number; total: number; file: string } }) => {
      const { done, total } = e.payload;
      bar.style.width = `${Math.round((done / Math.max(total, 1)) * 100)}%`;
      txt.textContent = `${done} / ${total}`;
    },
  );
  try {
    const res = await invoke<TagResult>("tag_folder", { folder, settings });
    const errNote = res.errors.length ? `, ${res.errors.length} errors` : "";
    setStep("tag", res.errors.length ? "error" : "done",
      `${res.tagged} tagged`,
      `${res.tagged} files tagged from their names${errNote}` +
      (res.errors.length ? ` — first: ${res.errors[0][0]}: ${res.errors[0][1]}` : ""));
    return true;
  } finally {
    unlisten();
    prog.classList.add("hidden");
  }
}

async function stepRekordbox(mapping: [string, string][]): Promise<boolean> {
  if (mapping.length === 0) {
    setStep("rekordbox", "skipped", "nothing to sync", "no files were renamed this run");
    return true;
  }
  setStep("rekordbox", "running", "planning…");
  const status = await invoke<RekordboxStatus>("rekordbox_status", { settings });
  if (!status.db_path) {
    setStep("rekordbox", "error", "no master.db",
      "master.db not found — set its path in Settings and run again");
    return true;
  }
  const folderName = folder!.split(/[\\/]/).pop() ?? null;
  const plan = await invoke<RelinkItem[]>("rekordbox_plan", {
    settings, mapping, folderFilter: folderName,
  });
  if (plan.length === 0) {
    setStep("rekordbox", "done", "nothing to relink", "no collection entries matched the renames");
    return true;
  }

  const table = el("table");
  const thead = el("thead");
  const hr = el("tr");
  ["Collection entry", "", "Relinked to"].forEach((h) => hr.appendChild(el("th", "", h)));
  thead.appendChild(hr);
  table.appendChild(thead);
  const tbody = el("tbody");
  for (const item of plan) {
    const tr = el("tr");
    tr.append(
      el("td", "mono old-name", esc(item.old_name)),
      el("td", "arrow", "→"),
      el("td", "mono", esc(item.new_name)),
    );
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);

  // block while Rekordbox runs; re-check on demand
  const banner = el("div", "banner");
  const bannerText = el("span", "", "");
  const recheck = el("button", "", "Re-check") as HTMLButtonElement;
  banner.append(bannerText, recheck);
  const okBtn = $("#modal-ok") as HTMLButtonElement;
  const setRunning = (running: boolean) => {
    banner.classList.toggle("hidden", !running);
    okBtn.disabled = running;
    bannerText.textContent =
      "Rekordbox is running — close it fully, then hit Re-check.";
  };
  recheck.onclick = async () => {
    const s = await invoke<RekordboxStatus>("rekordbox_status", { settings });
    setRunning(s.running);
  };

  const modalPromise = showModal({
    title: `Relink ${plan.length} track${plan.length > 1 ? "s" : ""} in Rekordbox?`,
    sub: `master.db: ${status.db_path} — a timestamped backup is made first; ` +
         `cues, beatgrids and playlists are preserved.`,
    okLabel: `Back up & sync`,
    body: table,
    banner,
  });
  setRunning(status.running);
  const choice = await modalPromise;
  okBtn.disabled = false;

  if (!choice.ok && !choice.skip) return false;
  if (choice.skip) {
    setStep("rekordbox", "skipped", "skipped", "collection not touched");
    return true;
  }

  setStep("rekordbox", "running", "syncing…");
  try {
    const res = await invoke<RelinkResult>("rekordbox_apply", { settings, plan });
    setStep("rekordbox", "done", `${res.changed} relinked`,
      `backup: ${res.backup_path}`);
  } catch (e) {
    setStep("rekordbox", "error", "failed", String(e));
  }
  return true;
}

async function stepDedup(): Promise<boolean> {
  setStep("dedup", "running", "scanning…");
  const scan = await invoke<DedupScan>("dedup_scan", { folder, settings });
  if (scan.groups.length === 0) {
    setStep("dedup", "done", "no duplicates", `${scan.scanned} files scanned, all unique`);
    return true;
  }

  const wrap = el("div");
  const boxes: { cb: HTMLInputElement; path: string }[] = [];
  scan.groups.forEach((g, i) => {
    const block = el("div", "group-block");
    block.appendChild(el("div", "group-title",
      `${g.kind === "exact" ? "identical files" : "same track"} — group ${i + 1}`));
    const keep = el("div", "group-row");
    keep.append(el("span", "keeper-star", "★"), el("span", "", esc(basename(g.keeper.path))),
      el("span", "size", fmtSize(g.keeper.size)));
    block.appendChild(keep);
    for (const x of g.extras) {
      const row = el("div", "group-row");
      const cb = el("input") as HTMLInputElement;
      cb.type = "checkbox";
      cb.checked = true;
      boxes.push({ cb, path: x.path });
      row.append(cb, el("span", "", esc(basename(x.path))), el("span", "size", fmtSize(x.size)));
      block.appendChild(row);
    }
    wrap.appendChild(block);
  });

  const choice = await showModal({
    title: `Move ${scan.extras} duplicate${scan.extras > 1 ? "s" : ""} aside?`,
    sub: `★ marks the copy that stays (best quality, then largest). Ticked files are moved — nothing is deleted.`,
    okLabel: "Move duplicates",
    body: wrap,
  });
  if (!choice.ok && !choice.skip) return false;
  if (choice.skip) {
    setStep("dedup", "skipped", "skipped", `report written: duplicates.csv`);
    return true;
  }

  const extras = boxes.filter((b) => b.cb.checked).map((b) => b.path);
  setStep("dedup", "running", "moving…");
  const res = await invoke<MoveResult>("dedup_move", { folder, settings, extras });
  setStep("dedup", "done", `${res.moved} moved`, `moved to ${res.dest}`);
  return true;
}

async function stepRbDedup(): Promise<boolean> {
  setStep("rbdedup", "running", "scanning…");
  let scan: RbDedupScan;
  try {
    scan = await invoke<RbDedupScan>("rekordbox_dedup_scan", { folder, settings });
  } catch (e) {
    setStep("rbdedup", "error", "failed", String(e));
    return true;
  }
  if (scan.groups.length === 0) {
    setStep("rbdedup", "done", "collection clean",
      `${scan.entries} collection entries checked, no duplicates`);
    return true;
  }

  const wrap = el("div");
  const info = (e: RbEntry) =>
    `${e.cue_ids.length} cues · ${e.playlist_rows.length} playlists` +
    (e.plays ? ` · ${e.plays} plays` : "") + (e.rating ? ` · ★${e.rating}` : "");
  for (const [i, g] of scan.groups.entries()) {
    const block = el("div", "group-block");
    block.appendChild(el("div", "group-title",
      `${g.kind === "same-file" ? "same file, several entries" : "same track, several entries"} — group ${i + 1}`));
    const keep = el("div", "group-row");
    keep.append(el("span", "keeper-star", "★"), el("span", "", basename(g.keeper.path)),
      el("span", "size", info(g.keeper)));
    block.appendChild(keep);
    for (const x of g.extras) {
      const row = el("div", "group-row");
      row.append(el("span", "old-name", "✕"), el("span", "", basename(x.path)),
        el("span", "size", info(x)));
      block.appendChild(row);
    }
    wrap.appendChild(block);
  }

  // same running-gate as the relink step
  const banner = el("div", "banner");
  const bannerText = el("span", "",
    "Rekordbox is running — close it fully, then hit Re-check.");
  const recheck = el("button", "", "Re-check") as HTMLButtonElement;
  banner.append(bannerText, recheck);
  const okBtn = $("#modal-ok") as HTMLButtonElement;
  const setRunning = (running: boolean) => {
    banner.classList.toggle("hidden", !running);
    okBtn.disabled = running;
  };
  recheck.onclick = async () => {
    const s = await invoke<RekordboxStatus>("rekordbox_status", { settings });
    setRunning(s.running);
  };

  const modalPromise = showModal({
    title: `Remove ${scan.extras} duplicate collection entr${scan.extras > 1 ? "ies" : "y"}?`,
    sub: `★ stays; playlist memberships move to it, and its cues are kept (or inherited if it has none). ` +
         `A timestamped master.db backup is made first. Files on disk are not touched.`,
    okLabel: "Back up & clean",
    body: wrap,
    banner,
  });
  invoke<RekordboxStatus>("rekordbox_status", { settings }).then((s) => setRunning(s.running));
  const choice = await modalPromise;
  okBtn.disabled = false;

  if (!choice.ok && !choice.skip) return false;
  if (choice.skip) {
    setStep("rbdedup", "skipped", "skipped", `report written: rekordbox_duplicates.csv`);
    return true;
  }

  setStep("rbdedup", "running", "cleaning…");
  try {
    const res = await invoke<RbDedupResult>("rekordbox_dedup_apply", {
      settings, groups: scan.groups,
    });
    setStep("rbdedup", "done", `${res.removed} removed`, `backup: ${res.backup_path}`);
  } catch (e) {
    setStep("rbdedup", "error", "failed", String(e));
  }
  return true;
}

// ---- pipeline --------------------------------------------------------------- //

let busy = false;

async function organize() {
  if (!folder || busy) return;
  busy = true;
  const btn = $("#organize-btn") as HTMLButtonElement;
  btn.disabled = true;
  btn.textContent = "Organizing…";
  resetSteps();
  try {
    const { mapping, aborted } = await stepNormalize();
    if (!aborted) {
      await stepTag();
      if (await stepRekordbox(mapping)) {
        if (await stepDedup()) {
          await stepRbDedup();
        }
      }
    }
    showSummary(aborted);
  } catch (e) {
    const sum = $("#summary");
    sum.classList.remove("hidden");
    sum.textContent = `Something went wrong: ${e}`;
  } finally {
    busy = false;
    btn.disabled = false;
    btn.textContent = "Organize";
  }
}

function showSummary(aborted: boolean) {
  const sum = $("#summary");
  sum.classList.remove("hidden");
  sum.textContent = "";
  sum.appendChild(el("h2", "", aborted ? "Stopped." : "All done."));
  const line = el("div");
  line.append("Library: ");
  const link = el("span", "path-link", folder!);
  link.onclick = () => invoke("reveal_path", { path: folder });
  line.appendChild(link);
  sum.appendChild(line);
  if (!aborted) {
    sum.appendChild(el("div", "",
      "Open Rekordbox whenever you like — relinked tracks keep their cues and grids."));
  }
}

// ---- misc ------------------------------------------------------------------- //

const basename = (p: string) => p.split(/[\\/]/).pop() ?? p;
const fmtSize = (n: number) =>
  n > 1 << 20 ? `${(n / (1 << 20)).toFixed(1)} MB` : `${Math.ceil(n / 1024)} KB`;

function bindSettingsDrawer() {
  const drawer = $("#drawer");
  $("#settings-btn").onclick = () => {
    ($("#set-db") as HTMLInputElement).value = settings.master_db ?? "";
    ($("#set-backup") as HTMLInputElement).value = settings.backup_dir ?? "";
    ($("#set-dupes") as HTMLInputElement).value = settings.duplicates_dir ?? "";
    ($("#set-alpha") as HTMLInputElement).checked = settings.alphabetical_artists;
    ($("#set-tags") as HTMLInputElement).checked = settings.prefer_tags;
    ($("#set-title") as HTMLInputElement).checked = settings.set_title;
    ($("#set-artist") as HTMLInputElement).checked = settings.refresh_artist;
    ($("#set-threads") as HTMLInputElement).value = String(settings.max_threads ?? 0);
    drawer.classList.remove("hidden");
  };
  $("#drawer-close").onclick = async () => {
    const v = (id: string) => ($(id) as HTMLInputElement).value.trim() || null;
    const c = (id: string) => ($(id) as HTMLInputElement).checked;
    settings = {
      ...settings,
      master_db: v("#set-db"),
      backup_dir: v("#set-backup"),
      duplicates_dir: v("#set-dupes"),
      alphabetical_artists: c("#set-alpha"),
      prefer_tags: c("#set-tags"),
      set_title: c("#set-title"),
      refresh_artist: c("#set-artist"),
      max_threads: Math.max(0, parseInt(($("#set-threads") as HTMLInputElement).value, 10) || 0),
    };
    await invoke("save_settings", { settings });
    drawer.classList.add("hidden");
  };
}

async function setFolder(path: string | null) {
  if (!path) return;
  folder = path;
  $("#folder-path").textContent = path;
  ($("#organize-btn") as HTMLButtonElement).disabled = false;
  settings = { ...settings, last_folder: path };
  await invoke("save_settings", { settings });
}

async function main() {
  settings = await invoke<Settings>("get_settings");
  bindSettingsDrawer();
  if (settings.last_folder) await setFolder(settings.last_folder);
  $("#browse-btn").onclick = async () => {
    const picked = await invoke<string | null>("pick_folder", { initial: folder });
    await setFolder(picked);
  };
  $("#organize-btn").onclick = organize;
  resetSteps();
}

main();
