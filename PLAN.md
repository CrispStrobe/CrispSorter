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

**P13 Bilder vertical** — both tiers complete (A1–A4 + B1–B5).
Open follow-ups: image-overlay face boxes (needs sha256 cross-
reference at the CrispLens list endpoint), true semantic search
(needs CrispLens upstream to add an embedding-based route).

**Test coverage:** 311 unit tests pass in `tauri-app` (+2 `#[ignore]`'d
WebDAV-live integration tests gated by
`WEBDAV_TEST_URL`/`USER`/`PASS`), 20 in `crispcat`, 29 in
`crisplens-protocol`, 5 in `crisp-index-protocol` = **365 passing**.
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
- [x] **Tier 2 — complete** (`B1–B5`, ~21 h spec, shipped)
      All five Tier-2 slices on `main`:
      - **B1** (`0aa3a51`) — `crisplens-protocol` crate, Keychain
        session storage, Settings UI, Tauri + CLI parity.
      - **B4** (`250f137`) — `/api/health` + `/api/auth/me` polling
        with banner state machine (offline / session-expired /
        warming-up / ok).  Plus `enable-crispembed.sh` cargo
        target-dir fix.
      - **B5** (`8a4a2e0`) — Open-in-CrispLens deep-link from the
        Bilder preview pane; watchfolder cross-reference via
        `/api/watchfolders` with prefix-match hint when the
        previewed image lives under a CrispLens-watched folder.
      - **B3** (`01e6203`) — People view (Faces subtab equivalent)
        listing person clusters from `/api/people`; per-image
        faces endpoint `/api/images/{id}/faces` plumbed end-to-end
        with the live-verified `Face { bbox: Bbox }` nested-object
        shape.  Image-overlay face boxes deferred (need sha256
        cross-reference at the list endpoint).
      - **B2 reduced** (`814efe8`) — Remote text search via
        `/api/search` (filename / person-name substring — the live
        API doesn't expose semantic search; spec's "semantic search
        bar" wording is aspirational and tracked as a future
        CrispLens-upstream item).  Inline UI search box visible
        only when Tier 2 is authenticated.

      All five live-verified end-to-end against
      `https://<crisplens-host>`.  29/29 `crisplens-protocol`
      tests + 18/18 `images::crisplens` tests + 64/64 `images::*`
      tests pin both v2 and v4 wire shapes captured from live
      payloads.

Full design + slice breakdown + risk register + spec-vs-reality
notes: [**docs/P13_Bilder_integration.md**](docs/P13_Bilder_integration.md).

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
