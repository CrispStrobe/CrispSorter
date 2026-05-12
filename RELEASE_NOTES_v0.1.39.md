# CrispSorter v0.1.39

First release on the multi-crate workspace.  Eight weeks since v0.1.38,
covering phases P7.7–P15 from `PLAN.md` and growing the Rust test
suite from ~195 to **365 passing** tests across the workspace.

## Highlights

- **Bilder (Photos) vertical (P13)** — new local-first image tab on
  the existing LanceDB index with lazy thumbnails, EXIF preview pane,
  SHA-256 and perceptual-hash duplicate views.  Tier 2 wires through
  to a remote CrispLens server (auth via macOS Keychain / DPAPI /
  Secret Service, health-state banner, People view with face
  endpoints, watchfolder cross-reference, open-in-CrispLens
  deep-links).  CLI: `crispsorter images …`.
- **Übersicht at million-file scale (P9)** — columnar multi-select
  browse with persisted column registry, folder breadcrumb tree,
  preview pane, DB-side `ORDER BY` via `lance::Scanner`, scalar
  indexes on `parent_dir` + `volume_id`, cross-platform drive
  identification for `.caf` catalogs.
- **Remote index server (P11)** — in-tree `crisp-index-server`
  workspace member (Axum + LanceDB + Tantivy), durable job queue,
  compact binary embedding storage, hybrid backend mode
  (`BackendType::Hybrid`), server-side embedding + search wired
  through the desktop UI.  Settings exposes embedder location.
- **Cloud drives (P11)** — first-class `LocalDrive`,
  `InternxtDrive` (patched `internxt-cli` bridge), `FilenDrive`
  (patched `filen-python` JSON bridge), and `WebDavDrive`.  Inline
  Übersicht create-drive form, edit/delete UI, `insecure_tls`
  toggle, `crisp+drive://` URI scheme, manifest-only L1 ingest +
  on-demand L3 promote.  `SyncManager` pull-apply loop closed.
  Live-verified end-to-end against Filen and Internxt WebDAV servers.
- **Cloud-backup integration (P12)** — L1 manifest import
  (`source_files` → LanceDB), L3 promotion via `retrieve.py`
  bridge, reverse-lookup including Internxt tier, VPS-trigger
  indexing, `crisp+cb-archive://` URI scheme, archived-file UX hooks.
- **Robust ingest at scale (P10)** — `TaskFailureReason` taxonomy,
  300 s extraction timeouts, L2 fallback, DRM detection with popover,
  `list-failed` / `retry-failed` Tauri commands + CLI subcommands.
- **OCR Tier 3 (P7.8)** — PaddleOCR DB+SVTR via `usls`, enabled with
  `--features paddle-ocr`, CJK model selection from Settings.  Tier
  ladder is now Tesseract → ocrs → PaddleOCR.
- **`.cidx` offline archives (P7.7)** — LanceDB + Tantivy FTS export
  and mount, Archiv tab in Übersicht, per-row background promote, CLI
  `archive export` / `mount`.  Pure-offline full-text search over
  archived catalogs.
- **Batch (Stapel) overhaul** — producer/consumer pipeline so
  extraction overlaps LLM analysis, N+M worker concurrency, live
  throughput chip, per-row M|T|L progress pips, richer ingest logs,
  author-gated auto-accept, simpler post-sort dialog, re-analyse
  button, content-dedup (SHA-256) and book-chapter grouping
  (ISBN-13).
- **CLI parity (P8.2)** — new subcommands `batch process`,
  `chat query`, `index init / ingest`, `images {extensions, count,
  list, thumbnail, exif, duplicates, near-duplicates}`,
  `manpage`, shell `completion`.  `crispcat` lifted to its own
  workspace crate with a standalone `crispcat` binary
  (`cargo install --path crates/crispcat-cli`).
- **i18n** — Catalog, Duplicates, embed-bench, script-format, and
  Bilder panels all driven through the i18n table in EN + DE.

## Capabilities (as of v0.1.39)

