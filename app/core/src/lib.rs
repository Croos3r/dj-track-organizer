// SPDX-License-Identifier: GPL-3.0-only
//! organizer-core — the organizing logic of the DJ Track Organizer app.
//!
//! Rust port of the four Python skills in this repository, kept in behavioral
//! parity with them via fixtures generated from the Python implementations
//! (see `tools/gen_fixtures.py` and `tests/`).

pub mod csvio;
pub mod dedup;
pub mod health;
pub mod normalize;
pub(crate) mod parallel;
pub mod retry;
pub mod tagging;

#[cfg(feature = "rekordbox")]
pub mod rekordbox;
