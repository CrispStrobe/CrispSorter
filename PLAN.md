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
- **cb-api catalog moved to a dedicated block volume** (2026-05-28) — `CB_API_DB_PATH` in `/etc/cb-api.env` + `/etc/vps-worker.env` should point at attached block storage (ext4/XFS with proper POSIX `fsync`), not the host root disk or a CIFS share.  Deployment-specific paths kept out of the public repo; see the cloud-backup readme + env example for the supported shape.
- **cb-api body-store split → LanceDB** (cloud-backup's *Stage W*, **not** the same as CrispSorter's Stage W skeleton-index) — body text (`full_text`) routes through the new `api/body_store.py` and lives in the per-shard Lance `documents` table on attached object storage instead of inline in `file_references`.  Toggled via `CB_BODY_BACKEND=lance` in `/etc/cb-api.env`.  Wire contract unchanged — `cloud_backup.rs::ManifestRow`/`ManifestPullResponse`/`SearchHit` work against both backends with **zero protocol changes**, verified end-to-end via `crispsorter sync cloud-backup pull --include-full-text` returning identical bytes regardless of which backend the cb-api is running.  Scales the cb-api remote backend toward 5 TB+ of corpus without bloating the catalog volume; the catalog grows at metadata-only rate (~few hundred bytes/row → ~3 GB at 5 M files).  Test coverage: 53 new unit + 6 new live in cloud-backup (`tests/test_body_store.py` + `tests/test_file_lifecycle.py` + `tests/test_search_edge_cases.py` + `tests/test_real_file_extraction.py` + `tests/test_wallabag_live.py`).
- **Source-URL provenance** (v106, both repos) — `ExtractedDocument.source_url` + `RawDocument.url` + `DocumentChunk.url` (Arrow Utf8) + `ManifestRow.url` / `PullRow.url` / `SearchHit.url` on the wire.  Markdown extractor lifts YAML frontmatter `url:` via a tiny hand-parser (no new YAML dep — wallabag/Pocket/read-later exports use a uniform key:value shape).  PDF extractor lifts via lopdf's Info dict `/URL` + XMP `<dc:source>` / `<xmp:URL>`.  Cb-api stores in `file_references.url` (FTS5-indexed); Lance side mirrors as the documents-table `url` column.  CLI: `crispsorter index search --url-domain spiegel.de` pushes `url LIKE '%spiegel.de%'` into LanceDB's scalar SQL; same filter shape lands on cb-api's `/api/v2/index/search` via `HybridSearchFilters.url_domain`.  Migration v106 adds the column to existing LanceDB tables via `NewColumnTransform::AllNulls`.
- **Structured tags** (v107, both repos) — `ExtractedDocument.tags` + `RawDocument.tags` (already existed) + `DocumentChunk.tags` (Arrow `List<Utf8>`, already existed) + `ManifestRow.tags` / `PullRow.tags` / `SearchHit.tags` on the wire (`Vec<String>`).  Markdown extractor parses both YAML flow-form (`tags: ["a", "b"]`) and block-form (`tags:\n  - a\n  - b`).  Cb-api stores as JSON-encoded text in `file_references.tags` with a small `_decode_tags` helper that gracefully handles legacy NULL rows.  `ManifestRow::from_raw_document` filters out the `collection:<id>` routing markers so they don't leak into the user-visible tag set.
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
- P16 docx translation (Translate tab, v0.2.0): end-to-end `.docx` → `.docx` LLM translation via the [`crisp-docx`](https://github.com/CrispStrobe/crisp-docx) sibling workspace. 12 cloud LLM providers (OpenAI / Anthropic / Ollama / Groq / OpenRouter / Together / Cerebras / Mistral / Nebius / Scaleway / Poe / Google) + offline NMT via CrispASR (m2m100 / wmt21 / madlad / gemma4-e2b under `--features translate-nmt`); opt-in intra-paragraph format preservation via SimAlign + CrispEmbed under `--features translate-align`. Streams `translate://progress` events, persists form state, shows provider key status. **OS-keychain credential storage** for all LLM API keys with one-time migration out of plaintext `settings.json`. macOS arm64 + Linux release binaries ship with both `translate-align` and `translate-nmt` enabled; Windows release stays feature-less pending the deferred DLL-layout work.

Run `cargo test --workspace --lib` for the exact Rust unit-test count.
For per-feature deep-dives, see [HISTORY.md](HISTORY.md).

---

## Open TODOs

Only `[ ]` items live here. Shipped items are in HISTORY.md.

### P3.5 — CrispEmbed / CrispASR bundling

- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session) — RPATH / DLL colocation; each platform needs 1-2 release iterations. Opening prompt: `handover-prompts/session-prompt-crispembed-ci-matrix.md` (local-only — see .gitignore).
- [ ] **Phase 3 — mobile** (deferred)

### P5 — Future / planned

- [ ] **Batch session persistence → SQLite** — *high priority*; reproduced twice 2026-05-17 as "we LOST all the files?!" + UI hangs at 53/196.  Root cause: full batch persisted as a single JSON blob in `settings.json` via tauri-plugin-store; every save rewrites the whole file (tens of MB when items carry `extractedText`).  Mitigations landed in `068f8f5`/`05123d9`/`e84ca88` (strip `extractedText` to 500 chars in persisted snapshot, single-writer chain, auto-lift stuck items) but the design is fundamentally wrong for the workload.  5-slice migration plan (schema → Rust commands → TS wrapper + JSON migration → wire BatchManager → stress test) spec'd in `handover-prompts/session-prompt-batch-sqlite-persistence.md` (~7 h total).
- [ ] **Auto-process toggle on watch detection** — UX design pass complete (2026-05-16): per-folder three-mode dropdown (off / analyse / sort), opt-in initial scan, debounced queue, hourly file cap + daily cost cap, tray status surface, fail-soft error path.  6-slice implementation arc spec'd in `handover-prompts/session-prompt-auto-process-toggle.md` (~16 h total).
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
