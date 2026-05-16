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
- P15 Batch pre-processing: content-dedup (SHA-256), book-chapter grouping (ISBN-13)
- OCR: Tier 1 Tesseract, Tier 2 ocrs, Tier 3 PaddleOCR (`--features paddle-ocr`)
- `.cidx` offline archives: LanceDB + Tantivy FTS export/mount, Archiv tab in Übersicht, background-promote per row
- Schema-migration framework: versioned `Migration` trait with SQLite ledger, gap/duplicate detection, idempotent reruns; five consumers: `AddTextTranslatedColumns` (v100), `AddAudioMetadataColumns` (v101), `AddImageMetadataColumns` (v102), `RebuildFtsForBodyTranslated` (v103 — rebuilds Tantivy dir with `body_translated` schema on legacy indexes), `NullifyTranslationOnSubChunks` (v104 — nulls replicated translations on sub-chunk rows, saves O(N) storage); per-chunk translation dedup in ingest (Stage AA)
- `crisp+cb-archive://` URI scheme for cloud-backup archived files
- `crisp+drive://` URI scheme for any registered CloudDrive
- macOS arm64 packaging: `scripts/bundle_macos_native_libs.sh` co-bundles dylibs + ggml backends + homebrew transitives into `.app/Contents/Frameworks/`

Run `cargo test --workspace --lib` for the exact Rust unit-test count.
For per-feature deep-dives, see [HISTORY.md](HISTORY.md).

---

## Open TODOs

Only `[ ]` items live here. Shipped items are in HISTORY.md.

### P13.7 — Cloud-sync deferred items

- [x] **Skeleton index preservation (Stage AB)** — **SHIPPED 2026-05-16**. In `purge_to_size` Phase 2, opens `skeleton_index.db` (if it exists beside `lance/`) before deleting rows; extracts `author` + `parent_dir` from the full-row batches (deduped on `chunk_index==0`), upserts to `SkeletonIndex`; no-op on installs without skeleton mode. 1 new test `purge_preserves_skeleton_hints_on_eviction`.
- [ ] **Live test: shard backup to WebDAV** — backup to a tempfile WebDAV server, verify integrity via sha256 of unpacked tarball. (requires live drive)
- [ ] **Live tests: thin-client batch upload** — ship a small zipped batch end-to-end; verify rows appear in `/api/v2/index/search` with expected `full_text` + `embedding`. (requires live VPS)
- [ ] **Live test: VPS extraction** — VPS with `CB_CRISPLENS_URL` + `CB_CRISPASR_BIN` populated; upload an image + audio file; verify `face_count` + `full_text` in `<catalog-db>`. (requires live VPS with crispasr binary)

### P3.5 — CrispEmbed / CrispASR bundling

- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session) — RPATH / DLL colocation; each platform needs 1-2 release iterations. Opening prompt: `handover-prompts/session-prompt-crispembed-ci-matrix.md` (local-only — see .gitignore).
- [ ] **Phase 3 — mobile** (deferred)

### P5 — Future / planned

- [ ] **Auto-process toggle on watch detection** — risky, needs UX design pass before any code
- [ ] **PWA demo via File System Access API** — speculative

### P7.8 — OCR Tier 3 polish + Tier 4

- [ ] **SLANet table extraction** on top of Tier 3 PaddleOCR — adds structured table output for invoices / bank statements / grids. The `usls` crate already hosts a SLANet model. ~3-5 h.
- [ ] **Tier 4 — VLM OCR** (~1 wk) — `deepseek-ocr.rs`-style via Candle (not ort). DeepSeek-OCR / PaddleOCR-VL, Q4_K–Q8_0 quantisation, 4.7-9 GB models, macOS Metal target.

### P8.2 — CLI polish remaining

- [ ] **`cargo install crispsorter`** for the Tauri-app binary — needs binstall recipe + signing (macOS Developer ID, Windows Authenticode). `cargo install --path crates/crispcat-cli` already ships. ~2-4 h once a signing identity is in hand.

### CrispEmbed — leverage unused capabilities

- [ ] **ColBERT multi-vector retrieval** (`encode_multivec`, ~1 session) — per-token L2-normalised embeddings (BGE-M3 ColBERT head). Needs a new LanceDB column for the per-token vectors (FixedSizeList of variable length is awkward; might need a separate `chunk_multivec` table joined by `id`) + a late-interaction MaxSim scorer in the search pipeline.
- [ ] **Omnimodal cross-modal search** (`encode_audio` / `encode_image`, ~2 sessions) — BidirLM-Omni encodes text, audio, and images into a shared 2048-d space. Unlocks: type "photo of a sunset" → image hits without OCR; type "podcast about Bosnia" → audio hits without transcription. Needs a new model class (BidirLM-Omni isn't in the existing `EmbedderModel` enum), image-patch preprocessing (pixel patches + `grid_thw`), and a decision about how the 2048-d cross-modal vector coexists with the existing per-backend dense column (separate column? per-index dim selection at init?).

### P13.5 follow-ups (remaining)

- [ ] **Non-whisper audio-LID auto-resolution** — `2b80345` handles the whisper-method case by registry-resolving `whisper`. Silero / Ecapa / Firered still require explicit `--lid-model` paths because they aren't in CrispASR's registry. Add upstream registry entries (`lid-silero`, `lid-ecapa`, `lid-firered`) to close this.

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
