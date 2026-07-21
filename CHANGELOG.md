# Changelog

## [0.1.0](https://github.com/Croos3r/dj-track-organizer/compare/v0.0.1...v0.1.0) (2026-07-21)


### Features

* add health dashboard, CI/CD, and release skill ([b75026b](https://github.com/Croos3r/dj-track-organizer/commit/b75026b7c7285135985b47f7abaec4f5f18c749a))


### Bug Fixes

* resolve strict Clippy diagnostics ([bd64f73](https://github.com/Croos3r/dj-track-organizer/commit/bd64f73f18c6ce9134e1fbdbe11d589865b1e048))
* satisfy strict workspace Clippy checks ([aa84798](https://github.com/Croos3r/dj-track-organizer/commit/aa847984eb43d6c2c1373e5f77fd1ed34e57a789))

## 0.0.1 - 2026-07-22

Initial public release of Track Organizer.

### Highlights

- Native Windows desktop workflow for filename normalization, metadata tagging, Rekordbox relinking, file deduplication, and Rekordbox collection deduplication.
- Review gates before destructive operations, two-phase renames, rollback CSV generation, Rekordbox database backups, and refusal to write while Rekordbox is open.
- Library health dashboard with bounded missing-file samples and actionable scoring.
- Parallel file processing, transient Windows-lock retries, and fast duplicate detection using prefix-gated BLAKE3 hashing.

### Quality and delivery

- Behavioral parity tests against the original Python tools, including byte-level CSV and tag fixtures.
- Cross-platform formatting, linting, strict Rust Clippy, frontend production builds, and full workspace tests in CI.
- Automated draft Windows installer builds for version tags, ready for verification before publication.
