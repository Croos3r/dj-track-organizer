# Changelog

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
