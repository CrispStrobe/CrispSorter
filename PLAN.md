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
- [x] **Background-ingest on `.cidx` import** — Archiv tab: checkboxes on rows,
  selection bar with "Auf L3 hochstufen" button (calls `index_promote_cb_archive`
  per selected cb-archive row), "archiv" badge on L1 cb-archive rows.

### P7.8 — OCR Tiers 3 + 4

- [x] Tier 3 — PaddleOCR via `usls` (`--features paddle-ocr`). DB detection + SVTR recognition.
  CJK/Latin model selection: `OcrRecLang` enum (Auto/Latin/Cjk), path heuristic for Auto,
  Settings dropdown, bg_ingest `ocr_rec_lang` field + Tauri command.
  Remaining: SLANet table extraction.
- [ ] **Tier 4 — VLM OCR** (~1 wk) — `deepseek-ocr.rs`-style via Candle (not ort).
  DeepSeek-OCR / PaddleOCR-VL, Q4_K–Q8_0. 4.7-9 GB models. macOS Metal ✅.

### P8.2 — CLI (continuation)

- [x] `catalog` / `index stats|list|search|delete|export-cidx|inspect-cidx|list-failed|retry-failed|ingest-cb-manifest` / `batch add|list|apply` / `completion` / `manpage`
- [x] **`index init --model M --device D`** — downloads embedder model to data-dir/models/;
  supports bge-m3, multilingual-e5-*, bge-*-en-v1.5, nomic, minilm
- [x] **`index ingest <paths>... [--model M] [--device D]`** — full extraction+embedding
  pipeline headless; walks directories; SHA-256 + extract + embed + LanceDB+Tantivy write
- [ ] **`batch process`** — headless LLM extraction pass (needs llm_client without GUI)
- [ ] **`chat`** — `query "<prompt>"` / `transcribe` / `tts` headless
- [ ] **Polish** — `cargo install crispsorter` story (needs crispcat extraction first)

### P10 — Remaining

- [x] **DRM help-popover** — clicking `fail-badge.fail-drm` opens an inline popover
  explaining the encryption, with a close button. No third-party tool recommendations.
- [x] **CLI `skip-failed`** — `crispsorter index skip-failed [--dry-run]` permanently
  marks timeout/other rows as "unsupported" so the worker stops retrying them.

### P11 — Remote server (remaining)

- [x] **Server queue blob fix** — shipped: `embeddings_blob BLOB` + `embed_dims` columns;
  `payload_json` stores compact batch with empty vectors; blob is repacked on claim.
- [x] **IVF-PQ at 100M+ vectors** — `num_partitions` auto-scales to `sqrt(row_count)`,
  `sample_rate` exposed on `index_build_ivf_pq` Tauri command + `build_vector_index()`.
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
- [x] **VPS-trigger indexing** — `vps_worker.py` gains `_notify_crisp_index()`:
  after PROCESSED, POSTs L1 file metadata (from manifest `files[]`) to
  `CRISP_INDEX_URL/v1/ingest/batch` (batches of 64) via `urllib.request`.
  Opt-in via env vars: `CRISP_INDEX_URL`, `CRISP_INDEX_API_KEY`,
  `CRISP_INDEX_OWNER_ID`. Fully non-blocking on failure. Docs in readme.md.

### P13 — Image-vertical convergence with CrispLens (future)

CLIP image embedder (ONNX via ort, ~150 MB), face recognition (SCRFD + ArcFace),
Images tab in Übersicht, face clustering. Requires P11 server + SyncManager stable.
See [HISTORY.md](HISTORY.md) for detailed spec.

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
