---
name: release-version
description: Release a software version end to end: validate the repository, update versions, write release notes, create Git tags, publish GitHub releases, verify artifacts, and handle failures. Use for requests to release, cut, tag, publish, or prepare a new version, especially this Tauri app using npm, Rust, GitHub Actions, and Release Please.
---

# Release a Version

1. Inspect first. Run `git status --short --branch`, `git remote -v`, `git log -10 --oneline --decorate`, and inspect `app/package.json`, `app/src-tauri/tauri.conf.json`, release workflows, and the previous tag. Identify the target SemVer version and stable/prerelease status. Never discard or hide unrelated user changes.

2. Validate from `app/` using the CI commands: `npm ci`, `npm run format:check`, `npm run lint`, `npm run build`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features`. Stop on failures and report the exact command.

3. Write user-facing notes from commits since the previous tag. Group changes under Added, Changed, Fixed, and Security; omit empty sections; include compatibility, migrations, known limitations, and issue/PR links when known. Never invent claims. Prefer Release Please when configured; do not create a competing changelog.

4. For a manual release, update `app/package.json`, `app/package-lock.json`, `app/src-tauri/tauri.conf.json`, `app/Cargo.toml`, and `app/src-tauri/Cargo.toml` (and `app/Cargo.lock` when needed). Refresh lockfiles with package tooling, rerun checks, inspect `git diff --check`, then use `chore(release): vX.Y.Z`, an annotated `vX.Y.Z` tag, and only intentional staged files.

5. Before the first remote side effect, summarize version, commit, notes, checks, and files and ask for confirmation unless the user explicitly authorized the release. Never move an existing tag or force-push. Push commit and tag together with `git push origin <branch>` and `git push origin vX.Y.Z`.

6. Publish through the configured path. For manual GitHub publishing, verify `gh auth status`, then use `gh release create vX.Y.Z --title "Track Organizer vX.Y.Z" --notes-file <file>`. Verify the release URL, tag, workflow, draft/prerelease state, Windows installer/updater artifacts, and checksums. Report links to commit, tag, workflow, and release.

7. On failure, classify it as validation, versioning, tag, CI, packaging, or publishing. Fix before publishing. Delete an unused remote tag only with explicit approval and exact-target verification. For a bad published artifact, prefer a corrected patch release and never rewrite a consumed tag.

Read `references/project-release-reference.md` for this repository’s release topology. Finish with version, tag, validation, notes, URL, artifacts, and unresolved risks.