LanceDB + Tantivy hybrid search with RRF fusion, sparse BGE-M3 /
SPLADE channel, ONNX/CoreML + CrispEmbed GGUF backends (36-model
registry), `.caf` catalogs, `.cidx` offline archives, cloud drives
(Local / Internxt / Filen / WebDAV), remote `crisp-index-server` for
terabyte-scale indexes, P13 Bilder vertical with optional CrispLens
backend.

## Platform / packaging

- **macOS arm64** — `scripts/bundle_macos_native_libs.sh` ships
  `libcrispasr.dylib` + `libcrispembed.dylib` + ggml backends +
  homebrew transitives in `.app/Contents/Frameworks/` with rewritten
  LC_RPATH entries.  `.app.tar.gz` is repacked from the patched bundle
  so the Tauri updater serves the fix too.
- **Linux x86_64** — `.deb` patched in-place via
  `scripts/bundle_linux_native_libs.sh`; `crispasr-vulkan` and
  `crispembed-vulkan` features both enabled.
- **Windows x86_64** — ships without the GGUF embedder / on-device
  ASR backends this release.  Re-enabling them is queued behind a
  `SetDllDirectoryW("resources/bin")` startup hook (or a custom WiX
  fragment) so Tauri's `bundle.resources` layout resolves DLLs
  correctly; tracked as P3.5 Phase 2 in `PLAN.md`.

Pinned sibling versions for this build: `CrispEmbed v0.2.6`,
`CrispASR v0.5.7`, `llama.cpp b8340`.

## Test coverage

- `tauri-app` lib: 311 (+2 `#[ignore]`'d WebDAV-live integration
  tests gated by `WEBDAV_TEST_URL`/`USER`/`PASS`)
- `crispcat`: 20
- `crisplens-protocol`: 29
- `crisp-index-protocol`: 5
- **Total: 365 passing.**  Run with `cargo test --workspace --lib`.

## Notable bug fixes

- `fix(crispcat)`: tokio dev-dep so `cargo test --workspace` builds the
  lance-feature path.
- `fix(catalog)`: Hinzufügen now persists across restarts; svelte-check
  multi-class attribute warnings resolved.
- `fix(p9)`: L3 rows populate `fs_size` + `fs_mtime` in
  `metadata_json`; `indexed_at` sort restored.
- `fix(stapel)`: every step of Sortieren is surfaced, no false green
  checks, accessible Übersicht headers, working post-sort dialog.
- `fix(index)`: L1 / L2 paths no longer load the embedder; GGUF quant
  picker matches the variant being downloaded; real per-file download
  progress.
- `fix(build)`: bootstrap `protoc` on Windows via `paths.ps1`;
  PowerShell 5 / German-locale parse errors smoothed.
- `fix(release)`: macOS arm64 `.app.tar.gz` repack uses absolute path
  (v0.1.38 mode); Linux Patch / Re-upload steps tolerate missing
  per-arch target dirs.

## Documentation

- `docs/P13_Bilder_integration.md` — full slice breakdown +
  spec-vs-reality notes for the CrispLens integration.
- `PLAN.md` / `HISTORY.md` split per-phase logs from the living
  roadmap; archived plans live in `HISTORY.md`.
- `LEARNINGS.md` collects non-obvious patterns + pitfalls.
- README documents the optional shared cargo target-dir on an
  external volume.

## Known follow-ups (not blocking)

- **P3.5 Phase 2** — Linux/Windows GGUF embedder + on-device ASR
  re-enablement (RPATH / DLL colocation), tracked in `PLAN.md`.
- **P13 image-overlay face boxes** — partial in
  `feat(p13/followups)`; needs the upstream
  `GET /api/images/by-hash/{sha}` route in CrispLens for robust
  sha256 ↔ doc_id cross-reference.
- **P13 true semantic search** — one-line URL swap once CrispLens
  exposes `/api/search/semantic`.
- **P7.8 polish** — SLANet table extraction on top of Tier 3
  PaddleOCR; Tier 4 (VLM OCR via Candle) deferred.
