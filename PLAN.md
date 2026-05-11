# CrispSorter — Development Plan

> **Full specs for completed phases** → [HISTORY.md](HISTORY.md)
> **Technical patterns / pitfalls** → [LEARNINGS.md](LEARNINGS.md)
> **In-flight integration designs** → [docs/](docs/)

---

## Capabilities (shipped)

- LanceDB + Tantivy hybrid search, RRF fusion, sparse BGE-M3/SPLADE channel
- ONNX/CoreML + CrispEmbed GGUF backends, 36-model registry
- Batch AI sort (Stapel): extraction → LLM metadata → sort-path → move/copy/script
- P6 Catalog: `.caf` I/O, parallel scanner, duplicate engine, Übersicht columnar browse
- P7 Desktop search parity: folder tree, million-row pagination, preview pane, bg ingest
- P8 CLI: `version / doctor / catalog / index / batch / chat / completion / manpage`
- P9 Übersicht scale: DB-side ORDER BY (lance::Scanner), scalar indexes, volume filter
- P10 Robust ingest: TaskFailureReason, 300 s timeout, L2 fallback, DRM detection, skip-failed CLI
- P11 Remote server: `crisp-index-server` (Axum + LanceDB + Tantivy), durable job queue, server-side embedding
- P11 Cloud drives: `LocalDrive` + `InternxtDrive` + `FilenDrive` + `WebDavDrive` (live-verified against both Filen + Internxt local WebDAV servers); registry with create/edit/delete UI; `crisp+drive://` URIs; manifest-only L1 ingest + on-demand L3 promote
- P11 SyncManager: pull-apply loop closed (writes pulled rows as L1 metadata in local LanceDB)
- P12 cloud-backup: L1 manifest import (`source_files` → LanceDB), L3 via `retrieve.py`, reverse lookup, VPS-trigger indexing
- P15 Batch pre-processing: content-dedup (SHA-256), book-chapter grouping (ISBN-13)
- OCR: Tier 1 Tesseract, Tier 2 ocrs, Tier 3 PaddleOCR (`--features paddle-ocr`)
- `.cidx` offline archives: LanceDB + Tantivy FTS export/mount, Archiv tab in Übersicht, background-promote per row
- `crisp+cb-archive://` URI scheme for cloud-backup archived files
- `crisp+drive://` URI scheme for any registered CloudDrive (Local / Filen / Internxt / WebDAV)
- macOS arm64 packaging: `scripts/bundle_macos_native_libs.sh` co-bundles `libcrispasr.dylib` + `libcrispembed.dylib` + ggml backends + homebrew transitives into `.app/Contents/Frameworks/` with rewritten LC_RPATH entries

For per-feature deep-dives, see [HISTORY.md → "Phase ship index"](HISTORY.md).

---

## In Progress

**P13 Bilder vertical** — Tier 1 complete (slices A1–A4) + Tier 2
foundation landed (slice B1: protocol crate + keychain + auth).
Remaining: B2–B5 (semantic search, faces, health monitor, cross-link).
See P13 section below for the slice-by-slice status; details and
"spec vs reality" findings in [docs/P13_Bilder_integration.md](docs/P13_Bilder_integration.md).

**Test coverage:** 311 unit tests pass in `tauri-app` (+2 `#[ignore]`'d
WebDAV-live integration tests gated by
`WEBDAV_TEST_URL`/`USER`/`PASS`), 20 in `crispcat`, 16 in
`crisplens-protocol`, 5 in `crisp-index-protocol` = **352 passing**.
Run with `cargo test --workspace --lib`.

---

## Open TODOs

Only `[ ]` items live here.  Shipped items are in HISTORY.md.

### P3.5 — CrispEmbed / CrispASR bundling

- [x] Phase 1 — macOS arm64 (see HISTORY.md)
- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session)
      RPATH / DLL colocation; each platform needs 1-2 release iterations.
      Opening prompt: [docs/session-prompt-crispembed-ci-matrix.md](docs/session-prompt-crispembed-ci-matrix.md).
- [ ] **Phase 3 — mobile** (deferred)

### P5 — Future / planned

- [ ] **Auto-process toggle on watch detection** — risky, needs UX
      design pass before any code
- [ ] **PWA demo via File System Access API** — speculative

### P7.8 — OCR Tier 3 polish + Tier 4

- [ ] **SLANet table extraction** on top of Tier 3 PaddleOCR — adds
      structured table output for invoices / bank statements / grids.
      The `usls` crate already hosts a SLANet model.  ~3-5 h.
- [ ] **Tier 4 — VLM OCR** (~1 wk) — `deepseek-ocr.rs`-style via
      Candle (not ort).  DeepSeek-OCR / PaddleOCR-VL, Q4_K–Q8_0
      quantisation, 4.7-9 GB models, macOS Metal target.

### P8.2 — CLI polish remaining

- [ ] **`cargo install crispsorter`** for the Tauri-app binary — needs
      binstall recipe + signing (macOS Developer ID, Windows
      Authenticode).  `cargo install --path crates/crispcat-cli` already
      ships.  ~2-4 h once a signing identity is in hand.
- [ ] **CLI `transcribe`** — deferred, needs an ASR/TTS headless
      bootstrap path (would lean on CrispASR's CLI binary).
- [ ] **CLI `tts`** — same, needs CrispTTS or a sibling backend.

### P13 — Bilder vertical (Photos / images)

- [x] **Tier 1 — local-only Bilder tab** (`A1–A4`, ~25 h, shipped)
      Image-row filtered view (`Übersicht → Bilder`), lazy-loaded
      thumbnails (PNG via `image` crate), EXIF preview pane
      (`kamadak-exif` with permissive `continue_on_error` for
      piexif-shaped IFD chains), SHA-256 dup view, perceptual-hash
      near-dup view (`image_hasher`'s `HashAlg::Gradient` at 8×8 —
      see the slice-doc deviation note for why DCT-pHash didn't fly
      at 64-bit).  Zero external server deps.  Full CLI parity:
      `crispsorter images {extensions, count, list, thumbnail, exif,
      duplicates, near-duplicates}`.
- [x] **B1 — Tier 2 foundation** (`crisplens-protocol` crate +
      Keychain + Settings UI, ~6 h, shipped)
      New workspace member `crates/crisplens-protocol/` modelling
      both v2 (FastAPI) and v4 (Express) wire shapes, `keyring`-backed
      session-cookie storage (per-URL, never written to JSON),
      Tauri commands + CLI parity for settings + login/logout +
      session-status.  Live-verified end-to-end against
      `https://<crisplens-host>` (cookie lands in macOS Keychain;
      settings JSON file confirmed cookie-free).
- [ ] **Tier 2 — remaining slices** (`B2–B5`, ~21 h on paper, see
      [docs/P13_Bilder_integration.md](docs/P13_Bilder_integration.md)
      for the live-server-aware feasibility notes)
      B2 remote search (CrispLens has only person-name/full-text
      search, not semantic — reduce scope or wait for upstream),
      B3 Faces subtab (endpoints `/api/people` + `/api/images/{id}/faces`
      verified live), B4 health monitor + degradation banner
      (`/api/health` verified live), B5 open-in-CrispLens deep-link +
      watchfolder cross-reference (`/api/watchfolders` verified live).

Full design + slice breakdown + risk register + spec-vs-reality
notes: [**docs/P13_Bilder_integration.md**](docs/P13_Bilder_integration.md).

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
