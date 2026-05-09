//! crispcat — Cathy/Catfish .caf catalog library.
//!
//! Extracted from `src-tauri/src/catalog/` (PLAN P6 Phase 5) so a thin
//! standalone CLI (`crispcat-cli`) and other consumers can use it without
//! pulling in the full Tauri / LanceDB / Tantivy stack.
//!
//! Modules:
//! * [`caf`]   — `.caf` binary read/write (Cathy v6, v7, v8 formats)
//! * [`index`] — `FileIndex` + `FileEntry` types
//! * [`scan`]  — parallel filesystem scanner with optional hashing
//! * [`dedup`] — duplicate detection across folders / `.caf` files
//! * [`lance`] — *(feature `lance`)* LanceDB-backed materialisation

pub mod caf;
pub mod dedup;
pub mod index;
pub mod scan;

#[cfg(feature = "lance")]
pub mod lance;
