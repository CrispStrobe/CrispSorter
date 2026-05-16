# CrispSorter — Development Plan

> **Full specs for completed phases** → [HISTORY.md](HISTORY.md)
> **Technical patterns / pitfalls** → [LEARNINGS.md](LEARNINGS.md)
> **In-flight integration designs** → [docs/](docs/)

---

## Capabilities (shipped)

- LanceDB + Tantivy hybrid search, RRF fusion, sparse BGE-M3/SPLADE channel
- ONNX/CoreML + CrispEmbed GGUF backends, 36-model registry; registry-driven
  embedder selection (Stage X): any GGUF entry selectable without a release,
  `Embedder.runtime_dim` auto-discovered, download+select UI per entry
- Batch AI sort (Stapel): extraction → LLM metadata → sort-path → move/copy/script
- P6 Catalog: `.caf` I/O, parallel scanner, duplicate engine, Übersicht columnar browse
- P7 Desktop search parity: folder tree, million-row pagination, preview pane, bg ingest
- P8 CLI: `version / doctor / catalog / index / batch / chat / completion / manpage`
- P9 Übersicht scale: DB-side ORDER BY (lance::Scanner), scalar indexes, volume filter
- P10 Robust ingest: TaskFailureReason, 300 s timeout, L2 fallback, DRM detection, skip-failed CLI
- P11 Remote server: `crisp-index-server` (Axum + LanceDB + Tantivy), durable job queue, server-side embedding
- P11 Cloud drives: `LocalDrive` + `InternxtDrive` + `FilenDrive` + `WebDavDrive` (live-verified); registry with create/edit/delete UI; `crisp+drive://` URIs; manifest-only L1 ingest + on-demand L3 promote
- P11 SyncManager: pull-apply loop closed; federated search across local + cb-api + CrispLens (Stage S) with RRF merge + per-backend badges
- P12 cloud-backup: L1 manifest import, L3 via `retrieve.py`, reverse lookup, VPS-trigger indexing
- P13 Bilder vertical: image-row filtered Übersicht tab, lazy thumbnails, EXIF preview pane, SHA-256 + perceptual-hash dup grouping, CrispLens Tier 2 connector
- P13.5 Audio + Translation vertical: symphonia + ffmpeg decode, 24 ASR / 5 TTS / 4 MT / 4 LID backends, `chat transcribe` + `chat tts` CLI, index-time audio/video extraction (22 file types), audio-LID routing, text-LID at index time, on-demand + batch translation; script-aware multilingual reranker routing (Stage Z: `has_nonlatin_script` ≥25% threshold, `reranker_multilingual` field, UI dropdown)
- P13.6 Multimodal UX + L1/L2/L3 audio: Stapel + Kataloge accept all 22 extensions; audio L2 via schema migration v101; `index_audio_promote_l3` action
- P13.7 Image L1/L2/L3 + search CLI + CrispLens push: image L2 via migration v102; `crispsorter index search` CLI with full filter set; CrispLens image push
- P13.7 Cloud-backup HTTP API + bidirectional sync: cb-api (FastAPI, bcrypt auth, manifest push/pull, shard export/import/list, embedding push/query); CrispSorter SyncManager `CloudBackup` mode; GUI Cloud-backup panel; sync CLI; shard backup to cloud drives (Stage Q) with incremental watermarks + retention; manifests-DB import bridge (Stage R); cb-api key minting from GUI (Stage T); L1-only thin-client mode (Stage U) with vps_worker CrispLens + CrispASR bridges (Stage V); skeleton local index (Stage W) + remote-only search fallback; "Sync now" button + `sync_status_all` (Stage O)
- P13.7 Local DB size cap + LRU pruning (Stage P): `IndexConfig.local_max_size_bytes`, `crispsorter index purge --max-size N`, 1-hour background purge worker
- P13.5 Stage AC — Non-whisper audio-LID auto-resolution: `lid-silero` / `lid-ecapa` / `lid-firered` registry entries in CrispASR; `LidMethodChoice::Ecapa/Firered` CLI variants; `resolve_audio_lid_model_path` generic resolver; Silero/Ecapa/Firered auto-resolve arms in `cmd_chat_transcribe`; Phase 6 — `crispasr::LidMethod::{Firered=2, Ecapa=3}` upstream variants, `detect_language_from_pcm` routes all 4 through the same module-level C-ABI
- P15 Batch pre-processing: content-dedup (SHA-256), book-chapter grouping (ISBN-13)
- OCR: Tier 1 Tesseract, Tier 2 ocrs, Tier 3 PaddleOCR (`--features paddle-ocr`)
- `.cidx` offline archives: LanceDB + Tantivy FTS export/mount, Archiv tab in Übersicht, background-promote per row
- Schema-migration framework: versioned `Migration` trait with SQLite ledger, gap/duplicate detection, idempotent reruns; six consumers: `AddTextTranslatedColumns` (v100), `AddAudioMetadataColumns` (v101), `AddImageMetadataColumns` (v102), `RebuildFtsForBodyTranslated` (v103), `NullifyTranslationOnSubChunks` (v104), `AddColbertMultivec` (v105 — `multivec_packed LargeBinary` + `multivec_n_tokens Int16`)
- Stage AD — ColBERT multi-vector retrieval: `EmbedHandle::embed_multivec` + `has_colbert`; ingest populates `multivec_packed`/`multivec_n_tokens` when BGE-M3 GGUF is active; `maxsim` late-interaction scorer + `unpack_multivec` in search.rs
- Stage AE — ColBERT search-time re-ranking: `LocalIndex::rerank_with_colbert(candidates, query_multivec, limit)` fetches `multivec_packed` by `chunk_row_id`, replaces each candidate's score with MaxSim, re-sorts; wired through `SearchEngine::maybe_colbert_rerank` into both `search_hybrid` and `search_text` via `SearchFilters::colbert_rerank` flag; surfaced as `--colbert` on `crispsorter index search`; empty-query is a no-op, rows without multivec keep their original score
- `crisp+cb-archive://` URI scheme for cloud-backup archived files
- `crisp+drive://` URI scheme for any registered CloudDrive
- macOS arm64 packaging: `scripts/bundle_macos_native_libs.sh` co-bundles dylibs + ggml backends + homebrew transitives into `.app/Contents/Frameworks/`

