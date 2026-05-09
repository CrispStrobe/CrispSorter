# CrispSorter — Development Plan

> **Full specs for completed phases** → [HISTORY.md](HISTORY.md)
> **Technical patterns / pitfalls** → [LEARNINGS.md](LEARNINGS.md)

---

## Capabilities (shipped)

- LanceDB + Tantivy hybrid search, RRF fusion, sparse BGE-M3/SPLADE channel
- ONNX/CoreML + CrispEmbed GGUF backends, 36-model registry
- Batch AI sort (Stapel): extraction → LLM metadata → sort-path → move/copy/script
- P6 Catalog: .caf I/O, parallel scanner, duplicate engine, Übersicht columnar browse
- P7 Desktop search parity: folder tree, million-row pagination, preview pane, bg ingest
- P8 CLI: `version / doctor / catalog / index / batch / completion / manpage`
- P9 Übersicht scale: DB-side ORDER BY (lance::Scanner), scalar indexes, volume filter
- P10 Robust ingest: TaskFailureReason, 300 s timeout, L2 fallback, DRM detection
- P11 Remote server: `crisp-index-server` (Axum + LanceDB + Tantivy), durable job queue, server-side embedding
- P12 cloud-backup: L1 manifest import (`source_files` → LanceDB), L3 via `retrieve.py`
- P15 Batch pre-processing: content-dedup (SHA-256), book-chapter grouping (ISBN-13)
- OCR: Tier 1 Tesseract, Tier 2 ocrs, Tier 3 PaddleOCR (`--features paddle-ocr`)
- `.cidx` offline archives: LanceDB + Tantivy FTS export/mount, Archiv tab in Übersicht
- `crisp+cb-archive://` URI scheme for cloud-backup archived files

---

## In Progress

*(nothing actively in-flight — see Open TODOs below)*

---

## Open TODOs

### P3.5 — CrispEmbed / CrispASR bundling

- [x] Phase 1 — macOS arm64 (shipped)
- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session)
  RPATH / DLL colocation; each platform needs 1-2 release iterations.
- [ ] **Phase 3 — mobile** (deferred)

### P5 — Future / planned

- Auto-process toggle on watch detection (risky, needs UX design)
- PWA demo via File System Access API

### P6 — Catalog

- [ ] **Phase 5 — extract `crispcat` workspace crate** (optional/deferred)
  Move `src-tauri/src/catalog/` to `crates/crispcat/` for a thin standalone CLI.

### P7.7 — Mountable archive index

- [x] LanceDB export (`export_cidx`) + Tantivy FTS companion (`--include-fts`)
- [x] Mount in Übersicht "Archiv" tab, FTS companion auto-loaded
- [ ] **Background-ingest on `.cidx` import** — when browsing an archive, trigger
  L3 promotion for selected rows (spawns bg_ingest worker with the cidx path).

### P7.8 — OCR Tiers 3 + 4

- [x] Tier 3 — PaddleOCR via `usls` (`--features paddle-ocr`). DB detection + SVTR recognition.
  Remaining: per-document CJK/Latin model selection; SLANet table extraction.
- [ ] **Tier 4 — VLM OCR** (~1 wk) — `deepseek-ocr.rs`-style via Candle (not ort).
  DeepSeek-OCR / PaddleOCR-VL, Q4_K–Q8_0. 4.7-9 GB models. macOS Metal ✅.

### P8.2 — CLI (continuation)

- [x] `catalog` / `index stats|list|search|delete|export-cidx|inspect-cidx|list-failed|retry-failed|ingest-cb-manifest` / `batch add|list|apply` / `completion` / `manpage`
- [ ] **`index init`** — download embedder model from CLI (tokio + hf-hub)
- [ ] **`index ingest <path>`** — full extraction+embedding pipeline headless
- [ ] **`batch process`** — headless LLM extraction pass (needs llm_client without GUI)
- [ ] **`chat`** — `query "<prompt>"` / `transcribe` / `tts` headless
- [ ] **Polish** — `cargo install crispsorter` story (needs crispcat extraction first)

### P10 — Remaining

- [x] **DRM help-popover** — clicking `fail-badge.fail-drm` opens an inline popover
  explaining the encryption, with a close button. No third-party tool recommendations.
- [ ] **CLI `--skip-failed`** — `bg_ingest start --skip-failed` flag honours the
  skip-on-fail rules from `extraction_failure_reason_for_uri`.

### P11 — Remote server (remaining)

- [x] **Server queue blob fix** — shipped: `embeddings_blob BLOB` + `embed_dims` columns;
  `payload_json` stores compact batch with empty vectors; blob is repacked on claim.
- [ ] **IVF-PQ at 100M+ vectors** — `sample_rate` knob in LanceDB IVF-PQ build.
- [ ] **IVF-PQ at 100M+ vectors** — `sample_rate` knob in LanceDB IVF-PQ build.
- [ ] **Runtime modes** — `Standalone | Server | Hybrid` enum replacing `BackendType`.
- [ ] **Cloud drives** — `trait CloudDrive` + SMB/SFTP/Filen/Internxt impls.
- [ ] **SyncManager** — local ↔ remote sync outbox, pull delta, reconnect detection.

### P12 — cloud-backup (remaining)

- [x] L1 manifest import via `index_ingest_cb_manifest`
- [x] L3 promotion via `retrieve.py` (`index_promote_cb_archive` + CloudDownload button)
- [x] **Reverse lookup UI** — `index_lookup_cb_file` Tauri command queries
  `source_files`+`archives`; preview pane shows Lokal/VPS availability when
  a `crisp+cb-archive://` row is opened. Manifest DB path persisted as
  `cbManifestDbPath` setting on first import.
- [ ] **VPS-trigger indexing** — cloud-backup `vps_worker.py` hook posts to
  `crisp-index-server` after decrypting each archive. ~100 lines of Python.

### P13 — Image-vertical convergence with CrispLens (future)

CLIP image embedder (ONNX via ort, ~150 MB), face recognition (SCRFD + ArcFace),
Images tab in Übersicht, face clustering. Requires P11 server + SyncManager stable.
See [HISTORY.md](HISTORY.md) for detailed spec.

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
