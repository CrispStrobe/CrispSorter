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
- P11 Cloud drives: `LocalDrive` + `InternxtDrive` + `FilenDrive` + `WebDavDrive` (live-verified against both Filen + Internxt local WebDAV servers); registry with create/edit/delete UI; `crisp+drive://` URIs; manifest-only L1 ingest + on-demand L3 promote
- P11 SyncManager: pull-apply loop closed (writes pulled rows as L1 metadata in local LanceDB)
- P12 cloud-backup: L1 manifest import (`source_files` → LanceDB), L3 via `retrieve.py`
- P15 Batch pre-processing: content-dedup (SHA-256), book-chapter grouping (ISBN-13)
- OCR: Tier 1 Tesseract, Tier 2 ocrs, Tier 3 PaddleOCR (`--features paddle-ocr`)
- `.cidx` offline archives: LanceDB + Tantivy FTS export/mount, Archiv tab in Übersicht
- `crisp+cb-archive://` URI scheme for cloud-backup archived files
- `crisp+drive://` URI scheme for any registered CloudDrive (Local / Filen / Internxt / WebDAV)

---

## In Progress

*(nothing actively in-flight — see Open TODOs below)*

**Test coverage:** 215 unit tests pass (195 tauri-app + 20 crispcat).
Run with `cargo test --workspace`. See [HISTORY.md](HISTORY.md) → "Test sweep
— 2026-05-09" for a per-module breakdown.

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

- [x] **Phase 5 — `crispcat` workspace crate** — `crates/crispcat/` ships
  caf/dedup/index/scan modules; `lance` module is feature-gated (default off)
  so a `cargo install crispcat-cli` build doesn't pull in lancedb. The Tauri
  app uses `crispcat = { features = ["lance"] }` and re-exports it as
  `crate::catalog` so existing call sites are unchanged.
  `crates/crispcat-cli/` ships a standalone `crispcat scan|info|browse|find-dupes`
  binary — no Tauri, no LanceDB, no embedder.

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
- [x] **`batch process`** — headless LLM extraction pass via OpenAI-compatible endpoint.
  `crispsorter batch process [--job-id J] [--limit N] [--llm-url URL] [--llm-model M]
  [--export-path DIR] [--path-template T] [--out-plan FILE] [--dry-run]`
  Extracts text → calls chat/completions → parses XML metadata → emits sort plan JSON.
- [x] **`chat query "<prompt>"`** — POSTs to OpenAI-compatible /chat/completions;
  `--context-files` extracts + appends text from files; `--system` sets system prompt.
  transcribe / tts deferred (need ASR/TTS headless bootstrap).
- [x] **Polish (partial)** — `cargo install --path crates/crispcat-cli` works
  for the standalone catalog CLI. Full `cargo install crispsorter` story for
  the Tauri-app binary is still WIP (needs binstall recipe + signing).

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
- [x] **Runtime modes** — `BackendType` gains `Hybrid` variant (serializes as "hybrid").
  Hybrid init path = Local for now (SyncManager placeholder). Settings dropdown
  shows Standalone/Server/Hybrid with i18n. Data-dir + remote fields visible in Hybrid.
- [x] **Cloud drives** — `trait CloudDrive` (list_dir/read_file/write_file/
  delete/stat) + `LocalDrive` (std::fs, covers OS-mounted SMB/SFTP/NFS) +
  `InternxtDrive` (Python `internxt-cli` bridge — patched cli.py adds
  `--json` on `whoami`/`list-path`/`resolve`) + `FilenDrive` (Python
  `filen-cli` bridge — patched cli.py adds `--json` on `whoami`/`ls`/
  `resolve`/`trash`, plus a missing `handle_trash` impl) + `WebDavDrive`
  (generic HTTP — Nextcloud/ownCloud/mailbox.org/Synology + the local
  WebDAV servers that filen-cli and internxt-cli expose; PROPFIND parser
  handles both `D:`-prefixed and default-namespace wire shapes; optional
  `insecure_tls` for self-signed servers).  `DriveRegistry` (drives.json
  persistence with optional `username` / `password` / `insecure_tls`
  fields) routes each `DriveType` to its real backend.  6 Tauri commands
  (`drive_list / drive_create / drive_update / drive_delete /
  drive_list_dir / drive_stat`).  SFTP still piggybacks on OS-mount.
- [x] **Generic remote ingest + promote (`crisp+drive://`)** —
  `FileLocation::Drive { drive_id, remote_path }` URI scheme;
  `crate::drives::walk()` recursive walker over any registered drive;
  `index_ingest_drive_manifest` Tauri command (manifest-only L1 ingest,
  no bandwidth cost beyond directory listings); `index_promote_drive_archive`
  Tauri command (fetch via `read_file`, stage, route through the existing
  cb-archive `promote_path` pipeline → L3).
- [x] **UI wiring** — Quellen tab → "Cloud-Ordner" toolbar button →
  inline dialog with: drive picker, create/edit/delete drive form
  (Label, Typ, URL/Pfad, optional WebDAV Benutzer/Passwort + selbst-
  signiertes Zertifikat akzeptieren), remote path, ext filter, depth.
  Per-row CloudDownload icon-button on `crisp+drive://` index rows
  (Promote to L3, sibling to the existing cb-archive button).
- [x] **Live e2e tests + server-side bug fixes** — 2 `#[ignore]`'d
  integration tests (`webdav_live_list_root`, `webdav_live_write_read_delete_roundtrip`)
  surfaced two real upstream bugs that were patched in their respective
  repos: filen-python missed cache invalidation on `trash_item` /
  `delete_permanent` (DELETE always 500'd via wsgidav's post-check);
  internxt-cli's `Folder.get_etag()` crashed with `int(None)` on root
  PROPFIND.  Verified live against both filen-python webdav-start :8088
  and internxt-cli webdav-start :9999 — full PUT→STAT→GET→DELETE round-
  trip succeeds on each.
- [x] **SyncManager** — `src-tauri/src/sync/`: SQLite outbox (`sync_outbox.db`),
  `enqueue/claim_batch/mark_done/mark_error/clear_failed`, `push_pending`
  (POST per op type), `pull_pending` (GET `/v1/sync/since?ts=…&limit=…`),
  `is_remote_online` (GET /health), `sync_state` kv table.
  Server: new `routes/sync.rs` + `VectorStore::rows_since(since_ms, limit)`
  + stdlib `iso_from_ms` formatter; returns paginated chunk_index=0 rows
  with `{rows, max_indexed_at, has_more}` shape.
  5 Tauri commands: `sync_status/sync_push/sync_pull/sync_enqueue/sync_clear_failed`.
  Nav sync chip (⇅ N) polls every 30 s; click triggers push.

### P12 — cloud-backup (remaining)

- [x] L1 manifest import via `index_ingest_cb_manifest`
- [x] L3 promotion via `retrieve.py` (`index_promote_cb_archive` + CloudDownload button)
- [x] **Reverse lookup UI** — `index_lookup_cb_file` Tauri command queries
  `source_files`+`archives`; preview pane shows Lokal / VPS / **Cloud (Internxt)**
  availability when a `crisp+cb-archive://` row is opened. Reads
  `archives.upload_verified` + `remote_path` + `local_deleted` so the chip
  distinguishes "VPS verified" from "VPS pruned, cloud-only". Manifest DB
  path persisted as `cbManifestDbPath` setting on first import.
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
