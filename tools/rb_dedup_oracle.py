#!/usr/bin/env python3
"""Oracle side of the collection-dedup parity test.

Opens the given (synthetic, throwaway) master.db copy EXPLICITLY by path via
pyrekordbox, runs the real --rekordbox-db functions from the dedup skill, and
prints a JSON summary. With --apply it also merges+deletes and commits, so the
Rust test can diff the resulting database state against its own port.

Never call this with a real master.db: it exists purely so the Python skill
and the Rust app can be compared on identical scratch databases.

Usage: python tools/rb_dedup_oracle.py <path-to-master.db-copy> [--apply]
"""
import importlib.util
import json
import os
import sys


def main():
    db_path = sys.argv[1]
    apply_changes = "--apply" in sys.argv[2:]
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    script = os.path.join(repo, "skills", "dedup-tracks", "scripts", "dedup_tracks.py")
    spec = importlib.util.spec_from_file_location("dedup_tracks", script)
    dt = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(dt)

    db = dt.rb_open_database(db_path)
    entries = dt.rb_load_entries(db, None)
    groups = [(kind, *dt.rb_keeper(g)) for kind, g in dt.rb_find_groups(entries)]
    summary = [
        {
            "kind": kind,
            "keeper": keeper["path"],
            "extras": sorted(x["path"] for x in extras),
        }
        for kind, keeper, extras in groups
    ]
    removed = 0
    if apply_changes:
        for _, keeper, extras in groups:
            for extra in extras:
                dt.rb_merge_into_keeper(db, keeper, extra)
                removed += 1
        # commit on the raw session: pyrekordbox's commit() wrapper refuses
        # while ANY Rekordbox instance runs, but this is a throwaway synthetic
        # db that no Rekordbox has open (its USN bookkeeping is excluded from
        # the parity diff anyway).
        db.session.commit()
    print(json.dumps(
        {"entries": len(entries), "summary": summary, "removed": removed},
        ensure_ascii=False,
    ))


if __name__ == "__main__":
    main()