Run `cargo test --workspace --lib` for the exact Rust unit-test count.
For per-feature deep-dives, see [HISTORY.md](HISTORY.md).

---

## Open TODOs

Only `[ ]` items live here. Shipped items are in HISTORY.md.

### P13.7 — Cloud-sync deferred items

- [x] **Live test: shard backup to WebDAV** — *2026-05-16*: full end-to-end against Internxt's built-in WebDAV server (`internxt webdav-start -b` on `http://localhost:9999/`).  Three layers verified: (a) transport — both `webdav_live_*` tests pass (PROPFIND root listing + PUT→STAT→GET→DELETE roundtrip via `WebDavDrive`); (b) Internxt-direct — added two `internxt_live_*` tests exercising the `cli.py` subprocess path (full WRITE→STAT→READ→DEL→STAT-after-delete roundtrip against the live account); (c) integration — pulled the production `__single__` shard from cb-api (`/api/shard/export`, 88179 byte gzip tarball, sha256 captured), MKCOL parent, PUT via WebDAV (201), GET back (200, 88179 bytes), **sha256 matches byte-for-byte**, tarball contains `shard.db`; cleanup DELETE 204+204.  Real bugs caught and fixed across CrispSorter + cloud-backup + internxt-python (commits `5ab135f`, `9f56cb5`, `7b09898`).
- [x] **Live tests: thin-client batch upload** — *2026-05-16*: deployed cloud-backup `api/` to the production VPS (rsync; see `CLAUDE.md` for the topology).  Verified end-to-end against the live cb-api on `127.0.0.1:7869`: (1) manifest push (`{accepted:1}`), (2) file upload-by-hash streaming POST (`stored:true, local_blob_path:"b2/75/..."`), (3) GET download with byte-for-byte sha verification, (4) `/api/v2/extract/status` correctly tracks queue state (seeded 3 rows: pending/in_progress/done → endpoint returned `pending:1, in_progress:1, done:1, worker_db_found:true`; cleanup → `0/0/0`), (5) `/api/shard/list` returned the production `__single__` shard with row_count=2124, max_indexed_at, (6) `/api/shard/export/__single__` streamed a 76 KB gzip tarball containing `shard.db` (500 KB).  Proves the streaming-upload Rust fix + Stage R wire shape + Stage U status + Stage Q export against production.
- [x] **Live test: VPS extraction — image path** — *2026-05-16*: full end-to-end against production cb-api + vps-worker + face-rec (CrispLens on `127.0.0.1:7865`).  Stage U/V chain verified: client `POST /api/manifest/push` → `POST /api/files/by-hash/<sha>` (streaming) → vps-worker's 60-s `ExtractionWorker.enqueue_pending()` picks up the row → dispatches to `_extract_via_crisplens` (image ext) → CrispLens RetinaFace+ArcFace returns `face_count` + caption → `UPDATE file_references SET face_count = ?, full_text = ? WHERE file_hash = ?`.  Eleven blobs drained in ~30 s; queue went `pending:11 → done:11, failed:0`; the test `png` correctly returned `face_count=0` (8×8 PNG, no faces), text rows got `full_text` populated by `_extract_text_from_blob`.  Caught + fixed two real bugs along the way: `cloud-backup 9aaefb1 fix(extract): join through files for blob path; use file_hash on file_references` (the original `enqueue_pending` SELECT raised `OperationalError("no such column: local_blob_path")` against the controller.py legacy schema) and `9f56cb5 fix(extract): send required local_path field + bump CrispLens timeout` (upstreamed from a VPS-side hotfix).
- [ ] **Live test: VPS extraction — audio path** — blocked on CrispASR binary not yet built.  Rust crate at `/mnt/storage/whisper.cpp/crispasr/`; build via `/root/.cargo/bin/cargo build --release` (cargo is not on default PATH).  Then add `CB_CRISPASR_BIN=…/target/release/crispasr` to `/etc/vps-worker.env` and restart.  The `_extract_via_crispasr` function graceful-no-ops until the binary exists.

