//! Drive cataloging + duplicate finding + offline file search.
//!
//! Phase 1 of PLAN P6: a Rust port of Catfish's core indexing + .caf
//! format I/O, designed to slot into CrispSorter alongside the existing
//! batch / LanceDB / FTS pipelines.
//!
//! The .caf format is byte-compatible with the classic Cathy tool and
//! all subsequent Catfish versions (1-8), so a CrispSorter-produced
//! catalog round-trips through any other Cathy/Catfish reader and vice
//! versa.
//!
//! Architecture:
//! * [`caf`] — pure binary reader/writer; no fs scanning, no hashing.
//! * [`index`] — in-memory `FileIndex` (size-bucketed for O(1) dedup).
//! * [`scan`] — rayon-parallel directory walker (jwalk-backed).
//!
//! Tauri commands live in `lib.rs` and just thread these helpers
//! through to the frontend.

pub mod caf;
pub mod dedup;
pub mod index;
pub mod lance;
pub mod scan;
