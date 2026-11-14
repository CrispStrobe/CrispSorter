# crispcat

UI-independent Cathy/Catfish CAF reader/writer, metadata scanner and duplicate
utilities. The crate is MIT licensed; see LICENSE. LanceDB materialization is
optional and disabled by default. The Rust library can be consumed without
Tauri or a Python runtime; `crispcat-cli` is the small command-line companion.

CAF read supports versions 1–8 and write supports v6/v8. Modern v8 round trips
preserve exact zero bytes, explicit empty directories and directory timestamps.
Legacy zero-size provenance is exposed in `FileIndex.unknown_size_paths`.
`read_file_cancellable` / `write_file_cancellable` accept a cooperative stop
predicate; cancelled writes retain existing output. This metadata model is
materialized in memory. Native catalogue stores can consume the format adapter
without adopting an in-memory catalogue as their working database.

CrispCloud consumes a pinned, reviewed format-only slice of this library behind
its Rust/SQLite API. CAF exports are portable snapshots; they do not replace the
working database or carry its scan-completeness diagnostics. The upstream
Unicode extension uses heuristic UTF-8 detection; classic Cathy/Python readers
need Latin-1 compatibility exports.

The CAF codec and metadata types now live in the independently buildable `../crispcat-format` crate. This crate re-exports `crispcat::caf` and `crispcat::index`, preserving existing imports and type identity. Scanning, hashing and optional database materialization remain here.