### P3.5 — CrispEmbed / CrispASR bundling

- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session) — RPATH / DLL colocation; each platform needs 1-2 release iterations. Opening prompt: `handover-prompts/session-prompt-crispembed-ci-matrix.md` (local-only — see .gitignore).
- [ ] **Phase 3 — mobile** (deferred)

### P5 — Future / planned

- [ ] **Auto-process toggle on watch detection** — risky, needs UX design pass before any code
- [ ] **PWA demo via File System Access API** — speculative

### P7.8 — OCR Tier 3 polish + Tier 4

- [ ] **SLANet table extraction** on top of Tier 3 PaddleOCR — adds structured table output for invoices / bank statements / grids.  The `usls` crate already hosts a SLANet model.  ~3-5 h.  *Handover prompt ready:* `handover-prompts/session-prompt-slanet-table-extraction.md` (210 lines; design questions resolved, step-by-step plan).
- [ ] **Tier 4 — VLM OCR** (~1 wk, 3-4 focused sessions) — `deepseek-ocr.rs`-style via Candle (not ort). DeepSeek-OCR / PaddleOCR-VL, Q4_K–Q8_0 quantisation, 4.7-9 GB models, macOS Metal target.  *Handover prompt ready:* `handover-prompts/session-prompt-tier4-vlm-ocr.md` (226 lines; full multi-session arc).

### P8.2 — CLI polish remaining

- [ ] **`cargo install crispsorter`** for the Tauri-app binary — needs binstall recipe + signing (macOS Developer ID, Windows Authenticode). `cargo install --path crates/crispcat-cli` already ships. ~2-4 h once a signing identity is in hand.  *Handover prompt ready:* `handover-prompts/session-prompt-cargo-install-signed.md` (354 lines; covers Apple notarisation + Authenticode + crates.io flow + the `if: always()` release-pipeline fix).

### CrispEmbed — leverage unused capabilities

- [ ] **Omnimodal cross-modal search** (`encode_audio` / `encode_image`, ~2 sessions) — BidirLM-Omni encodes text, audio, and images into a shared 2048-d space. Unlocks: type "photo of a sunset" → image hits without OCR; type "podcast about Bosnia" → audio hits without transcription.  *Handover prompt ready:* `handover-prompts/session-prompt-omnimodal-cross-modal-search.md` (399 lines; 9 design questions resolved, schema v106 spec, sidecar-embedder pattern, Rust-port spec for the HF image processor).

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
