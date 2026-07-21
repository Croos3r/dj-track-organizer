# Project Release Reference

This is a Tauri v2 app under `app/`: npm/TypeScript/Vite/Prettier frontend and a Rust workspace with `core` and `src-tauri`.

- CI: `.github/workflows/ci.yml`
- Release Please: `.github/workflows/release-please.yml`, `release-please-config.json`, `.release-please-manifest.json`
- Windows packaging: `.github/workflows/release.yml` with the configured NSIS target
- Tags: `vMAJOR.MINOR.PATCH`

Normal automated path: merge conventional commits into `main`, review and merge the Release Please PR, let it create the version tag, then review the tag-triggered Tauri Windows workflow and GitHub release artifacts before publishing.

Trust live repository files over this reference if they differ, and report discrepancies.
