// SPDX-License-Identifier: GPL-3.0-only
// Bridge to the Tauri backend. In a plain browser (vite dev without the
// native shell) it degrades to a canned mock so the UI can be exercised
// visually without touching any real files.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type EventCallback, type UnlistenFn } from "@tauri-apps/api/event";

const inTauri = "__TAURI_INTERNALS__" in window;

// ---- mock data -------------------------------------------------------------- //

const mockRows = [
  { old: "Insomniak, Drymk - 1312 (Drymk Remix).wav", new: "Drymk, Insomniak - 1312 (Drymk Remix).wav", origin: "filename" },
  { old: "01 Some Artist - Some Title.mp3", new: "Some Artist - Some Title.mp3", origin: "filename" },
  { old: "Angerfist - And Jesus Wept.wav", new: "Angerfist - And Jesus Wept.wav", origin: "filename" },
];

async function mockInvoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  await new Promise((r) => setTimeout(r, 350));
  switch (cmd) {
    case "get_settings":
      return {
        last_folder: "C:\\Users\\You\\Music\\Track", master_db: null, backup_dir: null,
        duplicates_dir: null, alphabetical_artists: true, prefer_tags: true,
        set_title: true, refresh_artist: true, max_threads: 0,
      };
    case "save_settings": return null;
    case "pick_folder": return "C:\\Users\\You\\Music\\Track";
    case "reveal_path": return null;
    case "scan_plan":
      return {
        rows: mockRows, total: 1172, to_rename: 2, already_correct: 1169,
        manual: ["Mastering msbtwu.wav"], used_tags: true,
      };
    case "apply_renames":
      return { renamed: 2, skipped: 0, rollback_path: "…\\rename_rollback.csv", plan_path: "…\\rename_plan.csv" };
    case "tag_folder": {
      for (let i = 1; i <= 10; i++) {
        await new Promise((r) => setTimeout(r, 120));
        mockEmit("tag-progress", { done: i * 117, total: 1172, file: `file ${i * 117}.wav` });
      }
      return { tagged: 1170, skipped: 2, errors: [] };
    }
    case "rekordbox_status":
      return { running: mockRekordboxRunning, db_path: "D:\\PIONEER\\Master\\master.db" };
    case "rekordbox_plan":
      return (args?.mapping as [string, string][]).map(([o, n]) => ({
        content_id: "1", old_name: o, new_name: n,
        new_path: `D:/Music/Track/${n}`,
        title: n.split(" - ").slice(1).join(" - ").replace(/\.\w+$/, ""),
        artist: n.split(" - ")[0],
      }));
    case "rekordbox_apply":
      mockRekordboxRunning = false;
      return { changed: (args?.plan as unknown[]).length, backup_path: "…\\rekordbox_backups\\master.db.20260712.bak" };
    case "dedup_scan":
      return {
        groups: [{
          kind: "same-song",
          keeper: { path: "C:\\Track\\Artist - Song.wav", artist: "Artist", title: "Song", dur: null, size: 52_400_000 },
          extras: [{ path: "C:\\Track\\Artist - Song.mp3", artist: "Artist", title: "Song", dur: null, size: 9_800_000 }],
        }],
        report_path: "…\\duplicates.csv", extras: 1, scanned: 1172,
      };
    case "dedup_move":
      return { moved: (args?.extras as string[]).length, dest: "C:\\Track\\_duplicates" };
    case "rekordbox_dedup_scan":
      return {
        entries: 1174, extras: 2, report_path: "…\\rekordbox_duplicates.csv",
        groups: [
          {
            kind: "same-file",
            keeper: { id: "1", path: "D:/Music/Track/Alpha - One.wav", ext: ".wav", title: "One",
              created: "2026-01-01", cue_ids: ["a", "b"], playlist_rows: [["s1", "p1"]], plays: 3, rating: 0, comment: "" },
            extras: [{ id: "2", path: "d:\\music\\track\\alpha - one.wav", ext: ".wav", title: "One",
              created: "2026-02-01", cue_ids: [], playlist_rows: [], plays: 0, rating: 0, comment: "" }],
          },
          {
            kind: "same-song",
            keeper: { id: "3", path: "D:/Music/Track/Beta - Two.wav", ext: ".wav", title: "Two",
              created: "2026-01-01", cue_ids: [], playlist_rows: [["s2", "p2"]], plays: 0, rating: 0, comment: "" },
            extras: [{ id: "4", path: "D:/Music/Track/Beta - Two.mp3", ext: ".mp3", title: "Two",
              created: "2026-01-01", cue_ids: ["c"], playlist_rows: [], plays: 0, rating: 5, comment: "" }],
          },
        ],
      };
    case "rekordbox_dedup_apply": {
      const groups = (args?.groups ?? []) as { extras: unknown[] }[];
      return {
        removed: groups.reduce((n, g) => n + g.extras.length, 0),
        backup_path: "…\\rekordbox_backups\\master.db.20260713.bak",
      };
    }
    default:
      throw new Error(`mock: unknown command ${cmd}`);
  }
}

let mockRekordboxRunning = true; // flips false after first apply / re-check
const mockListeners = new Map<string, ((e: { payload: unknown }) => void)[]>();
function mockEmit(event: string, payload: unknown) {
  (mockListeners.get(event) ?? []).forEach((cb) => cb({ payload }));
}

// second re-check in the mock reports Rekordbox closed, to exercise the banner
setTimeout(() => { mockRekordboxRunning = false; }, 8000);

// ---- exports ----------------------------------------------------------------- //

export const invoke: <T>(cmd: string, args?: Record<string, unknown>) => Promise<T> = inTauri
  ? (tauriInvoke as never)
  : (mockInvoke as never);

export const listen: <T>(event: string, cb: EventCallback<T>) => Promise<UnlistenFn> = inTauri
  ? (tauriListen as never)
  : (async (event: string, cb: (e: { payload: unknown }) => void) => {
      const arr = mockListeners.get(event) ?? [];
      arr.push(cb);
      mockListeners.set(event, arr);
      return () => {
        const a = mockListeners.get(event) ?? [];
        a.splice(a.indexOf(cb), 1);
      };
    }) as never;

export const isMock = !inTauri;
