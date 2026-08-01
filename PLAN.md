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
- **cb-api catalog moved to a dedicated block volume** (2026-05-28) — `CB_API_DB_PATH` in `<cb-api-env>` + `<vps-worker-env>` should point at attached block storage (ext4/XFS with proper POSIX `fsync`), not the host root disk or a CIFS share.  Deployment-specific paths kept out of the public repo; see the cloud-backup readme + env example for the supported shape.
- **cb-api body-store split → LanceDB** (cloud-backup's *Stage W*, **not** the same as CrispSorter's Stage W skeleton-index) — body text (`full_text`) routes through the new `api/body_store.py` and lives in the per-shard Lance `documents` table on attached object storage instead of inline in `file_references`.  Toggled via `CB_BODY_BACKEND=lance` in `<cb-api-env>`.  Wire contract unchanged — `cloud_backup.rs::ManifestRow`/`ManifestPullResponse`/`SearchHit` work against both backends with **zero protocol changes**, verified end-to-end via `crispsorter sync cloud-backup pull --include-full-text` returning identical bytes regardless of which backend the cb-api is running.  Scales the cb-api remote backend toward 5 TB+ of corpus without bloating the catalog volume; the catalog grows at metadata-only rate (~few hundred bytes/row → ~3 GB at 5 M files).  Test coverage: 53 new unit + 6 new live in cloud-backup (`tests/test_body_store.py` + `tests/test_file_lifecycle.py` + `tests/test_search_edge_cases.py` + `tests/test_real_file_extraction.py` + `tests/test_wallabag_live.py`).
- **Source-URL provenance** (v106, both repos) — `ExtractedDocument.source_url` + `RawDocument.url` + `DocumentChunk.url` (Arrow Utf8) + `ManifestRow.url` / `PullRow.url` / `SearchHit.url` on the wire.  Markdown extractor lifts YAML frontmatter `url:` via a tiny hand-parser (no new YAML dep — wallabag/Pocket/read-later exports use a uniform key:value shape).  PDF extractor lifts via lopdf's Info dict `/URL` + XMP `<dc:source>` / `<xmp:URL>`.  Cb-api stores in `file_references.url` (FTS5-indexed); Lance side mirrors as the documents-table `url` column.  CLI: `crispsorter index search --url-domain spiegel.de` pushes `url LIKE '%spiegel.de%'` into LanceDB's scalar SQL; same filter shape lands on cb-api's `/api/v2/index/search` via `HybridSearchFilters.url_domain`.  Migration v106 adds the column to existing LanceDB tables via `NewColumnTransform::AllNulls`.
- **Structured tags** (v107, both repos) — `ExtractedDocument.tags` + `RawDocument.tags` (already existed) + `DocumentChunk.tags` (Arrow `List<Utf8>`, already existed) + `ManifestRow.tags` / `PullRow.tags` / `SearchHit.tags` on the wire (`Vec<String>`).  Markdown extractor parses both YAML flow-form (`tags: ["a", "b"]`) and block-form (`tags:\n  - a\n  - b`).  Cb-api stores as JSON-encoded text in `file_references.tags` and as `List<Utf8>` in `documents.tags` on Lance.  `ManifestRow::from_raw_document` filters out the `collection:<id>` routing markers so they don't leak into the user-visible tag set.
- **Federated tag + URL filters on the v2 path** — `crispsorter sync cloud-backup hybrid-search --tag pocket-import --url-domain spiegel.de` pushes both predicates into cb-api's `/api/v2/index/search`.  Server translates: `--url-domain` → `url LIKE '%<dom>%'`, `--tag` → `array_has(tags, '<value>')`.  Both run on Lance's scalar SQL with the matching column types (Utf8 + List<Utf8>).  `HybridSearchHit` echoes both fields back on every result row so the federated tab renders "Open original" + tag chips without a second round-trip.
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
- **Search-UX Tier 1 + local `--tag` (v0.3.0)** — L1-aware local search (`sync cloud-backup pull` writes each pulled L1 row into local Tantivy, so a pulled corpus is findable offline via `index search`); unified top-level `crispsorter search "query"` that queries local + cb-api v2 hybrid, RRF-merged and source-badged (`--local-only` / `--cloud-only` to force a leg; shared `--ext`/`--lang`/`--folder-prefix`/`--year-min/max`/`--url-domain`/`--tag` filters); `<mark>`-highlighted ~300-char snippets (`index/snippet.rs::highlight_snippet`) + "Open original" globe button on hits carrying a `url`; `--tag` on local `index search` (`array_has(tags, …)`). Pulled rows ingest with `chunk_index = 0, chunk_total = 1`. See [HISTORY.md](HISTORY.md) 2026-05-29 + `RELEASE_NOTES_v0.3.0.md`.
- P16 docx translation (Translate tab, v0.2.0): end-to-end `.docx` → `.docx` LLM translation via the [`crisp-docx`](https://github.com/CrispStrobe/crisp-docx) sibling workspace. 12 cloud LLM providers (OpenAI / Anthropic / Ollama / Groq / OpenRouter / Together / Cerebras / Mistral / Nebius / Scaleway / Poe / Google) + offline NMT via CrispASR (m2m100 / wmt21 / madlad / gemma4-e2b under `--features translate-nmt`); opt-in intra-paragraph format preservation via SimAlign + CrispEmbed under `--features translate-align`. Streams `translate://progress` events, persists form state, shows provider key status. **OS-keychain credential storage** for all LLM API keys with one-time migration out of plaintext `settings.json`. macOS arm64 + Linux release binaries ship with both `translate-align` and `translate-nmt` enabled; Windows release stays feature-less pending the deferred DLL-layout work.

- **Universal document viewer (v0.8.0/v0.9.0)** — `DocumentViewer.svelte` with format-specific sub-viewers (PDF canvas, image zoom/pan, DOCX, EPUB, HTML, CSV, text). `PdfTools.svelte` tab with 18 lopdf-based PDF operations (extract, remove, reorder, rotate, crop, merge, split, page numbers, watermark, insert blank, metadata, encrypt, decrypt, sanitise, signatures, PDF/A, redact).
- **Discovery & clustering (v0.9.0)** — K-means++ topical clustering, knowledge graph (NER co-occurrence), synonym expansion (94 EN+DE groups), RSS/Atom feed ingestion, clipboard/screenshot capture.
- **DMS & compliance (v0.9.0)** — document versioning (SHA-256 groups), audit trail (append-only SQLite), retention policies, document comparison (word-level diff), annotation layer, reading queue/highlights, stamp on export, DOCX/HTML export.

- **CrispEmbed scan cleanup (v0.9.1)** — despeckle, blackfilter, two-up page splitting, content-bbox auto-crop. Wired via `OcrCleanupSpec` toggles + standalone Tauri commands.
- **Document-type classification (v0.9.1)** — heuristic classifier (18 types) runs at ingest, auto-tags every document with `doctype:<class>`.

Run `cargo test --workspace --lib` for the exact Rust unit-test count (1034 as of P28+P26.4+P27.8+P27.11).
For per-feature deep-dives, see [HISTORY.md](HISTORY.md).

---

## Open TODOs

Only `[ ]` items live here. Shipped items are in HISTORY.md.

### P3.5 — CrispEmbed / CrispASR bundling

- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session) — RPATH / DLL colocation; each platform needs 1-2 release iterations. Opening prompt: `handover-prompts/session-prompt-crispembed-ci-matrix.md` (local-only — see .gitignore).
- [x] **Phase 3 — mobile** ✅ SHIPPED v0.4.0→v0.4.1 (2026-06-04/05).  Full feature parity on Android aarch64 + iOS.  No feature flags — one binary, all platforms.  Vendored OpenSSL; lance-linalg Android+iOS patch; responsive UI (bottom tab bar on phones); CI jobs for APK + IPA.  Sidecar commands (Ollama/llama.cpp/MLX/TTS spawn) compile everywhere but only surface in desktop UI.  `mistralrs` runs in-process on all platforms.  See [HISTORY.md](HISTORY.md) 2026-06-04 session log.
  - [x] **Android SAF handler** — `mobile_fs` Rust module with Tauri commands (`mobile_fs_list_folder`, `mobile_fs_read_file`, `mobile_fs_move_file`, `mobile_fs_create_dir`, `mobile_fs_delete`).  `SAFBridge.kt` in `src-tauri/android-src/` (copy into gen/android/ after init).  Desktop fallback via `std::fs`.
  - [x] **iOS security-scoped bookmarks** — `mobile_fs_start_access` / `mobile_fs_stop_access` Tauri commands.  Placeholder for objc2 FFI to `NSURL.startAccessingSecurityScopedResource()`.
  - [x] **Native lib bundling scripts** — `scripts/bundle_android_native_libs.sh` (copies .so into `jniLibs/arm64-v8a/`), `scripts/bundle_ios_frameworks.sh` (copies xcframeworks into `gen/apple/Frameworks/`).
  - [ ] **iOS unsigned .app in release** — Rust cross-compiles for `aarch64-apple-ios` but Tauri's xcodebuild wrapper has a broken workspace file.  Workaround in CI: create valid `contents.xcworkspacedata` + call `xcodebuild -project` with `CODE_SIGNING_ALLOWED=NO`.  In progress.
  - [x] **Platform detection** — `src/lib/platform.ts` (`isMobile()`, `isDesktop()`, `platformName()`).  Settings uses `showSidecarControls` to hide spawn buttons on mobile.

### P5 — Future / planned

- [x] **Batch session persistence → SQLite** — ✅ SHIPPED (commits `06e0282` → `00e9962`).  Fixed the "we LOST all the files?!" data-loss + UI-hang-at-53/196 bugs by replacing the single JSON-blob-in-`settings.json` persistence with a transactional SQLite store (`src-tauri/src/batch_session/`, one row per item, WAL, bulk upserts).  All 5 slices landed plus extras the handover prompt didn't spec (processed-history dedup → skip re-extraction of previously-sorted files: `record_processed`/`lookup_history`/`history_count`; full `extractedText` stripped from the IPC payload + lazy-loaded from SQLite on resume).  15 `batch_session` unit tests green (roundtrip, bulk 100+, interleaved upsert/clear, migration sentinel, processed-history).  See [HISTORY.md](HISTORY.md) + `handover-prompts/session-prompt-batch-sqlite-persistence.md` for the original spec.
- [x] **Auto-process toggle on watch detection** — ✅ SHIPPED (2026-06-21).
  Per-folder `WatchMode` enum (Off/Analyse/Sort), debounced auto-dispatch
  queue (5 s batch window → `folder-watch:auto-process` Tauri event),
  rate limiting (hourly per-folder cap 100, daily global cap 500), opt-in
  initial scan on registration.  Tauri commands: `watch_set_mode`,
  `watch_list_modes`, `watch_queue_status`.  Frontend: per-folder mode
  dropdown in Settings, persisted in `watchModes`.  `ALLOWED_EXTS`
  expanded to images + audio/video.  12 unit tests.  Remaining follow-ups:
  tray status surface, fail-soft dead-letter UI, cost cap (token-based
  rather than file-count-based) — tracked as future polish.
- [ ] **PWA demo via File System Access API** — speculative

### P7.8 — OCR Tier 3 polish + Tier 4

- [x] **SLANet table extraction** — ✅ SHIPPED (2026-06-05). `detect_table_structure()` + `ocr_with_tables()` in `ocr_paddle.rs`. Uses `usls::SLANet` with `slanet_lcnet_v2_mobile_ch` model (~50 MB).  Returns HTML table skeleton (`<table><tr><td>...`) appended to OCR text. Gated behind same `paddle-ocr` feature.  Frontend rendering of table structure pending.
- [x] **Tier 4 — VLM OCR** ✅ Superseded by CrispEmbed integration (P17.2 +
  P20). All VLM engines (DeepSeek-OCR-2, GOT-OCR2, GLM-OCR, Qwen2.5-VL,
  InternVL2, Granite Vision, LightOnOCR, Pix2Struct, Qwen3-VL) run via
  CrispEmbed GGUF — no Candle needed. 13 engines wired in `engine_id()`,
  CLI, and Settings UI. The Candle-based approach is no longer planned.

### P8.2 — CLI polish remaining

- [ ] **`cargo install crispsorter`** for the Tauri-app binary — needs binstall recipe + signing (macOS Developer ID, Windows Authenticode). `cargo install --path crates/crispcat-cli` already ships. ~2-4 h once a signing identity is in hand.  *Handover prompt ready:* `handover-prompts/session-prompt-cargo-install-signed.md` (354 lines; covers Apple notarisation + Authenticode + crates.io flow + the `if: always()` release-pipeline fix).

### P17 — CrispEmbed deep integration

CrispEmbed has grown well beyond dense text embedding.  This phase
wires every useful capability into CrispSorter: layout-aware
extraction, multi-engine OCR, math OCR, face detection/recognition,
cross-modal omnimodal embeddings, decoder-model embeddings, and
standalone ViT image search.  All gated behind `--features crispembed`
(or the Metal/Vulkan/CUDA sub-features).  Each item ships with unit
tests (mock/stub, run in CI) **and** live tests (`#[ignore]`, require
GGUF models on disk).

- [x] **P17.1 — Layout-aware PDF extraction** ✅ SHIPPED.
  `extractors/layout.rs` (352 lines): `CrispLayout` RT-DETRv2, 17 region
  types, `detect_regions` + `order_regions_reading_order`, cached detector.

- [x] **P17.2 — CrispEmbed OCR engines** ✅ SHIPPED.
  `extractors/ocr_crispembed.rs`: Tier 4 OCR via `crispembed::OcrPipeline`
  + `CrispOcrPipeline`. All 13 engines (dbnet_trocr through qwen3vl).

- [x] **P17.3 — Math OCR** ✅ SHIPPED.
  `extractors/math_ocr.rs` (303 lines): `crispembed::MathOcr`,
  PP-FormulaNet-L + PosFormer, LaTeX output.

- [x] **P17.4 — Face detection** ✅ SHIPPED.
  `images/face.rs` (175 lines): `CrispFace` YuNet/SCRFD, `detect_faces` /
  `count_faces`. Detection only (no recognition — EU AI Act).

- [x] **P17.5 — BidirLM-Omni cross-modal embeddings** ✅ SHIPPED.
  `index/omni_embed.rs` (357 lines): `encode_text_omni`, `encode_image_omni`,
  `encode_audio_omni`, `encode_text_with_image_omni`, `omni_similarity`.
  2048-D shared space.

- [x] **P17.6 — Decoder embeddings** ✅ SHIPPED.
  `EmbedderModel` variants: `Gemma3Embed2B` (2048d), `ModernBertBase` (768d),
  `ModernBertLarge` (1024d), `DebertaV2Xlarge` (1536d), `NomicBertMoe` (768d).
  GGUF-only, CrispEmbed backend.

- [x] **P17.7 — Standalone ViT image embeddings** ✅ SHIPPED.
  `images/vit_embed.rs` (224 lines): `CrispVit` SigLIP/CLIP, `embed_image_vit`
  Tauri command, `encode_file`.

### P18 — cb-api semantic search: model selection, license compliance, CI/release fixes (2026-06-13)

Work toward populating cb-api's dormant `documents.embedding` (NULL
corpus-wide → `/api/v2/index/search` is FTS-only) so the wallabag corpus
becomes semantically searchable, plus the compliance + pipeline fixes
that surfaced along the way.

- [ ] **cb-api server-side embedding backfill (PIXIE-Rune-v1.0).** Empirical
  CrispEmbed-C++ benchmark on the real DE/EN wallabag corpus (M1 clean +
  VPS) picked **PIXIE-Rune-v1.0** (XLM-R, Apache-2.0, 1024d) over Octen-0.6B
  / bge-m3 / arctic-v2: best German+English self-retrieval (MRR@10 0.83) AND
  ~3× faster than the 0.6B decoder, lowest RAM, no schema change. Plan:
  `crispembed-server` sidecar (PIXIE-Rune GGUF, loopback) on the VPS + rewire
  cb-api `api/embed.py::embed_text` to it (same engine as the client →
  vector parity), + a resumable `scripts/backfill_embeddings.py`
  (`merge_insert` writeback). Staged on the VPS; **activation deferred until
  the box has RAM/CPU headroom** (it was OOM/contended during this session).
- [x] **Model license-consent gate.** ✅ Verified on current `main` (2026-06-20).
  `license_consent::ensure()` enforced in `Embedder::new`, `Reranker::load`,
  and `NerHandle::load_inner` before any download. Covers Jina v3/v5 (CC-BY-NC),
  EmbeddingGemma (Gemma Terms), sauerkraut-gliner-lfm + LFM2.5 models (LFM
  Open License v1.0). Registry-name overrides also gated via
  `license_for_registry_name()`. 3 unit tests green. CLI `--accept-license` +
  GUI consent dialog + `CRISPSORTER_ACCEPT_MODEL_LICENSE` env var.
- [x] **CI license-scan hardened** against transient crates.io-index TLS
  flakes (`SSL_ERROR_SYSCALL`): `CARGO_NET_RETRY` + `CARGO_HTTP_MULTIPLEXING=false`
  + index warm-up retry before `licenses:gen`. The non-trivial-count
  assertion was silently tripping when `cargo metadata` flaked.
- [x] **Release build fixed — `desktop` feature missing.** P17 gated
  `tauri-plugin-shell` (+ notify/process/mistralrs/native-tls) behind the
  `desktop` feature (`default = []`), but `release.yml`'s desktop builds
  didn't pass `desktop`, so `capabilities/default.json`'s `shell:default`
  failed to resolve in `build.rs` → **every v0.5.0 desktop build failed and
  an empty release was published** (the `if: always()` publish gotcha). All
  three desktop `tauri_args` now include `desktop`.
- [x] **Re-release v0.5.0.** ✅ DONE (2026-06-20). The CI re-ran after the
  `desktop`-feature fix and populated the draft with full assets (dmg, deb,
  apk, windows portable, app.tar.gz). Published the draft as a non-latest
  release (v0.6.0 remains Latest).

### P19 — Further CrispEmbed integration (v0.11.8 pinned; HEAD is v0.11.8+114)

P17 already wired most of CrispEmbed's surface (dense / sparse / ColBERT /
rerank / `MathOcr` / `OcrPipeline` / `CrispVit` / `CrispLayout` / `CrispFace` /
omni image+text). CrispEmbed HEAD (unreleased) adds **Qwen3-VL-2B** (engine 12,
DeepStack injection, fused attention, KV cache fast path), **PaddleOCR-VL**
(NaViT + ERNIE-4.5, 109 langs, SOTA 96.3% OmniDocBench, Apache-2.0),
**FireRed-OCR** (Qwen3-VL fine-tune, tables+LaTeX), **SmolDocling** (SigLIP +
SmolLM2-135M, 256M, Apache-2.0, outputs DocTags), **LFM2.5-Embedding/ColBERT**
(350M, 1024d/128d, LFM-1.0 restricted), and major **DeepSeek-OCR-2 performance
improvements** (9 min → ~23 s via Metal acceleration, -655 MB memory via per-row
embedding dequant). Remaining gaps:

- [x] **⭐ GLiNER NER → entity tags + facets** (`crispembed::CrispNER`). ✅ SHIPPED
  (2026-06-13). New `index::ner` module — `NerModel` enum
  (`sauerkraut-gliner-lfm` German-tuned default + `gliner-deberta` Apache-2.0
  alt), `NerHandle` cheap-clone lazy-loader mirroring `RerankerHandle` (GGUF
  download via hf-hub, license gate at load, soft-fail to empty tags,
  `crispembed`-gated no-op stub otherwise). At index time
  `ingest_documents_batch` runs NER once per document on the (truncated)
  `full_text` and merges deduped/capped `"<label>:<text>"` tags
  (`person:…`/`org:…`/`loc:…`/`date:…`) into `RawDocument.tags` before rows are
  built — so the existing tag-cloud sidebar, `array_has(tags,…)` filter,
  `index search --tag`, and federated `--tag` light up with **zero schema
  change**. Opt-in via `IndexConfig.ner_{enabled,model,labels,threshold,
  max_entities,max_chars}` (default off). Sauerkraut-LFM (LFM Open License
  v1.0) routed through `index::license_consent` (restricted → consent
  required); DeBERTa is permissive. Settings panel (toggle / model / labels /
  threshold / caps) + DE/EN i18n + license-consent dialog; opt-in
  `TagCloud groupEntities` view groups namespaced tags by label prefix. CLI
  `index ingest` + L3-reingest honour the persisted NER config. See
  [HISTORY.md](HISTORY.md) 2026-06-13 session log.
- [x] **Finish audio cross-modal search — ingest wiring.** ✅ SHIPPED (2026-06-20,
  commit `9eec70e`). `ExtractedDocument.audio_pcm` surfaces decoded 16 kHz mono
  PCM from the audio extractor. `bg_ingest` feeds it to `encode_audio_omni()`
  via `spawn_blocking`, storing the 2048-D embedding in `embedding_omni`.
  Audio/video files now get omni cross-modal embeddings at index time alongside
  their text transcription. Search-side query (ANN over `embedding_omni`) is
  still pending — tracked in the omni/vit search channel prompt.
- [x] **Expose all OCR Tier-4 variants.** `OcrPipeline` is integrated; all 13
  engines (dbnet_trocr, surya, got, glm, qwen2vl, internvl2, tesseract, parseq,
  deepseek_ocr2, pix2struct, granite_vision, lightonocr, **qwen3vl**) are now
  wired in `engine_id()`, the CLI `--engine` value_parser, and the Settings UI
  stage-builder dropdown.  PARSeq correctly treated as det+rec (not VLM) in
  `isVlmEngine`.  The user-configurable model overrides (`det_model`/`rec_model`
  or VLM single-model) route through to CrispEmbed's model registry, so
  PaddleOCR-VL (`paddleocr-vl`), FireRed-OCR (`firered-ocr`), Nanonets-OCR
  (`nanonets-ocr-s`), and H2OVL (`h2ovl-2b`) are usable as model overrides on
  the matching engine (qwen2vl/internvl2).  **Qwen3-VL requires a CrispEmbed
  release > v0.11.8** (engine 12 is on HEAD, untagged).
- [x] **(Minor)** `rerank_biencoder` ✅ SHIPPED — `IndexConfig.use_embedder_as_reranker`
  toggle in Settings, wired through `SearchEngine.with_embedder_as_reranker()`
  → `maybe_rerank()` fallback. Cosine-similarity re-scoring via the loaded
  dense embedder when no dedicated cross-encoder GGUF is installed.
- [x] **(Minor)** `encode_tokens` for token-level match highlighting. ✅ SHIPPED.
  `Embedder::encode_tokens(text) -> Vec<(String, Vec<f32>)>` delegates to
  `CrispEmbedBackend::encode_tokens` (per-subword contextual embeddings).
  New `index/token_highlight.rs` module: `highlight_tokens()` computes
  per-token cosine similarity between query and document tokens, returns
  `TokenSpan { offset, length, score }` above a configurable threshold;
  `merge_spans()` coalesces adjacent/overlapping spans.  Tauri command
  `index_token_highlight(query, doc_text, threshold)` registered.  4 unit
  tests (cosine, merge_adjacent, merge_overlapping).  ONNX path returns
  empty (GGUF-only).

### P20 — Configurable OCR pipeline (cleanup + engines + post-process)

A user-tweakable, C++-primary OCR pipeline driven by CrispEmbed's
`ocr_orchestrator` (see CrispEmbed PLAN). CrispSorter is the thin caller +
settings surface; pipeline logic lives in CrispEmbed C++.

- [x] **⭐ Configurable OCR pipeline + full per-stage builder** ✅ SHIPPED
  (2026-06-15, CRISPEMBED_REF v0.10.1). `IndexConfig`-adjacent
  `OcrPipelineConfig` (in `bg_ingest`, via `bg_ingest_set_ocr_pipeline`):
  master toggle, source-type **router** (screenshot / scanned-doc / photo),
  per-stage **cleanup** (deskew / crop / whiten / binarize+Sauvola + NAFNet
  denoise), engine choice, **engine params** (DBNet prob/box/short-side; VLM
  prompt/max-tokens), text-yield + confidence **accept-gate** with chain
  **escalation**, and an optional **post-OCR punct/spacing restore**
  (FireRedPunc / PCS). Engines: DBNet+TrOCR, Surya, **Tesseract LSTM**,
  GOT-OCR2, GLM-OCR, Qwen2.5-VL, InternVL2. Two modes: *simple* (flat toggles
  → `CrispOcrPipeline::new`) and *advanced* (`stages[]` → `from_stages`); empty
  `stages` = simple, backward-compatible. Default OFF → legacy Rust tier
  ladder unchanged. `extractors/ocr_crispembed::ocr_via_pipeline` +
  `build_pipeline`; `extractors/mod.rs` dispatch; Settings "Smart OCR Pipeline"
  panel incl. the per-stage **stage builder** (add/remove/reorder; per-stage
  engine + cleanup + params + gate) + DE/EN i18n. Tesseract recogniser
  defaults to `tesseract-eng`. See [HISTORY.md](HISTORY.md) 2026-06-15.
- [x] **⭐ Multi-page + layout pipeline** ✅ SHIPPED (2026-06-15). Three slices
  over a shared page-sourcing spine (`extractors/page_source.rs`):
  - **Slice 1 — multi-page TIFF.** `rasterize_pages` splits a multi-frame TIFF
    into per-frame temp PNGs (pure-Rust `tiff` crate; Gray8/RGB8/RGBA8); single
    frame → original path (zero-copy). The image arm loops pages, OCRs each via
    `ocr_image_page`, joins with a form-feed `PAGE_SEPARATOR`.
  - **Slice 2 — PDF rasterization.** `rasterize_pdf` (cargo feature `pdf-render`,
    PDFium bound at runtime via `pdfium-render`; co-located lib → system lib)
    renders each page at ~200 DPI → PNG. The empty-text-PDF arm rasterizes +
    per-page OCRs, falling back to the legacy whole-file tesseract shell-out
    when no rasterizer is present.
  - **Slice 3 — layout-aware reading order.** Optional pass (`OcrPipelineConfig
    .layout`): CrispEmbed RT-DETRv2 (`extractors/layout.rs`) detects regions,
    orders them top-to-bottom / left-to-right (column-aware), then OCRs each in
    reading order — text→engine, formula→math OCR, figure/table skipped,
    header/footer optionally dropped (`drop_headers_footers`). `layout_threshold`
    knob. Process-wide cached detector (`LAYOUT_DET`). Falls back to whole-page
    OCR when no regions are found. Settings panel + DE/EN i18n. Needs
    `crispembed`; off by default.
- [x] **Multi-page release wiring** ✅ (2026-06-15). `pdf-render` is now in the
  release build features (all 3 native platforms); a per-platform `libpdfium`
  (bblanchon/pdfium-binaries `latest`) is staged into `src-tauri/bin/` →
  bundled via the existing `bin/*` resource glob into `resources/bin/`.
  `rasterize_pdf` searches the standard bundle locations relative to the exe
  (exe dir, `resources/bin`, macOS `../Resources/...`/`../Frameworks`, `../lib`)
  then the system lib. The PDF arm degrades gracefully (legacy tesseract
  fallback) if the lib is absent, so a bundling miss is a soft-fail. Live test
  `page_source::pdf_rasterize_live` (`$CS_TEST_PDF`, `--features pdf-render`).
  ⚠️ The actual bundle placement + runtime load is validated by a release CI
  run (cannot be checked locally); the soft-fall-back bounds the risk.
- [x] **CLI parity** ✅ (2026-06-15). New top-level `crispsorter ocr <FILE>`
  command: ad-hoc OCR of a single image/PDF, printing recognized text
  (`-f json` envelope or `-f text`). Flags map onto the full pipeline —
  primary `--engine` (dbnet_trocr / surya / tesseract / got / glm / qwen2vl /
  internvl2) + `--det-model`/`--rec-model`, pre-processors `--cleanup`
  (on/off), `--denoise` (+`--nafnet-model`), `--layout` (+`--layout-threshold`,
  `--drop-headers-footers`), post-processor `--punct-model`, accept-gate
  `--min-chars`/`--min-confidence`, and `--source-type` routing. dbnet_trocr
  uses simple mode; other engines build a single explicit stage so the choice
  takes effect. Forces OCR on text-layer PDFs (`ocr_pdf_min_chars = usize::MAX`).
  `cli/mod.rs::cmd_ocr`. (CrispEmbed's `crispembed --ocr-pipeline` is the
  lower-level C++ lever.)
- [x] **Tests** ✅ (2026-06-15). `engine_id`/`source_type_id` mapping +
  `OcrPipelineConfig` serde (incl. layout fields) + `OcrCleanupSpec` defaults
  are covered in `extractors::ocr_pipeline_tests`; opt-in live E2E
  `ocr_crispembed::ocr_pipeline_live_simple` / `_tesseract_stage` +
  `page_source::pdf_rasterize_live` exercise the FFI / engine / raster paths.
  The live `crispembed-metal` path validates in CI on the v0.10.1 re-pin.
- [x] **OCR structured/searchable output (`ocr_render`)** ✅ hOCR/ALTO SHIPPED
  (2026-06-15). CrispEmbed shipped the renderers + `crispembed::ocr_render`
  Rust binding (+ registered punct models, closing that follow-up); I fixed an
  upstream compile blocker in the binding (undefined `OcrRegion` → alias of
  `OcrResult`; missing `libc` dep — CrispEmbed `848071a`) and adapted to the new
  `CrispOcrPipeline::new` vlm params. CrispSorter wiring:
  `extractors/ocr_render.rs` (`OcrOutputFormat`, `OcrRegion`, `RenderPage`, Rust
  text renderer + `render_structured` → `crispembed::ocr_render`),
  `ocr_crispembed::ocr_regions_via_pipeline` (box+text+confidence from the
  cached orchestrator), and `ocr --render text|hocr|alto|pdf [--out F]`
  (multi-page aware via `ocr_render_pages`). text + **hOCR + ALTO** work under
  the `crispembed` feature; rendering stays in C++ per "keep it all in cpp".
  Live test `ocr_render::hocr_render_live`.
- [x] **Searchable PDF + single-document multi-page** ✅ (2026-06-15). Bound the
  lower-level `ocr_render.h` API in Rust (CrispEmbed `35a484b`:
  `crispembed::ocr_render_pages` over `create/begin/add_page*/end/output_size`)
  — binary-safe (PDF via `output_size` → `Vec<u8>`) and multi-page (one document
  across all pages). CrispSorter `render_structured` now uses it for hOCR / ALTO
  / **PDF**; PDF un-gated, multi-page no longer concatenated. Live tests
  `ocr_render::hocr_render_live` + `pdf_render_live`. No C++ changes (symbols
  were already `extern "C"`).
- [ ] **Future — cc_detect + classical_preproc.** CrispEmbed landed a
  model-free CC line detector + adaptive-Otsu/deskew/despeckle classical
  preproc. They integrate as orchestrator detector/cleanup options once the
  parallel agent wires them into the orchestrator; CrispSorter then exposes them
  via the existing `OcrPipelineConfig` surface (detector choice / cleanup
  methods) with minimal new code.

---

## UX gaps after v107 — the wallabag corpus is searchable end-to-end, but only via the federated CLI

A fresh-eye review surfaced one big asymmetry: we built the
infrastructure (storage tiers, schema, wire, scripts, tests) all the
way through but the *desktop UX on top of it* lags meaningfully
behind.  The verification proved it — `crispsorter index search`
errors with `FTS index not found` on freshly pulled wallabag rows
because pulled L1 chunks (manifest-only, chunk_index = -1, no
embeddings) don't go through the extract-and-embed pipeline that
populates Tantivy.  So the user can pull 50 K articles into the
local LanceDB, see them in the file's bytes, but can't actually
search them locally — only via the federated CLI against the live
cb-api tunnel.

This roadmap closes that gap, in priority order.

### Tier 1 — ✅ SHIPPED in v0.3.0 (2026-05-29)

All three Tier-1 gaps are closed.  Full spec → [HISTORY.md](HISTORY.md)
2026-05-29 session log + `RELEASE_NOTES_v0.3.0.md`.

- [x] **L1-aware local search** — `sync cloud-backup pull` writes each
  pulled L1 chunk into local Tantivy in the same pass it writes
  LanceDB (delete-then-add by `doc_id`, soft-fails on unwritable FTS).
  Pulled rows ingest with `chunk_index = 0, chunk_total = 1`.
- [x] **Unified `crispsorter search` verb** — local + cb-api v2 hybrid,
  RRF-merged, source-badged; `--local-only` / `--cloud-only`; shared
  filters push down on the cb-api leg.
- [x] **Click-to-open-source + snippets** — `index/snippet.rs::highlight_snippet`
  (`<mark>`-wrapped ~300-char window) + "Open original" globe button in
  `IndexSearch.svelte`.

### Tier 2 — smaller gaps worth closing soon

- [x] **`--tag` flag on local `crispsorter index search`** — shipped
  in v0.3.0; emits `array_has(tags, '<value>')` on Lance.
- [x] **Tag-cloud sidebar in Übersicht.** ✅ SHIPPED — opt-in
  (default-hidden) tag cloud in the Übersicht browse.  Backend:
  `DocumentFilter.tags` (AND semantics → `array_has(tags,'…')` per
  tag in `filter_to_sql`), `LocalIndex::tag_facets` + `index_tag_facets`
  command (counts ignore the filter's own tag selection; skips
  `collection:` markers).  Frontend: reusable `TagCloud.svelte`
  (count-weighted clickable chips, built so the search-results view
  can reuse it) mounted behind a default-off "Tags" toggle in
  `IndexIngest.svelte`.  Tests: `tag_facets_counts_and_skips_markers`
  + `tag_filter_is_and_semantics` green.  Also mounted on **both** the local
  search-results pane and the **federated pane** (`IndexSearch.svelte`) —
  same `TagCloud`, default-hidden, facets computed client-side from the
  hits on screen (per-document/per-hit counts), AND-narrows the displayed
  results.  The federated wire (`FederatedHit`) already carried `tags`
  for local + cloud_backup hits (CrispLens carries none), so that pane
  needed zero Rust changes.  *Follow-up:* URL/settings persistence of the
  selected tags across launches.
- [ ] **Server-side embeddings shipped with pulls** so the local
  embedder doesn't have to re-run.  ⚠️ **Blocked upstream:** a
  2026-05-31 live audit found the cb-api `documents.embedding`
  column is **NULL across the corpus** — the server vector arm is
  dormant and `/api/v2/index/search` runs FTS-only
  (`used_vector:false`).  So today cb-api does *not* compute/store
  vectors for the bulk corpus; the prerequisite is a server-side
  embed-backfill (tracked in the cloud-backup PLAN).  Once vectors
  exist server-side: ship them via the wire so pulled rows are
  vector-searchable locally without a re-embed pass.  Requires
  embedding-model-name reconciliation between the two stores
  (same model = same vector).
- [x] **`/api/search` returns a `snippet` field** — ✅ DONE
  (cloud-backup `feat/api-search-snippet`).  `SearchHit.snippet` is a
  match-centred `<mark>`-highlighted window (FTS5 `snippet()` on the
  SQLite path; `_make_snippet` window on the Lance path).
  `?include_full_text=false` omits the body for the ~100× payload cut,
  defaulting true so the L1-ingest-from-hit flow is unaffected.  Tests:
  2 e2e + 5 unit; full non-live suite 210 green.  **Live-verified on the
  production VPS (Lance path).**  Client side (CrispSorter) now closes the
  loop: `CloudBackupClient::search` takes an `include_full_text` flag —
  the federated legs (GUI `sync_federated_search` + CLI) request
  `false` and render the server `snippet` (strip `<mark>`, re-highlight
  via the XSS-safe `highlightSnippet`); `sync_cb_search` keeps `true`
  (its hits feed L1 ingest).  `cloud_backup` wire tests 39 green; npm
  check clean.
- [x] **v1 `/api/search` url + tags audit** — ✅ DONE (already shipped).
  Both the Lance and SQLite SELECTs in `/api/search` already pull
  `fr.url` + `fr.tags` and decode them onto `SearchHit`; the stale
  "FTS5 SELECT may not surface tags" note predated that fix.  Confirmed
  during the snippet work.

### Tier 3 — cool but probably overkill until someone asks

- [x] **Cross-corpus deduplication by canonical URL** — ✅ SHIPPED (2026-06-05). `index_url_duplicates` Tauri command + CLI `crispsorter index url-duplicates` + frontend "URL-Duplikate" button in Übersicht overview tab.  Groups documents by `url` column (v106+), returns `UrlDuplicateGroup` with items.  i18n keys added (EN+DE).  Deletion/merge actions pending.
- [x] **LLM-suggested topical clustering** ✅ SHIPPED (2026-07-04).
  `index_label_clusters` Tauri command — sends cluster top terms +
  sample titles to the configured LLM (OpenAI-compatible API) and
  returns human-readable labels.  "AI Label" button in Dashboard.
  Enhances the existing K-means++ clustering from P24.1.
- [ ] **Vector embeddings for the wallabag bodies** — once #1
  lands, the natural next step is semantic search ("articles about
  how schools handle bullying") via the existing embedder backed
  by cb-api's chunks-and-bodies storage.  (Same upstream blocker as
  Tier 2 above: the cb-api `embedding` column is currently
  unpopulated — confirmed live 2026-05-31.)

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)

---

## CLI ↔ GUI feature parity audit (2026-06-20)

**CLI surface:** 15 top-level commands, ~78 subcommands (see `cli/mod.rs`).
**GUI surface:** 204 Tauri commands across 19 functional areas.

### Implemented but unreachable from both CLI and GUI

*(None — all modules are now wired.)*

### CLI-only (no GUI equivalent)

*(None — all CLI commands now have GUI equivalents.)*

### GUI-only (no CLI equivalent)

*(Inherently visual or already have functional CLI equivalents:)*

- OCR Workbench interactive correction — inherently visual, no CLI equivalent possible
- Background ingest controls / durable job queue — CLI `index ingest` handles headless ingest
- LLM sidecars — CLI `chat query/transcribe/tts` provide headless equivalents
- `index_image_promote_l3` / `index_audio_promote_l3` — CLI `index promote-l3 <doc_id>` already handles all file types (explicitly mirrors both; see cli/mod.rs line 2030)

### P21 round 3 — vit_embed + omni_embed revived (2026-06-20)

- [x] **Schema migration v108 — `embedding_omni` (2048-D).**
  `AddOmniEmbedding` migration adds `FixedSizeList<Float32, 2048>`
  column to the LanceDB documents table.  Idempotent.  Backfills
  existing rows with NULLs.

- [x] **Schema migration v109 — `embedding_vit` (768-D).**
  `AddVitEmbedding` migration adds `FixedSizeList<Float32, 768>`
  column.  Same pattern as v108.

- [x] **`DocumentChunk` + `RawDocument` + `build_schema()` extended.**
  Both structs gain `embedding_omni: Option<Vec<f32>>` and
  `embedding_vit: Option<Vec<f32>>` fields (`#[serde(skip)]`).
  Arrow schema in `build_schema()` gains matching FixedSizeList
  columns.  `chunks_to_record_batch` serialises them.  All 13
  `DocumentChunk` and 20 `RawDocument` construction sites updated.

- [x] **`bg_ingest` computes embeddings at ingest time.**
  After text extraction, `ingest_one` conditionally runs:
  - `vit_embed::embed_image()` for image files (768-D SigLIP/CLIP)
  - `omni_embed::encode_image_omni()` for image files (2048-D)
  - `omni_embed::encode_audio_omni()` for audio/video files (2048-D)
    — via `ExtractedDocument.audio_pcm` (commit `9eec70e`)
  All via `spawn_blocking`; soft-fail (log + continue with None).

- [x] **`images/vit_embed.rs` no longer dead.**  Called from bg_ingest
  for image files; embeddings stored in the `embedding_vit` column.

- [x] **`index/omni_embed.rs` no longer dead.**  Called from bg_ingest
  for image files; embeddings stored in the `embedding_omni` column.

### Still pending (follow-up work)

| Item | Effort | Handover prompt | Notes |
|------|--------|-----------------|-------|
| ~~Audio omni embedding~~ | ~~4 h~~ | ~~`session-prompt-audio-omni-embedding.md`~~ | ✅ DONE `9eec70e` |
| ~~Omni/ViT RRF search channel~~ | ~~6 h~~ | ~~`session-prompt-omni-vit-search-channel.md`~~ | ✅ DONE `a0ecdee` |
| ~~Cross-modal search UI~~ | ~~2 h~~ | ~~`session-prompt-cross-modal-search-ui.md`~~ | ✅ DONE `a0ecdee` |

### P21 — CLI ↔ GUI parity gap closure (2026-06-20)

Closed 8 parity gaps in one session:

- [x] **`doctor_check` Tauri command + Settings Diagnostics panel.**
  `lib.rs::doctor_check` — returns the same JSON as `cli::cmd_doctor`
  (tesseract, ocrs, PaddleOCR, CrispEmbed, face detection, embedder
  model cache, LanceDB dir).  Settings sidebar gains a "Diagnostics"
  entry with a "Run Diagnostics" button → checklist with green/red
  indicators + LanceDB path.

- [x] **KIE + Table tools wired into OcrWorkbench.**
  `OcrWorkbench.svelte` now invokes the existing `tool_kie_extract`
  and `tool_table_extract` Tauri commands that were registered but
  uncalled.  KIE section: comma-separated label input → extract button →
  results table (label / value / score).  Table section: extract button →
  rendered HTML output with rows×cols count.

- [x] **CLI `watch` command.**  New `crispsorter watch <DIR>` subcommand
  (`cli/mod.rs::cmd_watch`).  Headless equivalent of the GUI folder
  watcher — prints new-file paths to stdout as they appear (same
  `notify` + debounce + extension filtering as `watcher/mod.rs`).
  Runs until Ctrl-C; `desktop` feature-gated (same as the GUI watcher).
  Pipe to `xargs -I{} crispsorter batch add "{}"` for auto-queue.

- [x] **CLI `index tag-facets` command.**  New `crispsorter index
  tag-facets [--limit N]` subcommand.  Calls `LocalIndex::tag_facets`
  and prints tag / count in JSON or columnar text.

- [x] **Advanced search filters in IndexSearch GUI.**  The filter panel
  (`IndexSearch.svelte`) now exposes: extension, language, year range,
  folder prefix, URL domain, tag, audio duration range, camera
  make/model, and ColBERT rerank toggle — 12 new inputs matching the
  full `SearchFilters` struct.

- [x] **Advanced filters on `sync_federated_search` Tauri command.**
  `sync/tauri_commands.rs::sync_federated_search` accepts 12 new
  optional parameters (`ext`, `lang`, `year_min/max`, `folder_prefix`,
  `url_domain`, `tag`, `audio_duration_min/max`, `image_camera_make/
  model`, `colbert_rerank`).  Builds a `SearchFilters` and passes it
  to `engine.search_hybrid()` instead of the empty default.

- [x] **Single-image face detection in GUI.**
  `images/tauri_commands.rs::images_detect_faces` — resolves a
  `location_uri` to a local path, runs `face::detect_faces` on a
  blocking thread, returns `Vec<FaceDetectionDto>`.  Registered in
  both handler lists.  `IndexIngest.svelte` image preview pane gains
  a "Detect Faces" button + count display.  Detection only (no
  biometric identification — EU AI Act compliant).

- [x] **`images/face.rs` + `ocr_paddle.rs::ocr_with_tables` no longer
  dead.**  Face detection is now reachable via the GUI's
  `images_detect_faces` command; table-enhanced PaddleOCR is reachable
  via the `tool_table_extract` Tauri command + OcrWorkbench UI.

- [x] **`doctor` no longer CLI-only.**  GUI equivalent via the Settings
  Diagnostics panel.

- [x] **File watcher no longer GUI-only.**  CLI `watch` provides the
  headless equivalent.

- [x] **`tool_kie_extract` + `tool_table_extract` no longer uncalled.**
  Both wired into the OcrWorkbench Svelte component.

- [x] **`sync cloud-backup admin {mint,revoke,list}` already had a GUI**
  — Settings panel (verified: `Settings.svelte` lines 1772–1816 call
  `sync_cb_admin_mint/revoke/list_keys`).  Removed from the CLI-only
  gap list.

**Round 2** — closed 12 more gaps:

- [x] **`index_purge` Tauri command + Settings UI.**
  `index/tauri_commands.rs::index_purge(max_size, dry_run)` wraps
  `LocalIndex::purge_to_size`.  Settings → Diagnostics gains a Purge
  section with size input, dry-run checkbox, and result display.

- [x] **`index_skip_all_failed` Tauri command + Settings UI.**
  Marks all retryable extraction failures as permanently skipped
  (reason `"unsupported"`).  Settings gains "Skip All Failed" +
  "Retry All Failed" buttons (the latter wraps the existing
  `index_retry_all_failed` command).

- [x] **`index_l1_only_scan` Tauri command + Settings UI.**
  Walks a directory, computes SHA-256 per file, writes thin L1 rows
  via `IngestPipeline`.  Settings gains an "L1 Scan Folder" button
  with a folder picker dialog, showing scanned/written counts.

- [x] **`sync_cb_restore_shard` Tauri command + Settings UI.**
  Restores a shard backup from a cloud drive to the cb-api VPS.
  Settings cloud-backup section gains a "Restore Shard" form (prefix,
  drive ID, optional date).

- [x] **CLI `math-ocr` command.**  New top-level `crispsorter math-ocr
  <FILE> [--model NAME]`.  Recognizes mathematical formulas in images
  and prints LaTeX to stdout.  Uses `extractors/math_ocr.rs`.

- [x] **`tool_math_ocr` Tauri command + OcrWorkbench UI.**
  Exposes `math_ocr::recognize_formula` as a Tauri command.
  OcrWorkbench gains a "Math OCR (LaTeX)" section with a recognize
  button and LaTeX output display.

- [x] **CLI `index mount-cidx` command.**  Opens a `.cidx` offline
  archive and prints doc/chunk counts + FTS availability.

- [x] **CLI `index search-cidx` command.**  Opens a `.cidx`, runs FTS
  search, prints results.  Standalone alternative to the GUI mount +
  browse workflow.

- [x] **CLI `index list-models` command.**  Lists CrispEmbed model
  registry entries (name, description, filename, size).  Gated behind
  `--features crispembed`.

- [x] **`sync cloud-backup partition/backup-shards/import-from-manifest-db`
  already had GUI equivalents** — Settings cloud-backup section calls
  `sync_cb_partition`, `sync_cb_backup_shards`,
  `sync_cb_import_from_manifest_db`.  Removed from the CLI-only gap list.

- [x] **`extractors/math_ocr.rs` no longer dead.**  Standalone formula
  recognition now reachable via `crispsorter math-ocr` CLI + `tool_math_ocr`
  Tauri command + OcrWorkbench UI.

- [x] **Embedder registry no longer GUI-only.**  CLI `index list-models`
  provides the headless equivalent.

- [x] **`.cidx mount/unmount` no longer GUI-only.**  CLI `index
  mount-cidx` + `index search-cidx` provide headless equivalents.

**Audit corrections** (round 2):

- [x] **`index_image_promote_l3` / `index_audio_promote_l3` already
  had a CLI equivalent** — `index promote-l3 <doc_id>` explicitly
  mirrors both (see comment at cli/mod.rs:2030).  Removed from
  GUI-only gap list.
- [x] **Background ingest / job queue / LLM sidecars** are not parity
  gaps — CLI `index ingest`, `batch process/apply`, and `chat
  query/transcribe/tts` provide the headless equivalents.  The GUI
  variants add visual feedback (progress bars, streaming) that is
  inherently non-CLI.

**Final status:** All closable CLI↔GUI parity gaps are resolved.
`vit_embed.rs` and `omni_embed.rs` are fully wired into bg_ingest
(images + audio) **and** the search pipeline:
- `search_hybrid()` gains a 4th omni channel (text→image/audio) via
  `SearchFilters.omni_search` + `LocalIndex::search_vector_column()`
- `SearchEngine::search_by_image()` + `index_search_by_image` Tauri
  command + CLI `index search --image <PATH>`
- Frontend "Search by Image" button + "Omni cross-modal" filter checkbox
The full ingest→search pipeline for cross-modal embeddings is complete.

### P22 — Search UX & Discovery (2026-06-21)

Five features that close the gap between CrispSorter's strong backend
infrastructure and the daily-use UX that professional desktop-search
and knowledge-management tools have long standardised.

- [x] **P22.1 — Saved Searches.**  Persist named query+filter combos
  (store key `savedSearches` → `Vec<SavedSearch>`).  Tauri commands:
  `index_save_search`, `index_list_saved_searches`,
  `index_delete_saved_search`.  Frontend: bookmark button in the
  search bar saves the current query+mode+filters; sidebar list loads
  any saved search with one click; trash icon deletes.  DE/EN i18n.

- [x] **P22.2 — "More Like This" (similar-document discovery).**
  `index_find_similar(doc_id, limit)` Tauri command: looks up the
  document's dense embedding from LanceDB, runs ANN excluding the
  source doc, returns `Vec<SearchResult>`.  Frontend: "Find similar"
  button on every search result row; results replace the current list
  with a "Similar to: <title>" header.

- [x] **P22.3 — Auto-Summary at ingest.**  Schema migration v110 adds
  a `summary` Utf8 column.  `bg_ingest` generates an extractive
  summary (first 2–3 sentences, cleaned) during the ingest pass and
  stores it.  `SearchResult.summary` is surfaced in the frontend;
  when present, the search-result card shows the summary above the
  BM25 snippet for a better at-a-glance experience.  Tauri command
  `index_generate_summary(doc_id)` for on-demand regeneration.

- [x] **P22.4 — Natural-Language → Filters.**  Deterministic
  heuristic parser (`index/nl_query.rs`) extracts structured intent
  from plain-text queries: year ranges (`from 2023`, `2020–2024`),
  language (`in German`/`auf Deutsch`), file types (`pdf files`,
  `.docx`), folder prefixes (`in /home/…`), tag filters
  (`tagged X`), and URL-domain filters (`from spiegel.de`).
  Returns a cleaned query string + a pre-populated `SearchFilters`.
  Tauri command `index_parse_nl_query`.  Frontend: "Smart search"
  toggle in the search bar; when on, the query is parsed before
  dispatch so the user can type `German PDFs about climate 2023-2024`
  and get the right filters automatically.

- [x] **P22.5 — Corpus Dashboard.**  `index_corpus_stats` Tauri
  command returns `CorpusStats`: total docs, total chunks, extension
  distribution, language distribution, top tags (NER entity
  breakdown), year histogram, and total indexed size.  Frontend:
  new Dashboard component accessible from the Catalog/Ingest tab,
  rendering the stats as a summary grid + bar/pie charts (pure
  CSS, no chart library).  DE/EN i18n.

### P23 — Search power features (2026-06-21)

Five features that bring CrispSorter's search to parity with
professional desktop-search tools: fuzzy matching for OCR-heavy
corpora, progressive result refinement, email indexing, a visual
document timeline, and a transparent result cache for instant
re-queries.

- [x] **P23.1 — Fuzzy / typo-tolerant search.**  `fuzzify_query()`
  in `fts_query.rs` auto-rewrites bare word terms with `~1` (1 edit
  distance).  Skips phrases, operators, wildcards, existing fuzzy
  markers, pure numbers, and words < 4 chars.  New `SearchFilters.fuzzy`
  boolean wired through `index_search`.  Frontend: "Fuzzy" checkbox in
  the advanced filters panel.

- [x] **P23.2 — Search-within-results (progressive refinement).**
  New `SearchFilters.doc_id_scope: Vec<String>` restricts search to a
  specified set of doc_ids (emits `doc_id IN (…)` in `to_lance_sql()`).
  Frontend: "Refine" button appears when ≥2 results are displayed;
  clicking it captures current result doc_ids into a scope, clears the
  query for the user to type a narrowing query, and runs the next
  search scoped to those docs.  Scope badge + clear button.

- [x] **P23.3 — Email (.eml) extraction.**  New `extractors/eml.rs`
  module.  Parses RFC 822 headers (From → author, Subject → title,
  Date → year, List-Id → tag) and body (text/plain preferred,
  text/html with tag-stripping fallback, multipart MIME boundary
  splitting).  Registered in `supported()` and the dispatch match.
  2 unit tests (plain + HTML body).

- [x] **P23.4 — Document timeline.**  The Corpus Dashboard's year
  histogram is now a full-width interactive timeline with clickable
  bars.  Selecting a year highlights the bar and shows a filter hint.
  Pure CSS, no chart library.

- [x] **P23.5 — Search result LRU cache.**  `index/result_cache.rs`:
  32-entry LRU cache keyed on `(query, mode, filters_hash)` with
  generation-based invalidation (global `AtomicU64` bumped on every
  `ingest_batch`).  Wired into `SearchEngine.search_hybrid` —
  cache-hit skips embedding + FTS + ANN entirely, returning cloned
  results.  3 unit tests (hit/miss, invalidation, LRU eviction).

### P24 — Discovery & clustering (planned)

- [x] **P24.1 — Topical clustering.**  ✅ SHIPPED (2026-07-02).
  K-means++ on dense embeddings with TF-IDF term-based cluster naming.
  `LocalIndex::cluster_documents(k)` fetches all embeddings, runs
  K-means++ (20 Lloyd iterations), names each cluster by top TF-IDF
  terms.  Tauri command `index_cluster_documents`.  CLI:
  `crispsorter index cluster --k 5`.  Frontend: CorpusDashboard
  "Topical Clusters" panel with k selector + cluster cards showing
  name, doc count, and sample titles.

- [x] **P24.2 — Search history panel.**  Persist last 50 queries in
  the Tauri plugin-store under key `searchHistory`.  Frontend: history
  sidebar toggled from the search bar — list of recent queries with
  timestamp, one-click re-run, swipe-delete.  Deduplication on
  (query, mode) — re-running the same search bumps it to the top.

- [x] **P24.3 — Knowledge graph visualization.**  ✅ SHIPPED
  (2026-07-02).  `index_entity_graph(min_cooccurrence, max_nodes)`
  Tauri command — fetches NER entity tags, builds co-occurrence
  matrix from per-document tag sets, returns `EntityGraph { nodes,
  edges }`.  Nodes carry label, group (person/org/loc/date), doc
  count; edges carry weight (co-occurrence count).  Follow-up:
  frontend force-directed graph panel in Dashboard.

- [x] **P24.4 — Synonym expansion.**  ✅ SHIPPED (2026-07-02).
  `index/synonyms.rs` — embedded EN (50 groups) + DE (44 groups)
  synonym lists.  `synonym_expand_query()` OR-expands bare terms
  before FTS dispatch.  Wired into `search_text` and `search_hybrid`
  via `SearchFilters.synonyms` flag.  Frontend: "Synonyms (EN+DE)"
  checkbox in advanced filters.  6 unit tests.

- [x] **P24.5 — RSS/Atom feed ingestion.**  ✅ SHIPPED (2026-07-02).
  `extractors/feed.rs` using `feed-rs` crate — parses RSS 2.0, Atom,
  and JSON Feed formats.  `parse_feed()` yields `FeedEntry` per item
  (title, author, year, body text with HTML stripping, source URL,
  tags/categories).  `fetch_and_parse()` async variant fetches from
  URL.  Tauri commands `feed_fetch_and_parse` + `feed_parse_file`.
  4 unit tests (RSS2 + Atom + HTML stripping).  Follow-up: Settings
  panel for feed URL management + poll timer + auto-ingest.

- [x] **P24.6 — Clipboard / screenshot capture.**  ✅ SHIPPED
  (2026-07-02).  `extractors/clipboard.rs` using `arboard` crate —
  `read_clipboard()` returns text or saves clipboard image to temp
  PNG.  `save_clipboard_image_to_temp()` for OCR pipeline feeding.
  Tauri commands `clipboard_capture` + `clipboard_save_image`.
  Follow-up: system-tray "Capture" action + auto-ingest into index.

### P25 — DMS & compliance parity (planned)

Features that close the gap with professional document management
systems and enterprise OCR suites.  CrispSorter already has the
extraction pipeline, search engine, and OCR stack — these items add
the workflow and compliance layers that enterprise tools charge
thousands for.

- [x] **P25.1 — Document versioning.**  ✅ SHIPPED (2026-07-02).
  `index/versioning.rs` — WAL-mode SQLite `versions.db`.
  `VersionStore::record_version()` assigns monotonic `version_seq`
  per `version_group_id` (SHA-256 of canonical path).
  `get_versions(doc_id|path)` returns the full history.
  Tauri commands: `version_record`, `version_history`,
  `version_current`.  2 unit tests.

- [x] **P25.2 — Audit trail / access log.**  ✅ SHIPPED (2026-07-02).
  `audit/mod.rs` — append-only WAL-mode SQLite `audit.db` with
  `audit_log(id, ts, action, doc_id, detail, user_agent)`.
  `AuditLog::log()` for writes; `query()` with filters (since,
  action, doc_id, limit, offset); `count()` + `action_summary()`.
  Tauri commands: `audit_log_event`, `audit_query`, `audit_count`,
  `audit_summary`.  Indexed on ts, action, doc_id.  2 unit tests.

- [x] **P25.3 — Retention policies.**  ✅ SHIPPED (2026-07-02).
  `index/retention.rs` — WAL-mode SQLite `retention.db`.
  Per-folder or per-tag rules with `archive_after_days` and
  `delete_after_days`.  `RetentionStore::evaluate_rules()` checks
  all enabled rules against document metadata and returns actions.
  Tauri commands: `retention_add_rule`, `retention_list_rules`,
  `retention_delete_rule`, `retention_set_enabled`.  3 unit tests
  (CRUD, archive by folder, delete by tag).  Follow-up: daily
  background worker + Settings UI for rule management.

- [x] **P25.4 — Stamp / watermark on export.**  ✅ SHIPPED (2026-07-02).
  `tool_ocr_export` Tauri command gains `stamp_text` parameter; CLI
  `crispsorter ocr --render pdf --stamp "CONFIDENTIAL"`.  Applies
  `pdf_ops::add_watermark` to the rendered PDF after OCR output.
  Also available standalone via `crispsorter pdf watermark`.

- [x] **P25.5 — Barcode / QR code detection at ingest.**  Detect
  1D barcodes (Code128, EAN-13) and QR codes in scanned documents
  using `rxing` (pure Rust, Apache-2.0).  Store decoded values as
  `barcode:<value>` tags — lights up in the existing tag cloud,
  `--tag barcode:…` filter, and NER entity view.  Enables automated
  document routing by barcode (pair with Stapel sort rules).
  **Follow-up: expand barcode coverage** — `rxing` already supports
  24+ symbologies (Code 39, Code 93, ITF, Codabar, PDF417, Data Matrix,
  Aztec, MaxiCode, RSS/GS1 DataBar, UPC-E) but only a subset is wired.
  Expose all supported types in detection + tag output.  ~2 h.

- [x] **P25.6 — Form field extraction → structured CSV export.**
  Chain the existing KIE pipeline (CrispEmbed GLiNER + LiLT) with a
  batch export: user defines a field schema (label list), runs KIE
  across a folder of scanned forms (invoices, receipts, applications),
  exports a CSV with one row per document and one column per label.
  `index_batch_kie(folder, labels) → Vec<KieRow>` Tauri command.
  Frontend: "Batch Extract" section in the OCR Workbench with schema
  editor, folder picker, progress bar, and CSV download.

- [x] **P25.7 — Side-by-side document comparison.**  ✅ SHIPPED
  (2026-07-02).  `index/comparison.rs` via `similar` crate — word-level
  text diff returning `DiffSegment` array (equal/insert/delete tags).
  `compare_texts()` for raw strings, `compare_documents()` for
  indexed doc_ids (fetches full_text from LanceDB).  Stats: word
  counts, added/removed, changed_ratio.  Tauri commands:
  `compare_documents`, `compare_texts_raw`.  7 unit tests.

- [x] **P25.8 — Annotation layer.**  ✅ SHIPPED (2026-07-02).
  `index/annotations.rs` — WAL-mode SQLite `annotations.db`.
  `annotations` table (doc_id, page, x, y, w, h, ann_type, text,
  color, created_at).  CRUD + search via LIKE on text.  Tauri
  commands: `annotation_add/list/update/delete/search`.  3 tests.

- [x] **P25.9 — Reading queue & highlights.**  ✅ SHIPPED (2026-07-02).
  Same `annotations.db` — `highlights` table (doc_id, chunk_index,
  start_offset, end_offset, text, note, color, created_at).
  `reading_list(limit, offset)` returns all highlights sorted by
  recency.  Tauri commands: `highlight_add/list/reading_list/
  update/delete/count`.  Shared tests in annotations module.

- [x] **P25.10 — .mbox / Outlook .msg email extraction.**  Extend
  P23.3's `.eml` extractor to handle `.mbox` (concatenated messages
  split on `From ` lines) and `.msg` (Microsoft CFBF compound binary
  via `cfb` crate — extract PR_SUBJECT, PR_SENDER_NAME,
  PR_CLIENT_SUBMIT_TIME, PR_BODY / PR_RTF_COMPRESSED → strip to
  plain text).  Recursive attachment extraction feeds back into the
  main dispatch (a PDF attached to an email gets the full PDF
  extractor treatment).

### P26 — Enterprise DMS parity (planned)

Features that close the remaining gaps against professional document
management systems and enterprise OCR/archival suites.

- [x] **P26.1 — Document-type classification at ingest.**  ✅ SHIPPED
  (2026-07-03).  `index/doctype.rs` — heuristic classifier based on
  file extension + text content patterns (invoice/receipt keywords,
  contract signals, letter/memo markers, form fields, report
  structure).  18 document types: letter, invoice, receipt, form,
  email, report, specification, presentation, spreadsheet, image,
  audio, video, ebook, code, article, contract, memo, unknown.
  Wired into bg_ingest — every document gets a `doctype:<class>`
  tag automatically.  11 unit tests.  Follow-up: ViT-based
  classifier for higher accuracy on scanned documents.

- [x] **P26.2 — Watched folder → auto-classify → auto-file.**  ✅
  SHIPPED (2026-07-04).  `WatchMode::AutoFile` enum variant +
  `watcher/auto_file.rs` module.  `SortRule` templates map doctypes
  to destination path patterns (e.g. invoices → `Invoices/{year}/`).
  `resolve_destination()` classifies via P26.1 doctype heuristic and
  builds the target path.  17 default rules covering all document
  types.  7 unit tests.

- [x] **P26.3 — Table → CSV/XLSX export.**  Extend
  `tool_table_extract`'s HTML table output with structured CSV and
  XLSX export.  CSV via `csv` crate (already in deps); XLSX via
  `rust_xlsxwriter` (MIT, ~3 MB).  CLI: `crispsorter ocr --table
  --export csv|xlsx`.  Frontend: "Export as CSV" / "Export as XLSX"
  buttons in the OcrWorkbench table section.

- [x] **P26.4 — Zoned OCR / template matching.**  ✅ SHIPPED
  (2026-07-04).  Slices 1–3 complete (store + engine + CLI/Tauri);
  Slice 4 (frontend) deferred.

  **Goal:** User-defined extraction zones on a document template.
  Draw rectangles on a reference page, name each zone (e.g.
  "invoice_number", "total_amount"), save as a named template.  On
  demand, apply a template to a document image: crop each zone,
  OCR the crop, return structured `{label: text}` pairs.

  **Architecture — 4 slices:**

  **Slice 1 — Template store (`index/templates.rs`).**
  WAL-mode SQLite `templates.db` (same pattern as audit, retention,
  annotations).  Tables:
  ```
  templates (id PK, name TEXT UNIQUE, width INT, height INT, created_at INT)
  template_zones (id PK, template_id FK, label TEXT, x REAL, y REAL,
                  w REAL, h REAL)
  ```
  `TemplateStore` struct with CRUD: `create_template(name, w, h) → id`,
  `add_zone(template_id, label, x, y, w, h) → zone_id`,
  `get_template(id) → Template { zones }`, `list_templates`,
  `delete_template(id)`.  Coordinates are normalised 0.0–1.0
  (fraction of page width/height) so the same template works across
  DPI variants.  6+ unit tests (CRUD, duplicate name, delete cascade).

  **Slice 2 — Zone extraction engine (`index/zone_ocr.rs`).**
  `extract_zones(image_path, template) → Vec<ZoneResult>` where
  `ZoneResult { label, text, confidence }`.  For each zone:
  1. Load image via `image` crate (`open` → `DynamicImage`).
  2. Denormalise zone coords to pixel rect.
  3. `crop_imm(x, y, w, h)` → write crop to a temp PNG.
  4. OCR the crop via the existing `ocr_one_image` path (or
     `ocr_via_pipeline` when the smart pipeline is active).
  5. Collect `(label, text, confidence)`.
  Returns empty text for zones that fall outside the image bounds
  (soft-fail, not panic).  4+ unit tests (mock image, out-of-bounds
  zone, empty template).

  **Slice 3 — Tauri commands + CLI.**
  Tauri commands: `template_create`, `template_add_zone`,
  `template_list`, `template_get`, `template_delete`,
  `template_apply(location_uri, template_id) → Vec<ZoneResult>`.
  CLI: `crispsorter ocr zone --template <name> <FILE>`.
  All registered in both desktop + mobile handler lists.

  **Slice 4 — Frontend (Settings + OcrWorkbench).**
  Settings → "Templates" panel: create template (name + ref width/height),
  add zones (label + normalised rect), delete template/zone.  Backed by
  `template_create`, `template_add_zone`, `template_list`, `template_get`,
  `template_delete` Tauri commands.  OcrWorkbench → "Apply Template"
  section: dropdown of templates, "Apply" button → calls `template_apply`,
  results table (label | text | confidence).  DE/EN i18n keys.
  Follow-up: drag-to-draw zones on the page preview canvas.

- [x] **P26.5 — PDF/A archival conversion.**  ✅ SHIPPED (2026-07-02).
  `pdf_ops::convert_to_pdfa()` adds PDF/A-2b conformance metadata
  (XMP `pdfaid:part=2 conformance=B`, sRGB OutputIntent, PDF 1.7
  version).  Tauri command `pdf_convert_pdfa`.  CLI:
  `crispsorter pdf pdfa --out archival.pdf`.  Also available via
  existing `ocr --render pdf --pdfa` for OCR output.

- [x] **P26.6 — Digital signature detection.**  ✅ SHIPPED (2026-07-02).
  `pdf_ops::detect_signatures()` walks PDF annotation widgets for
  `/FT /Sig` fields, extracts signer name, reason, location, date,
  filter, sub-filter, ByteRange presence.  Falls back to AcroForm
  `/SigFlags`.  Tauri command `pdf_detect_signatures`.  CLI:
  `crispsorter pdf signatures`.  Cryptographic verification (PKCS#7)
  deferred — needs a CMS crate.

- [x] **P26.7 — Bulk PII redaction.**  ✅ SHIPPED (2026-07-02).
  `pdf_ops::redact_regions()` overlays black rectangles on specified
  page regions.  `pdf_ops::redact_text_patterns()` redacts matching
  strings in /Info metadata.  Tauri commands: `pdf_redact_regions`,
  `pdf_redact_text`.  CLI: `crispsorter pdf redact --patterns
  "name,address" --out redacted.pdf`.  Visual overlay approach;
  content-stream text removal deferred.

- [x] **P26.8 — Document status / review workflow.**  Lightweight
  approval flow: `doc_status` column (`pending_review` / `approved` /
  `rejected` / `archived`).  Schema migration v111.  Filter in
  Übersicht + search.  Bulk status-change in the overview table.
  No multi-user approval chains (rabbit hole) — just a per-document
  status flag that's enough for solo / small-team review workflows.

- [ ] **P26.9 — Scanner integration (SANE/TWAIN).**  Direct
  acquisition from flatbed/ADF scanners into the ingest pipeline.
  Linux: SANE via `sane-rs` (FFI to libsane).  macOS: ImageCaptureCore
  via objc2.  Windows: WIA via COM (deferred until Windows builds
  stabilise).  Settings panel for scanner selection, resolution, color
  mode, duplex.  Scanned pages feed directly into the OCR pipeline →
  index, skipping the filesystem.

### P27 — PDF manipulation & format parity (planned)

Features that turn CrispSorter from a read-only document intelligence
tool into a full document lifecycle platform.  The heaviest items are
PDF editing and form creation; the rest are moderate or small.

- [x] **P27.1 — PDF viewer + page-level operations.**  ✅ SHIPPED
  (v0.8.0, 2026-06-21).  Universal `DocumentViewer` component with
  pdfjs-dist canvas rendering (page nav, zoom, text selection) +
  `pdf_ops.rs` Rust module with 12 lopdf operations: reorder, extract,
  remove, rotate, crop, merge, split, add page numbers, watermark,
  insert blank, edit metadata.  `PdfTools.svelte` tab with page sidebar
  + operation panels.  CLI `crispsorter pdf <subcommand>` (12 verbs).
  All registered as Tauri commands (desktop + mobile).

- [x] **P27.2 — PDF text extraction & OCR overlay.**  ✅ Already
  shipped across prior phases.  Extract text: `extract_pdf_native`
  Tauri command + CLI.  OCR → searchable PDF: `ocr --render pdf` CLI
  + `tool_ocr_export` Tauri command (+ stamp_text in v0.8.1).  OCR →
  text: OCR Workbench + `crispsorter ocr --render text`.  All three
  capabilities reachable from the existing UI surfaces.

- [ ] **P27.3 — PDF text editing (in-place).**  Edit text paragraphs
  in a PDF by rewriting content streams.  Scope is intentionally
  limited to text replacement within existing bounding boxes — not a
  full desktop-publisher layout engine.  Approach: extract text runs
  with coordinates from the content stream, present them as editable
  spans in the preview pane, rewrite the affected content stream
  operators on save.  Font subsetting (embed only used glyphs) is the
  hardest sub-problem — use `subsetter` crate or CrispEmbed's
  freetype bindings.  Library: `lopdf` for low-level stream
  manipulation + PDFium for rendering the preview.  Frontend:
  click-to-edit text overlay on the page preview.  **Large** —
  20–40 h for a robust implementation; a "good enough" v1 that
  handles single-font Latin text is ~12 h.

- [ ] **P27.4 — Interactive PDF form creation.**  Insert AcroForm
  fields (text input, checkbox, radio, dropdown, signature placeholder)
  into an existing PDF.  Uses `lopdf` to write `/AcroForm` dictionary +
  widget annotations.  Frontend: field palette + drag-to-place on the
  page preview.  CLI: `crispsorter pdf add-field doc.pdf --type text
  --page 1 --rect 100,200,300,230 --name "invoice_number"`.  **Large**
  — AcroForm spec is deep; a v1 covering text + checkbox + dropdown
  is ~16 h; radio buttons and signature fields add ~8 h.

- [ ] **P27.5 — PDF/UA accessible output.**  Generate tagged PDFs
  with a structure tree (headings, paragraphs, tables, figures with
  alt-text) so the output is screen-reader-friendly.  Builds on the
  existing searchable-PDF renderer (`ocr --render pdf`) by adding
  `/StructTreeRoot`, `/MarkInfo`, and tagged content markers.  Requires
  proper reading-order (already available via P20's layout-aware
  pipeline).  Validation against the Matterhorn Protocol checklist
  (PDF/UA-1 conformance).  ~12–16 h.

- [ ] **P27.6 — MRC (Mixed Raster Content) compression.**  Layer-
  separated PDF compression for scanned documents: foreground (text)
  as JBIG2 or CCITT, background (images/photos) as JPEG2000/JPEG,
  mask layer as 1-bit.  Produces dramatically smaller files (often
  3–5× reduction) while preserving visual fidelity.  Approach:
  CrispEmbed's binarisation output already separates text from
  background; encode each layer with the optimal codec, compose into
  a single PDF page via content-stream image XObjects.  JBIG2 encoding
  via `jbig2enc` (C, shell-out) or a Rust port.  ~12 h.

- [x] **P27.7 — Password-protected PDF handling.**  ✅ SHIPPED (2026-07-02).
  `pdf_ops::decrypt_pdf` + `pdf_ops::is_encrypted`.  Tauri commands
  `pdf_decrypt`, `pdf_is_encrypted`.  CLI: `crispsorter pdf decrypt
  --password PW --out decrypted.pdf` + `crispsorter pdf is-encrypted`.
  Frontend: PDF Tools tab shows Decrypt button when PDF is encrypted,
  with password input panel.  ~4 h.

- [x] **P27.8 — Checkmark / OMR (Optical Mark Recognition).**  ✅
  SHIPPED (2026-07-04).

  **Goal:** Detect filled checkboxes, radio buttons, and bubble marks
  in scanned forms via classical CV (no ML model for v1).

  **Architecture — 3 slices:**

  **Slice 1 — OMR engine (`index/omr.rs`).**
  `detect_checkmark(image_path, x, y, w, h) → CheckmarkResult` where
  `CheckmarkResult { filled: bool, fill_ratio: f64, confidence: f64 }`.
  Algorithm: crop the zone, convert to grayscale, adaptive threshold
  (Otsu), count dark pixels / total pixels → `fill_ratio`.  If
  `fill_ratio > threshold` (default 0.15), mark as filled.  Confidence
  derived from distance to threshold.  No external CV dep — uses
  the `image` crate already in deps.  `detect_checkmarks(image_path,
  zones) → Vec<CheckmarkResult>` batch variant.  6+ unit tests
  (synthetic white/black images, partial fill, out-of-bounds).

  **Slice 2 — Template integration.**
  Extend `template_zones` with an optional `zone_type` column
  (default `"text"`, also `"checkbox"`).  `extract_zones` in
  `zone_ocr.rs` dispatches: `"text"` → OCR as before, `"checkbox"`
  → `detect_checkmark` → `ZoneResult.text = "true"/"false"`.
  Migration adds the column to existing templates.db.

  **Slice 3 — Tauri command + CLI.**
  `omr_detect(location_uri, x, y, w, h, threshold)` Tauri command.
  CLI: `crispsorter omr <FILE> --rect x,y,w,h [--threshold 0.15]`.
  Also usable via `crispsorter zone --template NAME FILE` when the
  template has checkbox-type zones.

- [ ] **P27.9 — Handwritten text recognition (ICR).**  Dedicated
  handwriting recognition beyond what the general VLM OCR engines
  provide.  v1: route handwriting regions (detected by the layout
  pipeline's region classifier) to a specialised engine — TrOCR
  fine-tuned on IAM/RIMES handwriting datasets via CrispEmbed GGUF,
  or Qwen2.5-VL with a handwriting-specific prompt.  v2: user-
  adaptive fine-tuning (few-shot LoRA on a handful of user-provided
  handwriting samples).  Scope depends heavily on script coverage —
  Latin handwriting is tractable; CJK/Arabic handwriting is a
  separate research problem.  ~8–12 h for v1 (Latin).

- [x] **P27.10 — Additional export formats.**  ✅ SHIPPED (2026-07-02).
  `extractors/export.rs` — `export_to_docx()` via `docx-rs` crate
  (title as Heading1 + body paragraphs) and `export_to_html()`
  (standalone HTML with embedded CSS, proper escaping).  Tauri
  commands: `export_to_docx`, `export_to_html`.  5 unit tests.
  Follow-up: XLSX (table export), EPUB (chapter structure), PPTX.

- [x] **P27.11 — Cloud storage connectors (OneDrive / Google Drive).**
  ✅ SHIPPED (2026-07-04).  Both connectors implement the `CloudDrive`
  trait via Microsoft Graph API v1.0 / Google Drive API v3.  OAuth2
  access token auth.  Registered in `DriveRegistry::instantiate`.
  8 unit tests.  OAuth webview flow + Settings UI deferred to
  follow-up.

  **Goal:** OAuth2-based cloud drive connectors beyond the existing
  WebDAV / Filen / Internxt support.

  **Architecture — per connector, implementing the `CloudDrive` trait
  (`list` / `download` / `upload` / `metadata`):**

  **OneDrive + SharePoint** (shared 80% of code): Microsoft Graph API
  via `oauth2` + `reqwest`.  Azure AD app registration (client_id +
  client_secret).  Token refresh via refresh_token grant stored in
  OS keychain (`keyring` crate, already used for LLM API keys).
  `list_folder` → `GET /me/drive/root:/{path}:/children`.
  `download` → `GET /me/drive/items/{id}/content`.
  `upload` → `PUT /me/drive/root:/{path}:/content` (< 4MB) or
  resumable upload session (> 4MB).  SharePoint: same Graph API,
  different drive root (`/sites/{site-id}/drive/...`).

  **Google Drive**: Drive API v3 via service account JSON key or
  OAuth2 web flow.  `list` → files.list with `q` parameter.
  `download` → files.get with `alt=media`.  `upload` → files.create
  multipart.  Folder semantics via `parents` field.

  **Common:** Each connector registers in the CloudDrive registry
  (same as `LocalDrive` / `InternxtDrive` / `FilenDrive` /
  `WebDavDrive`).  Settings UI: connector type dropdown, OAuth
  "Connect" button (opens webview for auth flow), token status
  indicator.  ~8 h per connector.

- [x] **P27.12 — Digital signature creation.**  ✅ SHIPPED (2026-07-04).
  `pdf_ops::sign_pdf()` via openssl (vendored). PKCS#12 cert loading,
  SHA-256 signing, /Sig dictionary + widget annotation on page 1.
  Tauri command `pdf_sign`. CLI: `crispsorter pdf sign --cert my.p12
  --password PW --out signed.pdf`.  Follow-up: visible signature
  appearance, LTV validation.

- [x] **P27.13 — PDF encryption & permissions.**  ✅ SHIPPED (2026-07-02).
  `pdf_ops::encrypt_pdf` with `EncryptConfig` (owner/user password +
  per-flag permissions: print, copy, modify, annotate, fill, assemble,
  high-quality print).  RC4-128 via lopdf `EncryptionVersion::V2`
  (AES V4/V5 deferred until lopdf exposes CryptFilter publicly).
  Tauri command `pdf_encrypt`.  CLI: `crispsorter pdf encrypt
  --owner-password ADMIN --no-print --no-copy --out protected.pdf`.
  Frontend: Encrypt panel in PDF Tools with password inputs +
  permission checkboxes.

- [x] **P27.14 — Hidden metadata removal.**  ✅ SHIPPED (2026-07-02).
  `pdf_ops::sanitise_pdf` strips: /Info dict, XMP metadata stream,
  JavaScript, EmbeddedFiles, OpenAction, per-page thumbnails, and
  annotations.  Returns list of what was stripped.  Tauri command
  `pdf_sanitise`.  CLI: `crispsorter pdf sanitise --out clean.pdf`.
  Frontend: "Sanitise" button in PDF Tools toolbar.

### P28 — Performance optimization pass (2026-07-04)

Systematic audit + optimisation of search, ingest, compile, and
frontend hot paths.  13 new unit tests.

- [x] **Search result cache.**  VecDeque LRU (O(1) eviction vs O(n)
  `Vec::remove(0)`).  Direct field-by-field hashing of `SearchFilters`
  instead of round-tripping through `serde_json::to_string`.  3 new
  tests (LRU promotion, hash determinism, f64 bit-pattern hashing).
- [x] **Zero-copy RRF merge.**  `rrf_merge_n` signature changed from
  `&[Vec<String>]` to `&[&[&str]]` — eliminates per-doc-id String
  cloning across all 4 RRF channels (FTS + dense + sparse + omni).
  Internal `HashMap<String, _>` → `HashMap<&str, _>`.  Owned Strings
  only materialised in the final output vec.  2 new tests.
- [x] **Allocation-free operator detection.**  `to_uppercase()` →
  `eq_ignore_ascii_case()` in `fts_query.rs` tokenizer, `fuzzify_query`,
  and `synonyms.rs`.  Guard against multi-byte UTF-8 panic on the
  `W/` / `PRE/` byte-slice check via `is_ascii()`.  4 new tests
  (mixed-case W/PRE, Unicode word safety, fuzzify operators, synonym
  operators).
- [x] **Ingest: parallel image embeddings.**  ViT + Omni
  `spawn_blocking` calls fired concurrently (both dispatched to the
  blocking pool before either is awaited).  ~2× wall-time improvement
  for dual-model image ingest.
- [x] **Ingest: conditional texts.clone().**  `texts.clone()` for
  `embed_full` now only happens when ColBERT is active; common path
  (no ColBERT) avoids the per-batch String vector copy.
- [x] **Ingest: single embedder lock.**  `model_id` read moved inside
  the existing lock guard, eliminating a redundant `embedder.lock().await`
  per batch.
- [x] **Ingest: single fs::metadata call.**  Eliminated duplicate
  `std::fs::metadata()` (was called for mtime-skip check, then again
  for mtime + file_size).  1 new test.
- [x] **LanceDB write batch size.**  Raised from 4× to 16× embed batch
  size (128 → 512 at default batch_size=32).  Arrow RecordBatch
  construction overhead is amortized over more rows.  1 new test.
- [x] **Snippet token filter.**  Replaced `.cloned().collect()` with
  in-place `.retain()` to avoid intermediate Vec allocation.  2 new tests.
- [x] **Dependency trimming.**  tokio `"full"` → 7 specific features
  (drops `net`, `signal`).  symphonia `"all"` → used codecs only
  (drops `adpcm`, `mp1`, `mp2`).  Removed unused `similar "unicode"`
  feature.  Removed duplicate `futures-util` dep (re-exported by
  `futures`); updated 6 import sites.
- [x] **Frontend: Vite vendor chunk splitting.**  `manualChunks` in
  `vite.config.js` splits pdfjs-dist, mammoth, tesseract.js, katex,
  deep-chat, web-llm, and HF transformers into separate lazy chunks.
- [x] **Frontend: lazy WebLLM import.**  `@mlc-ai/web-llm` dynamically
  imported inside `loadWebLLM()` instead of at module load time.
- [x] **ColBERT IN-list: single collect.**  Collapsed double Vec
  allocation (`ids` + `quoted`) into one pass in `rerank_with_colbert`.
- [x] **Vec::with_capacity in hot paths.**  Pre-computed `total_rows`
  from `batches` for `cluster_documents`, `list_failed_extractions`,
  `batches_to_search_results_with_scores`, and
  `record_batches_to_search_results`.
- [x] **LID text sampling: zero-alloc slice.**  Replaced
  `chars().take(2000).collect::<String>()` with a `char_indices`-based
  byte-boundary slice — avoids a heap allocation on every LID-enabled
  ingest call.
- [x] **Cargo profiles.**  `opt-level = 1` for deps in dev builds
  (arrow/lance/tantivy run ~3× faster); `lto = "thin"` in release.
- [x] **Browse scanner column projection.**  `scanner.project()` on
  `query_documents` excludes 3 embedding vectors, `multivec_packed`,
  `full_text_md`, `embedding_sparse`, `embedding_model` — potentially
  5–20× fewer bytes read per browse page.
- [x] **Cached Arrow schema.**  `Arc<Schema>` stored in `LocalIndex`
  at construction, reused by every `ingest_batch` (was rebuilding
  ~25 Fields per document).
- [x] **`truncate_str` helper.**  `snippet::truncate_str()` replaces
  `chars().take(N).collect::<String>()` at 5 hot-path sites (browse
  snippet, search snippet, translation snippet, federated snippet).
  Slices at char boundary without heap allocation.  2 new tests.
- [x] **Dynamic extractor imports.**  All 5 JS extractors (pdf, docx,
  epub, html, image) converted to `await import()` inside switch
  cases — mammoth, pdfjs, epub-parser, tesseract only load when the
  matching file type is processed.
- [x] **Column projection on all search queries.**  `search_result_columns()`
  helper applied to `search_vector`, `search_vector_column`,
  `find_similar`, `fetch_best_chunk_per_doc`, and
  `search_sparse_in_pool` — every LanceDB query now selects only the
  ~20 columns the result builder reads, excluding 3 embedding vectors
  + `multivec_packed` + `full_text_md` + `embedding_sparse` (except
  where needed for scoring).  Combined with the browse scanner
  projection, this covers all 6 major LanceDB read paths.
- [x] **Deferred doctype `to_lowercase()`.**  `text.to_lowercase()`
  moved past the extension-based early returns in `classify()` —
  avoids a full-text heap copy for extension-classified types.
- [x] **O(N) `chunk_text`.**  Replaced the O(N²)
  `text[pos..].find(word)` per-word loop with a single-pass byte-level
  word boundary scanner.
- [x] **Static diff tags.**  `DiffSegment.tag` changed from `String`
  to `&'static str` — eliminates one heap allocation per diff segment.
- [x] **NL query parser.**  5 × `to_lowercase()` → 1 (recompute only
  after mutations).  `while contains("  ")` loop → single-pass
  `split_whitespace().join()`.
- [x] **Warning cleanup.**  All 9 compiler warnings resolved (unused
  imports, unused variables, deprecated API, dead code).
- [x] **Edge-case test hardening.**  30 new tests across 11 modules:
  comparison (3), doctype (3), auto_file (3), nl_query (3),
  snippet (2), result_cache (2), annotations (3), retention (3),
  versioning (3), eml (2), export (3).  Total: 1006 tests.

### P29 — Cloud sync hardening (CrispCloud cross-pollination)

Patterns lifted from the [CrispCloud](../CrispCloud) sibling repo
(Flutter dual-panel cloud file manager, 14 providers, 4468 tests) that
directly strengthen CrispSorter's cloud sync path.  CrispCloud solves
many of the same problems (multi-provider uploads, offline resilience,
conflict handling) at a more mature level; the goal here is to port the
*designs*, not the Dart code.

#### Priority 1 — Transfer queue with backpressure

CrispCloud's `TransferQueue` enforces 3 concurrent transfers,
exponential backoff on transient failures, and unified progress
tracking.  CrispSorter's five cloud connectors (Internxt, Filen,
WebDAV, OneDrive, Google Drive) currently fire uploads/downloads
independently with no shared concurrency limit or retry policy.

- [x] **`sync/transfer_queue.rs` module.**  ✅ SHIPPED (2026-07-05).
  Bounded async `tokio::sync::Semaphore` (default 3 permits).
  `TransferDirection` enum (Upload / Download).  `TransferQueue::submit_upload`
  / `submit_download` return `TransferHandle` with `watch::Receiver<TransferProgress>`
  and `JoinHandle<Result<Vec<u8>>>`.  `TransferProgress` tracks `job_id`,
  `direction`, `drive_id`, `remote_path`, `bytes_done`, `bytes_total`,
  `TransferState` (Queued/Active/Retrying/Done/Failed/Cancelled).
  Backoff: `min(2^attempt * 500ms, 30s)` with jitter; transient network,
  timeout, and 5xx-style failures are retried while the semaphore permit is
  released.  `active_count()` for monitoring.  10 unit tests (concurrency
  limit, 4th-job-waits, progress reporting, failure state, transient retry
  recovery, cancellation during retry backoff, serde round-trip).
- [>] **Wire all 5 CloudDrive impls through the queue.**  Replace
  direct `reqwest` calls in `drives/{internxt,filen,webdav,onedrive,
  gdrive}.rs` upload/download methods with `TransferQueue::submit`.
  Each connector still owns its auth + endpoint logic; the queue only
  gates concurrency and retries. The GUI cloud-backup upload/restore path
  is now queue-backed for every configured provider. New GUI
  `drive_read_file` / `drive_write_file` commands also use the queue;
  CLI cloud-backup shard backup/restore and index archive promotion reads
  now use the queue as well. The lower-level FUSE read callback remains
  intentionally direct because its synchronous filesystem trait cannot
  safely await the async queue; provider-internal calls still need an
  explicit shared-queue boundary.
- [x] **Frontend: transfer drawer.**  Collapsible bottom panel showing
  active + queued transfers (filename, provider icon, progress bar,
  speed, cancel button). It polls the shared `transfer_queue_status`
  snapshot and uses `transfer_queue_cancel`; the snapshot boundary is used
  instead of a `transfer://progress` event because the queue retains
  terminal jobs for late-opening drawers.
- [x] **Tests.**  ✅ 11 tests shipped with the module (see above), including
  cancellation during retry backoff. Provider-wide queue wiring remains a
  separate integration task.

#### Priority 2 — Block-level delta sync

CrispCloud uses Adler-32 weak hash + SHA-256 strong hash per 4 MB
block, uploading only changed blocks (98.4% bandwidth savings on a
500 MB file with 8 MB changed).  CrispSorter's `cloud-backup` sync
re-uploads entire shards on every push.  As LanceDB lance files and
Tantivy segments grow, this becomes the dominant bandwidth cost.

- [x] **`sync/delta.rs` module.**  ✅ SHIPPED (2026-07-05).
  `Blockmap` struct with `Vec<Block>` where `Block { offset, size,
  weak_hash: u32, strong_hash: [u8; 32] }`.  `compute_blockmap(path,
  block_size)` + `compute_blockmap_from_bytes(data, block_size)`.
  `diff_blockmaps(local, remote) → Vec<ChangedBlock>`.  `delta_summary()`
  computes savings ratio.  Inline `adler32()` (no new dep).  Pure Rust.
  12 unit tests (Adler-32 known values, blockmap from file/bytes,
  single-block change, file growth, all-changed, serde round-trip,
  savings calculation).
- [ ] **`sync cloud-backup push --delta` flag.**  On push: compute
  local blockmap for each shard file, request remote blockmap from
  cb-api, diff, upload only changed blocks.  Falls back to full upload
  if the remote has no blockmap (first push or legacy server).
- [ ] **cb-api `/api/v2/shards/{id}/blockmap` + `/api/v2/shards/{id}/blocks`
  endpoints.**  `GET blockmap` returns the stored blockmap JSON.
  `PUT blocks?offset=N&size=M` writes a block at the given offset.
  `POST finalize` commits after all blocks are written.  Blockmap
  stored alongside each shard in the block-storage volume.
- [ ] **Lance file awareness.**  Lance `.lance` data files are
  append-mostly (new row groups appended, old ones rarely rewritten).
  Delta sync naturally exploits this — only the tail blocks change.
  Tantivy segments are immutable once written; only the `meta.json` +
  new segments need uploading.  Document this in the delta module so
  future maintainers understand why the savings are so high.
- [ ] **Tests.**  ✅ 12 unit tests shipped with the module (see above).
  Remaining: integration test with mock HTTP server verifying only
  changed blocks are uploaded.

#### Priority 3 — Offline operation queue with replay

CrispCloud persists failed/interrupted cloud operations to an
`OfflineQueue` SQLite table and replays them on reconnect.  CrispSorter
has a dead-letter queue for failed batch items, but no general offline
queue for cloud operations (drive uploads, sync pushes, manifest pulls).
If the network drops mid-sync, operations are lost.

- [x] **`sync/offline_queue.rs` module.**  ✅ SHIPPED (2026-07-05).
  WAL-mode SQLite `offline_queue.db`.  `OfflineQueue` with
  `enqueue(op_type, payload, provider_id)`, `dequeue_batch(limit)`,
  `mark_done(id)`, `mark_failed(id, error)`, `pending_count()`,
  `stats()` (pending/failed/total), `retry_all_failed()`,
  `purge_old(max_age)`.  Max 10 retries before permanent failure.
  6 unit tests (FIFO, mark_done, retry escalation, retry-all-reset,
  stats).
- [ ] **Enqueue on network failure.**  When a `TransferQueue` job
  exhausts its 5 retries (or gets a connection-refused / DNS error),
  persist it to the offline queue instead of dropping it.  Same for
  `sync cloud-backup push/pull` when the cb-api is unreachable.
  - [x] GUI drive writes stage failed bytes and enqueue a replay descriptor;
    startup maintenance and the explicit replay command retry those uploads.
    ✅ 2026-08-01
- [ ] **Replay on reconnect.**  Background task
  (`sync/offline_replay.rs`) polls network reachability every 60 s
  (HEAD request to the cb-api `/health` endpoint).  On success,
  drains the offline queue in FIFO order, re-submitting each op
  through the `TransferQueue`.  Exponential backoff on the poll
  interval (60 s → 120 s → 240 s, cap 600 s) to avoid hammering a
  flaky connection.
  - [x] Startup maintenance now applies independent 60s-to-600s exponential
    backoff to staged provider replay failures while manifest draining remains
    on its regular 30s ticker. ✅ 2026-08-01
- [ ] **Frontend: offline indicator.**  Status bar badge showing
  "N ops queued" when offline queue is non-empty.  Clicking opens a
  list with per-op details and a "Retry now" button.
  - [x] The existing transfer drawer now surfaces pending/failed counts and
    provides retry-failed and purge-failed controls. ✅ 2026-08-01
  - [x] Expanded drawer view now lists queued operation/provider, retry count,
    status, and the latest error diagnostic. ✅ 2026-08-01
- [ ] **Tests.**  ✅ 6 unit tests shipped with the module (see above).

#### Priority 4 — Conflict resolution policies

CrispCloud offers 5 policies: newest-wins, local-wins, remote-wins,
keep-both, manual.  CrispSorter's cloud-backup sync is currently
"last push wins" with no explicit conflict handling.  As federated
search grows (multiple machines indexing overlapping corpora), conflicts
will surface.

- [x] **`sync/conflict.rs` module.**  ✅ SHIPPED (2026-07-05).
  `ConflictPolicy` enum: `NewestWins`, `LocalWins`, `RemoteWins`,
  `KeepBoth`, `Manual` (default `NewestWins`).  `resolve_conflict(local,
  remote, policy) → Resolution` where `Resolution` is `UseLocal |
  UseRemote | KeepBoth { remote_doc_id } | NeedsManualReview`.
  Short-circuits on identical `source_hash` (no conflict).  `ConflictSide`
  struct with `doc_id`, `source_hash`, `updated_at`, `title`.
  10 unit tests (each policy, hash short-circuit, missing timestamps,
  equal timestamps, serde round-trip, default policy).
- [x] **Wire into `SyncManager` pull path.** ✅ 2026-08-01. Cloud-backup
  pull now compares same-path local hashes before ingest and applies the
  configured newest/local/remote/keep-both/manual policy. Remote replacement
  deletes stale local rows, keep-both uses a deterministic
  `<remote-hash>_remote` id, and manual conflicts are durably queued in
  `sync_conflicts` with idempotent `(path, remote_hash)` identity. Tauri
  list/ack commands expose the queue for the review surface.
- [x] **`IndexConfig.conflict_policy` setting.** ✅ 2026-08-01. Default is
  `NewestWins`; Settings persists the five policies through the authoritative
  index config and sync Tauri boundary. The cloud-backup pull CLI now accepts
  a one-shot `--conflict-policy newest|local|remote|keep-both|manual` override
  without rewriting Settings. ✅ 2026-08-01
- [ ] **Frontend: conflict review panel.**  When `Manual` policy is
  active and unresolved conflicts exist, show a review panel listing
  each conflict with local vs remote metadata side-by-side and
  accept/reject buttons.
  - [x] Settings now loads the durable queue and renders local/remote
    title/hash/timestamp metadata with refresh and safe "Keep local"
    acknowledgement. ✅ 2026-08-01
  - [ ] Remote acceptance remains deferred until the pull API can rehydrate
    the complete remote manifest row; the current UI deliberately does not
    pretend that acknowledging a conflict applies remote content.
- [ ] **Tests.**  ✅ 10 unit tests shipped with the module (see above).
  Manual queue persistence/deduplication coverage now ships in
  `sync::tests::manual_conflicts_are_durable_and_deduplicated`. ✅ 2026-08-01

#### Priority 5 — Share link generation

CrispCloud generates native share links for GDrive, OneDrive, and
Dropbox.  CrispSorter already connects to these providers but doesn't
expose sharing.  Since users store documents on these drives and search
them via CrispSorter, "share this document" from the search results is
a natural feature.

- [>] **`CloudDrive` trait: `share_link(path) → Option<String>`
  method** (default impl returns `None`).  The trait and unsupported-provider
  behavior are now covered by the drive tests.  OneDrive and Google Drive
  now override it with their native public-link APIs. Override in
  `OneDriveDrive` (Graph API `POST /me/drive/items/{id}/createLink`
  with `type: "view"`, `scope: "anonymous"`), `GDriveDrive` (Drive
  API `POST /files/{id}/permissions` + `webViewLink`), and
  `WebDavDrive` (Nextcloud OCS sharing API, if detected). WebDAV now
  detects `/remote.php/` roots and posts a read-only OCS share request.
  Internxt and Filen: stub until their public-link APIs are documented.
- [x] **Tauri command `drive_share_link(drive_id, path)`.**
  Resolves the drive, calls `share_link`, returns the URL or an
  error if the provider doesn't support sharing. ✅ Shipped 2026-07-31.
- [x] **Frontend: share button on search results.**  When a result's
  `location_uri` starts with `crisp+drive://`, show a share icon.
  Click → calls `drive_share_link` → copies URL to clipboard with an
  inline notification. ✅ Shipped 2026-07-31. Provider-specific links are
  now implemented for OneDrive and Google Drive; WebDAV detection and
  disabled-state discovery remain pending.
- [x] **Tests.**  Unit: URL format validation per provider, unsupported
  provider returns None, error handling for expired tokens. OneDrive URL
  construction, Google/unsupported-provider coverage, and a hermetic
  Nextcloud OCS request/response test now pass; Google and Microsoft Graph
  401 responses are also covered hermetically.

#### Priority 6 — Cloud provider version history

CrispCloud integrates with GDrive/OneDrive/Dropbox version history
(list versions, restore previous).  CrispSorter tracks document
versions locally (P25.1, SHA-256 groups in `versions.db`) but doesn't
tap into the provider-side version history, missing an opportunity to
unify local and cloud version tracking.

- [x] **`CloudDrive` trait: `list_versions` + `restore_version` +
  `share_link`.**  ✅ SHIPPED (2026-07-05).  Three new default methods
  on `CloudDrive` (all backward-compatible — existing impls inherit
  no-op defaults).  `FileVersion { id, modified_at, size, modifier_name }`
  type.  `DriveType::label()` helper.  **OneDrive:** `list_versions`
  via Graph API `GET /versions`, `restore_version` via `POST
  restoreVersion`.  **Google Drive:** `list_versions` via Drive API
  `GET /revisions`, `restore_version` via download-revision +
  PATCH-upload (GDrive has no native restore endpoint).
  4 unit tests (DriveType::label, FileVersion serde, default methods).
- [x] **Tauri commands `drive_list_versions` + `drive_restore_version`.**
  ✅ SHIPPED (2026-08-01); commands enforce provider capability checks before
  making network requests.
- [x] **Frontend: version history panel.**  ✅ SHIPPED (2026-08-01).  The
  cloud drive context pane combines provider versions with local index
  history, labels provenance, and offers guarded provider restore.  In the
  document viewer
  sidebar, when viewing a cloud-backed document, show a "Versions"
  tab listing cloud versions with timestamps and a "Restore" button.
  Merges with the existing local version history from P25.1 into a
  unified timeline (local versions tagged "local", cloud versions
  tagged with the provider name).
- [ ] **Tests.**  ✅ 4 unit tests shipped (see above).  Live tests
  require OAuth tokens — tagged `#[ignore]`.

#### Priority 7 — Certificate pinning

CrispCloud pins TLS certs for Google, Microsoft, Dropbox, and Amazon
endpoints.  CrispSorter talks to the same services via `reqwest` with
no pinning.  Low effort, meaningful security improvement.

- [x] **`sync/cert_pins.rs` module.**  ✅ SHIPPED (2026-07-05).
  `PinSet` struct with provider name, domain patterns, and SHA-256
  SPKI pin hashes.  `builtin_pin_sets()` covers Google (GTS Root R1 +
  R4), Microsoft (DigiCert Global Root G2 + Baltimore CyberTrust),
  Dropbox (DigiCert), Amazon/S3 (Amazon Root CA 1 + Starfield G2).
  `find_pin_set(hostname, sets)` with wildcard domain matching.
  `verify_pin(spki_hash, pin_set) → (matches, is_backup)`.
  8 unit tests (exact match, wildcard, find/verify, serde).
- [ ] **Wire into cloud drive constructors.**  Each `*Drive::new()`
  that talks to a pinnable endpoint uses `pinned_client()` instead
  of the default `reqwest::Client`.
- [ ] **Pin rotation strategy.**  Pin the *root* CA, not the leaf
  cert (roots rotate on a multi-year cadence).  Include 2 pins per
  provider (current + backup) to survive a CA migration.  Log a
  warning (not hard-fail) when only the backup pin matches — signals
  an upcoming rotation.
- [ ] **Tests.**  ✅ 8 unit tests shipped with the module (see above).

#### Priority 8 — HTTP/SOCKS5 proxy support

CrispCloud has `ProxyService` for HTTP and SOCKS5 proxies.  CrispSorter
has no proxy support — users behind corporate proxies can't use cloud
features.  `reqwest` already supports proxies natively, so this is
mostly plumbing + settings UI.

- [ ] **`IndexConfig` proxy fields.**  `proxy_url: Option<String>`,
  `proxy_username: Option<String>`, `proxy_password: Option<String>`.
  Supports `http://`, `https://`, `socks5://`, `socks5h://` URL
  schemes.  Password stored in OS keychain (same pattern as LLM API
  keys).
- [x] **`sync/proxy.rs` helper.**  ✅ SHIPPED (2026-07-05).
  `ProxyConfig` struct (url, username, password, all optional).
  `build_async_client(config)` and `build_blocking_client(config)`.
  Supports `http://`, `https://`, `socks5://`, `socks5h://`.  Falls
  back to default client when no proxy configured (respects env vars).
  8 unit tests (empty config, HTTP/SOCKS5, auth, invalid URL, serde).
- [ ] **Wire into all cloud-facing code.**  `CloudDrive` constructors,
  `SyncManager`, `cb-api` client, feed fetcher (`feed.rs`), LLM API
  clients.  The shared `CloudBackupClient::new_with_proxy`,
  `SyncManager::*_with_proxy`, and `fetch_and_parse_with_proxy` boundaries
  plus `RemoteClient::new_with_proxy` are now wired; remaining providers use their existing constructors until
  their credential/config plumbing is migrated. Single `build_proxy_client`
  call site shared via a lazy `OnceCell<reqwest::Client>` remains future work.
- [ ] **Settings UI.**  "Network" section: proxy URL input, username,
  password (masked), "Test connection" button (HEAD to
  `https://www.google.com` through the proxy).  DE/EN i18n.
- [ ] **Tests.**  ✅ 8 unit tests shipped with the module (see above).

#### Priority 9 — FUSE mounting for cloud indexing

CrispCloud can FUSE-mount cloud storage on Linux/macOS via `MountService`.
If CrispSorter could mount a cloud drive and index it via the existing
folder watcher, users wouldn't need to download entire cloud libraries
locally.  This turns CrispCloud's FUSE layer into a transparent indexing
source.

- [x] **`drives/fuse_mount/` module.**  ✅ SHIPPED (2026-07-05).
  `fuser` 0.14 optional dep, gated behind `--features fuse`.
  `FuseDriveFs` implements `fuser::Filesystem` with dynamic inode
  mapping (bidirectional `path ↔ ino` HashMap).  Read-only: write
  ops return `EROFS`.  `lookup`, `getattr`, `readdir`, `read` delegate
  to `CloudDrive`.  `mount_blocking(drive, mount_point)` helper with
  `RO` + `AutoUnmount` mount options.  `FuseMountConfig` (drive_id,
  mount_point, cache_max_bytes with 2 GB default) + `FuseMountStatus`.
  Read results use a bounded byte-based LRU (2 GB default); oversized files
  bypass the cache.
- [x] **Tauri commands: `drive_mount(drive_id, mount_point, cache_max_bytes)`,
  `drive_unmount(drive_id)`.** ✅ SHIPPED (2026-08-01). Mount runs on a
  dedicated thread, tracks active mounts, requires an absolute mount point,
  and returns a clear feature-disabled error in non-FUSE builds. Unmount uses
  the platform helper with a safe explicit path argument. The optional cache
  budget is applied by the FUSE filesystem. `drive_mount_status`
  exposes the process-local lifecycle registry for UI and automation callers.
- [ ] **Integration with folder watcher.**  Once mounted, the user
  can point the existing `crispsorter watch <mountpoint>` at the
  FUSE directory.  The watcher sees new/changed files and feeds them
  into the ingest pipeline as if they were local.  No changes needed
  in the watcher itself — it already works on any filesystem path.
- [ ] **Platform notes.**  Linux: needs `fuse3` package + user in
  `fuse` group.  macOS: needs macFUSE or FUSE-T.  Windows: deferred
  (WinFSP/Dokany is a separate effort).  `doctor` now reports both whether
  the optional Rust feature was compiled and whether the host runtime is
  available.
- [x] **Tests.**  ✅ 5 unit tests shipped (config serde + LRU eviction).
  Integration tests require FUSE privileges — tagged `#[ignore]`; cache
  eviction is covered without requiring FUSE privileges.

#### Priority 10 — Automation rule engine

CrispCloud has an `AutomationEngine` with trigger-action rules and a
`PluginService` with a local REST API.  CrispSorter's folder watcher
(`P5`, `P26.2`) auto-processes new files, but there's no user-
configurable rule engine for complex workflows.

- [x] **`watcher/rules.rs` module.**  ✅ SHIPPED (2026-07-05).
  `Trigger` enum (Extension/Doctype/Tag/FolderPrefix/SizeRange),
  `TriggerMode` (All/Any), `Action` enum (Ingest/Tag/MoveTo/UploadTo/
  RunOcr/Notify), `AutomationRule` struct with name, enabled, priority,
  triggers, trigger_mode, actions.  `evaluate(file, rules, match_all)
  → Vec<Action>`.  `default_rules()` ships 3 example rules (disabled).
  13 unit tests (each trigger type, AND/OR modes, priority ordering,
  match-all, disabled skip, no-match fallthrough, serde round-trips).
- [ ] **`AutomationEngine` struct.**  Loaded from persisted rules
  (Tauri store or SQLite).  `evaluate(file_path, metadata) →
  Vec<Action>`.  Called from the folder watcher's dispatch path
  (after classification, before the default auto-file behaviour).
  If no rules match, falls through to the existing `WatchMode`
  behaviour (backward-compatible).
- [ ] **Tauri commands: `automation_add_rule`, `automation_list_rules`,
  `automation_update_rule`, `automation_delete_rule`,
  `automation_test_rule(file_path)`.**
- [ ] **Settings UI: "Automation" panel.**  Rule list with
  add/edit/delete.  Rule editor: trigger conditions (AND/OR
  combinable), ordered action list, priority slider, enabled toggle.
  "Test" button runs a rule against a sample file and shows what
  actions would fire.  DE/EN i18n.
- [ ] **Example rules shipped as defaults (disabled).**
  "Invoices to accounting folder": trigger `doctype:invoice` →
  `MoveTo("Invoices/{year}/{month}/")`.
  "Photos to cloud": trigger `ext:jpg,png,heic` + `size > 1MB` →
  `UploadTo(gdrive, "/Photos/{year}/")`.
  "OCR all scans": trigger `folder_prefix:/Scans/` → `RunOcr(smart)`.
- [ ] **Tests.**  ✅ 13 unit tests shipped with the module (see above).

### P30 — crisp-docx deep integration (2026-07-05)

CrispSorter uses only ~6 of crisp-docx's ~25+ public functions (all in
the translate pipeline).  This phase wires in the remaining capabilities:
OOXML surgery pre-processing, document validation, heading inference for
the search index, blueprint analysis for the viewer, and body transplant
("restyle to template") as a new PDF-Tools-tab feature.

#### P30.1 — Translation pre-processing (quick wins)

Wire three zero-UI calls into the existing `translate_docx` pipeline so
translated output is more robust.

- [x] **`strip_rsids()` before translation.**  ✅ SHIPPED (2026-07-05).
  Called immediately after `open()` in `translate/tauri_commands.rs`.
- [x] **`check_package()` pre-flight.**  ✅ SHIPPED (2026-07-05).
  Called after open; issues emitted as `translate://warning` Tauri
  event (non-blocking).  Also available standalone via
  `docx_check(path)` Tauri command in `docx_tools.rs`.
- [x] **`normalize_quotes_in_package()` before translation.**  ✅
  SHIPPED (2026-07-05).  Called after `strip_rsids` with
  `QuoteStyle::English` (uniform `"…"` for LLM input).  Also
  available standalone via `docx_normalize_quotes(path, style, output)`
  Tauri command.
- [x] **Tests.**  ✅ SHIPPED (2026-07-30).  The fixture-based tests are
  no longer deferred: `docx_tools::fixtures` *authors* minimal `.docx`
  packages in memory (a docx is a zip of a few XML parts), so no binary
  fixtures enter the repo and the shape each test depends on is visible
  in the test.  15 behavioural tests now cover rsid stripping,
  `check_package` on a package with a dangling relationship, page
  geometry in points, unstated-vs-stated geometry, heading inference,
  transplant, notes conversion, footnote injection and quote styles —
  alongside the 6 original serde tests.

#### P30.2 — Heading inference at index time

Many scanned→OCR→DOCX documents have no explicit heading styles.
`infer_heading_levels()` detects H1/H2/H3 from direct formatting
(bold + font size clustering), giving the search index structural
metadata.

- [x] **Wire into DOCX extractor.**  ✅ SHIPPED (2026-07-05; tested
  2026-07-30).  `extractors::extract_docx` calls
  `infer_heading_levels(&pkg, None)`, fills `ExtractedDocument.headings`
  and prepends Markdown-style markers (`# Title`, `## Section`) to the
  indexed text, so headings get their term-frequency boost.  Two tests
  pin both directions: a formatted fixture yields `# Chapter One` /
  `## Section A` with the body intact underneath, and ordinary prose
  yields no invented markers.
- [x] **`docx_infer_headings(path)` Tauri command.**  ✅ SHIPPED
  (2026-07-05).  `docx_tools.rs::docx_infer_headings` returns
  `Vec<InferredHeading { level, text }>`.
- [x] **CLI: `crispsorter docx headings <FILE>`.**  ✅ SHIPPED
  (2026-07-05).  Prints indented `H1`/`H2`/`H3` labels in text mode,
  JSON array in `--format json`.
- [x] **Tests.**  ✅ SHIPPED (2026-07-30) — see P30.1; fixtures are
  authored in memory rather than committed.

#### P30.3 — Blueprint analysis + document properties

Expose `analyze_blueprint()` in the document viewer sidebar so users
see page geometry, default font, section info at a glance.

- [x] **`docx_analyze(path)` Tauri command.**  ✅ SHIPPED (2026-07-05).
  `docx_tools.rs::docx_analyze` returns `DocxBlueprint { sections,
  default_font, default_font_size_pt, style_count }`.
- [x] **"Document Properties" panel.**  ✅ SHIPPED (2026-07-30) — in
  the new `DocxTools.svelte` panel rather than the viewer sidebar: the
  other seven DOCX operations needed a home of their own, and reading a
  document's geometry belongs next to the operations that change it.
  Shows page size, orientation, margins, default font + size and section
  count.  An unstated measurement renders as "not stated", never as 0.
- [x] **CLI: `crispsorter docx info <FILE>`.**  ✅ SHIPPED
  (2026-07-05).  Prints font, style count, section geometry in text
  mode; full `DocxBlueprint` JSON in `--format json`.
- [ ] **Tests.**  Fixture-based tests deferred.

#### P30.4 — DOCX validation command

Standalone "validate this DOCX" feature, beyond the pre-flight check
in translation.

- [x] **`docx_check(path)` Tauri command.**  ✅ SHIPPED (2026-07-05).
  `docx_tools.rs::docx_check` returns `DocxCheckResult { ok, issues,
  valid }`.
- [x] **"Validate DOCX" button.**  ✅ SHIPPED (2026-07-30) in the DOCX
  panel: lists the issues found and the checks that passed (a report
  with no `ok` lines is indistinguishable from a check that never ran).
- [x] **CLI: `crispsorter docx check <FILE>`.**  ✅ SHIPPED
  (2026-07-05).  Prints ✓/✗ per axis in text mode; `DocxCheckResult`
  JSON in `--format json`.  Exit code 1 on issues.
- [ ] **Tests.**  Fixture-based tests deferred.

#### P30.5 — Body transplant ("Restyle to template")

The headline feature: user picks a "blueprint" DOCX (company template
with styles/headers/footers), CrispSorter grafts the content of
another document into it.

- [x] **`docx_transplant(source, blueprint, output)` Tauri command.**
  ✅ SHIPPED (2026-07-05).  `docx_tools.rs::docx_transplant` opens
  both, calls `transplant_body`, returns `TransplantResult { output_path,
  source_paragraphs, blueprint_styles }`.
- [x] **"Restyle" panel.**  ✅ SHIPPED (2026-07-30) in the DOCX panel:
  pick a template, write a new file, and see paragraphs moved /
  template styles / styles remapped.
- [x] **CLI: `crispsorter docx restyle --source doc.docx --blueprint
  template.docx --out restyled.docx`.**  ✅ SHIPPED (2026-07-05).
- [x] **Style mapping.**  ✅ SHIPPED (2026-07-05).
  `StyleIndex::from_package()` on both files, `StyleMapper::new()`
  with empty overrides, `apply_style_mapping()` on the transplanted
  result.  Fallback chain used automatically.  `TransplantResult`
  reports `styles_remapped` count.
- [ ] **Tests.**  Fixture-based tests deferred.

#### P30.6 — Footnote/endnote conversion

- [x] **`docx_convert_notes(path, target_kind, output)` Tauri
  command.**  ✅ SHIPPED (2026-07-05).  `docx_tools.rs::docx_convert_notes`
  accepts `"footnotes"` or `"endnotes"`.
- [x] **"Convert Notes" button.**  ✅ SHIPPED (2026-07-30) in the DOCX
  panel (footnotes ⇄ endnotes).
- [x] **CLI: `crispsorter docx convert-notes --to endnotes doc.docx
  --out converted.docx`.**  ✅ SHIPPED (2026-07-05).
- [ ] **Tests.**  Fixture-based tests deferred.

#### P30.7 — Footnote injection from LLM output

When the LLM (or OCR) produces text with inline `[N]` markers,
`inject_footnotes()` turns them into real Word footnotes.

- [x] **`docx_inject_footnotes(path, notes_map, output)` Tauri
  command.**  ✅ SHIPPED (2026-07-05).  `docx_tools.rs::docx_inject_footnotes`
  accepts `BTreeMap<u32, String>`, returns count of inserted footnotes.
- [ ] **Post-process hook in translate pipeline.**  After LLM
  translation, scan the output for `[N]` patterns, extract note texts,
  call `inject_footnotes`.  Opt-in via `IndexConfig.inject_footnotes`.
- [x] **Tests.**  ✅ SHIPPED (2026-07-30).
  `inline_markers_become_real_footnotes` runs the command on an authored
  fixture with `[1]`/`[2]` markers and asserts two footnotes exist, the
  note text is in `word/footnotes.xml`, a `footnoteReference` is in the
  body, and the literal `[1]` is *gone* — a document showing both the
  marker and the footnote mark is worse than either.
- [x] **CLI + GUI.**  ✅ SHIPPED (2026-07-30).  `crispsorter docx
  inject-footnotes <FILE> --note '1=text' --out F` (repeatable; a marker
  given twice is refused rather than silently overwritten, and unmatched
  markers are counted in the summary), plus an "Add footnotes" panel in
  `DocxTools.svelte`.

#### P30.8 — Corpus-wide quote normalization

Extend `normalize_quotes_in_package()` beyond translation to the
ingest pipeline, so full-text search isn't confused by mixed quote
styles in the corpus.

- [ ] **At index time (optional).**  When `IndexConfig.normalize_quotes`
  is true, run quote normalization on extracted text before indexing.
  Normalizes to ASCII `"` and `'` for consistent BM25 matching.
- [x] **`docx_normalize_quotes(path, style, output)` Tauri command.**
  ✅ SHIPPED (2026-07-05).  `docx_tools.rs::docx_normalize_quotes`
  accepts style name string.
- [x] **Tests.**  ✅ SHIPPED (2026-07-30).
  `quotes_are_curled_in_the_requested_style` asserts the German and
  English styles produce *different* bytes (a normaliser that ignores the
  style argument would otherwise pass), each with its own opener, and
  that the straight quotes are gone.
- [x] **GUI.**  ✅ SHIPPED (2026-07-30) — quote-style picker in
  `DocxTools.svelte`; the CLI verb shipped 2026-07-05.

### P31 — App Store submission readiness (2026-07-10)

Prepare the iOS and macOS builds for Apple App Store / TestFlight
submission.  Follows the playbook in `../appstore.md` (validated on
CrispChess + CrispSudoku + Brickwright).  The CI pipeline is
conditional on secrets — when `APPLE_API_KEY_P8` is set, it produces
a signed IPA + uploads to App Store Connect; otherwise falls back to
the existing unsigned build.

#### Prerequisites (human, one-time)

- [x] **App Store Connect API key** — Key ID `9RMU3C7422`, Issuer ID
  `5f618ba3-98ef-42ad-835c-fbbef6c76cf5` (hardcoded in release.yml
  env block — not secrets).  Remaining: store the `.p8` private key
  as repo secret `APPLE_API_KEY_P8` (`base64 < AuthKey_9RMU3C7422.p8`).
- [x] **Register bundle ID** `com.crispstrobe.crispsorter` ✅ DONE
  (2026-07-10).  Platform `UNIVERSAL`, Team ID `N9XSJ4M3GT`,
  resource ID `965ZJTQ9SK`.
- [x] **Create app record** ✅ DONE (2026-07-10).  Numeric app ID
  `6789543049`.  `APPSTORE_APP_ID` repo secret set.
- [ ] **App Privacy "nutrition label"** (browser-only — Apple blocks
  this via API).  CrispSorter collects no data → "Data Not Collected".
  Privacy policy URL: `https://crispstrobe.github.io/CrispSorter/privacy.html`
  (GitHub Pages enabled on `docs/` folder).

#### Shipped artifacts (2026-07-10)

- [x] **`entitlements.plist`** — macOS App Sandbox entitlements for
  Mac App Store.  Grants: `app-sandbox`, `network.client`,
  `network.server` (local Tauri dev server + crisp-index-server),
  `files.user-selected.read-write`, `files.downloads.read-write`.
- [x] **`ExportOptions.plist`** — iOS export options for
  `app-store-connect` method with automatic signing.
- [x] **`tauri.conf.json` macOS bundle config** — `entitlements`,
  `minimumSystemVersion: 12.0`, `hardenedRuntime: true`.
- [x] **`.gitignore` updated** — excludes `.appstoreconnect/` and
  `*.p8` files.
- [x] **`release.yml` iOS job rewritten** for App Store signing:
  - Decodes `APPLE_API_KEY_P8` secret into `~/.appstoreconnect/`
  - Archives with `-authenticationKeyPath` / `-allowProvisioningUpdates`
  - Exports signed IPA via `ExportOptions.plist`
  - Validates + uploads IPA to App Store Connect via `xcrun altool`
  - Falls back to unsigned build when secrets are absent
  - Injects `ITSAppUsesNonExemptEncryption=false` into Info.plist
- [x] **Consistent release asset naming** — Android APK and iOS IPA
  now follow `CrispSorter_{version}_{platform}.{ext}` pattern.

#### Additional artifacts shipped (2026-07-10)

- [x] **Privacy policy** — `docs/privacy.html`, hosted via GitHub Pages
  at `https://crispstrobe.github.io/CrispSorter/privacy.html`.
- [x] **`PrivacyInfo.xcprivacy`** — Required Reason API manifest
  (NSUserDefaults CA92.1, FileTimestamp C617.1, DiskSpace E174.1).
  Injected into the generated Xcode project in CI after `tauri ios init`.
- [x] **AGPL + App Store exception** — LICENSE file already has a
  Section 7 additional permission granting app marketplace distribution.
  No separate LICENSE-COMMERCIAL needed.
- [x] **All 4 repo secrets set** — `APPSTORE_API_KEY_ID`,
  `APPSTORE_API_ISSUER_ID`, `APPSTORE_API_KEY_P8`, `APPSTORE_APP_ID`.

#### Remaining (future sessions)

- [ ] **macOS MAS (Mac App Store) build.**  Separate from the existing
  `.dmg` (Developer ID) build.  Needs: `Apple Distribution` cert for
  signing the `.app`, `3rd Party Mac Developer Installer` cert for
  signing the `.pkg`, `productbuild` step, `altool --type macos` upload.
  The entitlements.plist is ready; the CI step + tauri.conf.json
  `signingIdentity` field are not.
- [ ] **Set `DEVELOPMENT_TEAM` in generated Xcode project.**  After
  `tauri ios init`, inject the team ID from secrets into the generated
  `project.pbxproj` (`CODE_SIGN_STYLE = Automatic;
  DEVELOPMENT_TEAM = <id>;`).  Currently relying on
  `-allowProvisioningUpdates` + API key to resolve this automatically.
- [ ] **TestFlight distribution automation.**  After upload succeeds:
  set encryption compliance, create internal beta group, assign build,
  add testers — all API-doable per `appstore.md` Step 9.
- [ ] **App Store listing metadata.**  Description, keywords, subtitle,
  category (`PRODUCTIVITY` or `UTILITIES`), screenshots (Simulator-
  generated per Step 11), pricing (free), age rating, review contact.
- [ ] **Screenshot generation in CI.**  Boot iOS Simulator, install
  debug `.app`, capture screenshots via `xcrun simctl io`, upload to
  App Store Connect API — per `appstore.md` Step 11.

---

### P32 — PDF read/edit completion (planned, 2026-07-29)

Closes the four gaps left after P27 shipped the one-shot PDF operations,
and puts a direct-manipulation editor in front of them.

#### Licence constraint (applies to every item below)

Third-party PDF dependencies **must be permissive** (MIT / Apache-2.0 /
BSD / Zlib / MPL-2.0).  Our own crates are AGPL-3.0-or-later and we hold
the copyright, so we can grant Apple the extra terms AGPLv3 §7 would
otherwise forbid — that is what makes AGPL + App Store work here.  We
cannot do that for code we do not own, so any third-party GPL/AGPL
dependency permanently blocks App Store distribution.  LGPL is also out:
iOS static-only linking defeats the relink provision.  Apache-2.0 flows
one-way into AGPLv3, so Apache dependencies are safe.

Ruled out for this reason, despite being the obvious reaches: MuPDF
(AGPL), Ghostscript (AGPL), Poppler (GPL-2/3), PDFtk (GPL), iText
(AGPL), Xpdf (GPL).

Verified permissive and approved for use (checked 2026-07-29):

| Library | Licence | Role |
|---|---|---|
| `lopdf` 0.44 | MIT | already in tree — annotations, forms, outlines |
| `qpdf` 0.3.5 → QPDF C++ | MIT/Apache-2.0 → Apache-2.0 | AES-256, linearize, repair |
| `krilla` 0.8 | MIT/Apache-2.0 | tagged PDF/UA output (feeds P27.5) |
| `hayro` 0.7 | Apache-2.0/MIT | pure-Rust rasterizer, no libpdfium binary |
| `printpdf` 0.12 / `pdf-writer` 0.15 | MIT / MIT-Apache | page composition |
| PDFium (bundled) | Apache-2.0 + BSD-3 | already used via `pdfium-render` |

#### Items

- [x] **P32.1a — PDF edit-session backend.**  ✅ SHIPPED (2026-07-29).  Today every `pdf_ops`
  command is one-shot `path → out_path`, so each edit forces its own
  save dialog.  Add a session held in Tauri state: open a document into
  an in-memory `lopdf::Document`, apply ops against it, keep an op log
  for undo/redo, save/save-as once.  Session commands wrap the existing
  pure functions so the CLI and one-shot paths keep working unchanged.

- [x] **P32.1b — Page editor GUI.**  ✅ SHIPPED (2026-07-29).  `PdfTools.svelte` currently renders
  pages as a text list (number + dimensions).  Replace with a pdf.js
  thumbnail grid: multi-select (click / shift / cmd), drag-to-reorder,
  delete, rotate in place, extract and insert page ranges, visual crop
  with an apply-to-all-pages toggle, live page-number preview, and text
  box placement.  Undo/redo wired to P32.1a.

- [x] **P32.2 — Print + native share seam.**  ✅ SHIPPED (2026-07-29;
  macOS share sheet live, Windows/Linux reveal-in-file-manager fallback,
  iOS impl still to write against the same trait).  Platform trait, desktop
  implementations now, iOS-ready.  macOS `NSSharingServicePicker`
  (AirDrop, Mail, Messages) + `NSPrintOperation`; Windows
  `IDataTransferManager` + ShellExecute print; Linux
  xdg-desktop-portal + `lp`.  The iOS implementation
  (`UIActivityViewController` / `UIPrintInteractionController`) slots in
  behind the same trait without touching call sites.  No iOS
  scaffolding in this scope — see P31.

- [x] **P32.3 — In-PDF annotations round-trip + export.**  ✅ SHIPPED
  (2026-07-29; backend + store bridge, no GUI surface yet).
  `index/annotations.rs` stores highlights and notes in SQLite keyed by
  `doc_id` + page + bbox, but nothing writes `/Annots`, so our markup is
  invisible outside the app and dies on export.  Both directions, pure
  `lopdf`: read `/Annots` from incoming PDFs into the same tables (which
  makes third-party markup FTS-searchable), and write the tables back
  out as real `/Annot` objects (Highlight / Text / Square / FreeText).
  Export to Markdown, CSV, JSON, and highlighted DOCX via `docx-rs`.

- [x] **P32.4 — Kindle clippings import with document matching.**
  ✅ SHIPPED (2026-07-29). Parser, match cascade and store wiring, with
  `kindle_list_books` + `kindle_import`. Upstream tier 3 (Calibre,
  highlighted DOCX) stays out of scope. No GUI surface yet.  Port
  tiers 1+2 of `CrispStrobe/highlighter` (Python, MIT).  Parse
  `My Clippings.txt` into the annotation tables, then locate each
  highlight in the real document text via a match cascade: exact/regex →
  normalised fuzzy (`similar` + `deunicode`) → embedding similarity over
  chunks (fastembed / CrispEmbed) → offset refinement through
  `crisp-docx-align::align_texts`.  Anchoring to real offsets is what
  makes imported highlights render in the viewer rather than float free.
  Calibre library integration and the DOCX generation path are the
  upstream tier-3 features and stay out of this slice.

- [x] **P32.5 — AcroForm read / fill / flatten.**  ✅ SHIPPED (2026-07-29).  No form support
  exists today (zero `/AcroForm` or `/Widget` references in tree).
  Traverse the field tree, read types and values, set `/V`, flatten to
  static page content.  First cut sets `NeedAppearances true` rather
  than generating `/AP` streams.  Pure `lopdf`, no new dependency.

#### P32.11 — Evaluate three new pure-Rust PDF crates (2026-07-30)

Surveyed on request; all three are MIT, so the permissive-only constraint is
satisfied in every case.  The interesting question is whether any removes a
deferral.

##### Source review (2026-07-30) — what is actually implemented

Cloned and read, because a feature list is not evidence and this project has
already been burned by one (`pdf_oxide`'s `writer/linearization.rs`: 696
lines, own docs say "reserved, no-op", `options.linearize` never read):

* **Decryption is real.** `zpdf-parser/src/crypt.rs` implements the Standard
  security handler — RC4, AESV2, and **AES-256 V5 R5/R6 via ISO 32000-2
  Algorithm 2.A**, recovering the file key from `/UE`/`/OE`, with `hash_r6`
  implementing the Algorithm 2.B hardened hash (iterated AES-128-CBC over 64
  repetitions). Authenticates against both `/U` and `/O`; a non-empty
  password matching neither returns `WrongPassword` instead of falling back
  to the empty-password path, which is lopdf's failure mode.
* **The decrypted *write* exists** — the half pdf_oxide gets wrong.
  `zpdf-writer/src/rewrite.rs`: opening with a password and rewriting
  "produces a plain-text equivalent — `/Encrypt` is dropped".
* **Linearization is wired**: `linearize_pdf` is exported from the crate
  root, called from their CLI, and the 449-line implementation writes a real
  `/Linearized` parameter dictionary with patched `/L /H /O /E /T`, a
  first-page xref, hint stream, main xref and `startxref`. Its doc comment is
  honest that the hint stream carries generic offsets rather than per-page
  detail — which the spec permits, hints being advisory.
* Also present: `subset.rs` (font subsetting → the tier-3 reflow
  prerequisite), `redact.rs`, `sign.rs`, `merge.rs`, `forms.rs`.
* **`wgpu` and `tiny-skia` are behind the facade's `cpu-render` /
  `gpu-render` features**; `zpdf-render` itself is only traits. With
  `default-features = false` no rasterizer is pulled in.

##### Scoped tasks — zpdf

- [x] **Z1. Optional dependency + feature.** ✅ (2026-07-30) `zpdf` (with
  `default-features = false`) and `zpdf-writer` 0.11 behind `pdf-zpdf`.
- [x] **Z2. Decrypt with a user password.** ✅ (2026-07-30)
  `pdf_ops::decrypt_via_zpdf`, first in the fallback chain (lopdf → zpdf →
  pdf_oxide → clear error). Verified against text read from the in-memory
  decrypted document *before* writing — because
  `verify_decrypted`'s content check silently short-circuits for an
  encrypted source (`pdf_extract` cannot read it, `src_text` comes back
  empty, the comparison is skipped), which is precisely the case where
  still-encrypted streams must not slip through. Also asserts no `/Encrypt`
  survives and the page count is unchanged, read back with lopdf rather than
  with the library that wrote it.
- [x] **Z3. `crispsorter pdf linearize`.** ✅ (2026-07-30) With the same
  discard-on-failure discipline as compression, and an honest error when the
  feature is off.
- [x] **Z4. Verify the claims with the independent harness.** ✅ RUN
  (2026-07-30) — **decryption confirmed, linearization rejected.**

  *Decryption works.* `pdf decrypt` on a file with a non-empty user password
  now produces output that qpdf calls valid, that opens with **no** password,
  whose encryption is genuinely gone, and whose text survives the round trip.
  That is the capability lopdf cannot provide and pdf_oxide's writer failed
  at, and it is graded by third-party tools rather than by zpdf.

  *Linearization is broken in zpdf 0.11.0.* On a 4-page MuPDF fixture:
  `qpdf --check-linearization` → `/N does not match number of pages`;
  `qpdf --check` → "reported number of objects (26) is not one plus the
  highest object number (10)" plus three "Pages tree includes non-dictionary
  object"; MuPDF → hundreds of `object out of range (11 0 R); xref size 11`,
  i.e. objects the page tree references are missing from the xref. MuPDF and
  poppler still recover the text, so a laxer check would have called this a
  pass — only qpdf's linearization-specific check names the `/N` defect. Our
  page-count/catalog guard rejects the output, so `pdf linearize` fails
  loudly and writes nothing. **The implementation is real, just wrong** —
  449 lines that genuinely patch offsets, unlike pdf_oxide's no-op.
  Follow-ups: report upstream, re-test on the next release, keep the verb
  behind the feature until then.

- [x] **Z4b. Linearization fixed in our fork.** ✅ (2026-07-30) The defect was
  one hunk: the **main** cross-reference table emitted `xref 0 3` + `/Size 3`,
  repeating objects 1–2 that the *first-page* table already covers, so every
  object after the first-page section (11..25 on a four-page file) existed in
  the body and in no table at all. Annex F splits it the other way — the
  front table covers the first-page section, the main table the remainder.

  Diagnosed by reading the emitted bytes, after ruling out misuse: our call
  matches their CLI and docs, and the failure is identical on a raw MuPDF
  file, a `qpdf --object-streams=disable` normalisation, and `rewrite_pdf`
  output. `github.com/CrispStrobe/zpdf`, branch
  `fix-linearize-xref-coverage`; upstream PR **Xero-Team/zpdf#4**.

  After the fix: qpdf reports "File is linearized" with no structural
  errors, pikepdf `is_linearized`, MuPDF and poppler text identical to
  source. `pdf linearize` works.

  **Packaging note.** `[patch.crates-io] zpdf-writer` does not work: patching
  one member of a workspace family leaves two copies of its siblings in the
  graph (the patched writer used the fork's `zpdf-parser`, the facade
  crates.io's), so `doc.file()` returns a `PdfFile` of the wrong identity.
  Both direct deps come from the fork instead. Revert to
  `version = "0.11.x"` when the PR lands.

- [x] **Z7. xref repair.** ✅ SHIPPED (2026-07-30). `pdf repair` recovers a
  file whose cross-reference data is damaged, verified across three modes — a
  `startxref` pointing at nonsense, a tail truncated before the xref *and*
  trailer, and a corrupted offset — each recovering all 4 pages and the
  intact file's text, with `qpdf --check` confirming the input really was
  damaged. Refuses a file with nothing recoverable rather than writing an
  empty document.

- [ ] **Z10. Real hint tables for linearization.** The one remaining gap:
  zpdf writes a placeholder hint stream (4 zero bytes), so
  `qpdf --check-linearization` reports `overflow reading bit stream`. Hints
  are advisory — every other reader is satisfied and qpdf's own `--check`
  says the file is linearized — so this is a polish item, not a blocker.
  Scope: bit-packed page-offset and shared-object hint tables per Annex F.
  The harness already asserts that this is the *only* remaining complaint, so
  a regression elsewhere fails rather than hiding behind it. `scripts/verify_zpdf_claims.py` (scratchpad) encrypts the fixture
  **two ways** — our CLI's AES-256 *and* `qpdf --encrypt … 256`, so the
  result is not graded on our own writer — then judges the output with
  `qpdf --show-encryption`, `pikepdf.is_encrypted`, MuPDF-without-password,
  and a word-for-word `pdftotext` comparison against the pre-encryption
  text. That last check is where pdf_oxide died: its output parsed, claimed
  not to be encrypted, and every content stream failed to inflate.
  Linearization is judged by `qpdf --check-linearization`, not by zpdf.
  **Promote into `scripts/verify_pdf_independent.py` once green**, gated on
  the feature being compiled in.
- [ ] **Z5. Tauri command + GUI.** `pdf_linearize` command and a Linearize
  button in `PdfTools.svelte`, next to the existing decrypt panel (which
  gains a working path for the first time). en+de i18n.
- [x] **Z6. Retired `pdf-decrypt-full` / `pdf_oxide`.** ✅ (2026-07-30) Z4
  confirmed zpdf's decrypt half against third-party tools, so pdf_oxide had
  nothing left to offer: its *reading* half worked (it authenticated a real
  user password and extracted correct text) but its writer emitted
  still-encrypted stream bytes, so the path always failed its own
  verification and never produced a usable file. Removed the dependency, the
  feature, and `decrypt_via_oxide` (43 lines). The fallback chain is now
  lopdf → zpdf → a clear error naming `--features pdf-zpdf`. Drops ~170
  transitive crates. The prose explaining *why* the design looks like this is
  kept in the Cargo.toml and pdf_ops comments — the reasoning is the valuable
  part, not the code.

- [ ] **Z8. Font subsetting → tier-3 text editing.** `subset.rs` is the
  prerequisite that made P32.8 tier 3 out of scope. Re-scope tier 3 only
  after Z4/Z7 land; line breaking is still ours to write.
- [ ] **Z9. Evaluate replacing PDFium for `pdf-render`.** zpdf's CPU
  rasterizer (tiny-skia) would drop a per-platform native binary from the
  bundle. Compare page images against PDFium's on the OCR corpus before
  believing it; this is a quality question, not a licence one.

##### Scoped tasks — pdfk (reference only, never a dependency)

- [ ] **K1. Own the decrypt path.** If Z4 fails, or to drop the zpdf
  dependency later: derive the file key from `/Encrypt` with the RustCrypto
  primitives already in the tree (`aes`, `sha2`, `md-5`, `rc4`, `cbc`),
  decrypt streams and strings, strip `/Encrypt`, let lopdf write. pdfk does
  this in ~2,000 lines with `lopdf` 0.39, which is proof the shape works —
  read `github.com/anistark/pdfk`, do not link it (CLI-only, no `[lib]`).
- [ ] **K2. Password-rotation verb.** pdfk's `change-password` is decrypt +
  re-encrypt, both of which we would then have. Cheap once K1 or Z2 lands.
- [ ] **K3. `pdf audit`.** pdfk's directory scan for encryption compliance
  maps onto our batch surface: report which files in a tree are encrypted,
  with which handler and permissions. `detect_signatures` and `is_encrypted`
  already exist; this is a walker plus a report.

- [ ] **`zpdf` 0.11.0** (MIT, pure Rust, 576 downloads, updated 2026-07-26;
  `github.com/Xero-Team/zpdf`).  Its README claims, in one crate, most of
  what we deferred: **linearization** ("fast web view"), **lazy xref repair +
  malformed-file recovery**, **AES-256 R5/R6 + RC4 encrypt *and* decrypt**,
  **font subsetting**, incremental updates, true redaction, AcroForm
  appearance generation, signature byte-range verification, PDF/A-1b/2b
  validation, and both a CPU (tiny-skia) and GPU (wgpu) renderer.  If the
  first three hold it retires the qpdf plan *and* the pdf_oxide writer gap,
  and the renderer could replace the bundled PDFium for `pdf-render`.
  **Verify before believing any of it**: `pdf_oxide`'s
  `writer/linearization.rs` is 696 lines whose own docs say "reserved,
  no-op", with `options.linearize` never read.  Plan: wire it behind an
  off-by-default feature and point `scripts/verify_pdf_independent.py` at
  the four claims that matter — decrypt with a real user password (content
  compared, not just "parses"), linearize and have `qpdf --check` confirm it,
  repair a deliberately-corrupted xref, subset a font and confirm the glyphs
  still render.  A feature list is not evidence; the harness is.
- [x] **`pdfk` 0.3.0** (MIT, 81 downloads) — **read, do not link.**  CLI-only
  (no `[lib]`), so it cannot be a dependency.  Its value is its manifest:
  `lopdf` 0.39 + raw `aes`/`sha2`/`md-5`/`rc4`/`cbc`/`ecb`, and it claims
  working `unlock` for user passwords including AES-256 in ~2,000 lines.
  That is the item we recorded as blocked — and it says the way through is
  *not* lopdf's own decryption (which only ever tries the empty password
  during `load`) but deriving the key from `/Encrypt` ourselves, decrypting
  the streams and strings, stripping `/Encrypt`, then letting lopdf write.
  No new dependency required: every crypto primitive is already in the tree
  for our AES-256 *encryption* path.
- [x] **`pdfbull` 0.10.5** (MIT) — **rejected.**  "A lightweight PDF reader",
  first published 2026-07-28, **8 downloads total**.  We already read PDFs
  three ways (lopdf, pdf-extract, PDFium).  Nothing to gain.

- [~] **P32.6 — qpdf backend.**  RESCOPED (2026-07-29); compression
  SHIPPED end-to-end (2026-07-30). AES-256 and
  object-stream compression turned out to be available in pure Rust —
  lopdf 0.38 already ships `EncryptionVersion::V5` (AESV3) and
  `Aes256CryptFilter`, so the P27.13 note about waiting for lopdf to
  expose CryptFilter was stale. AES-256 is now the default handler.
  `pdf_compress` deflates unfiltered streams and packs objects into
  `/ObjStm` + an xref stream (both are needed — packed objects are
  unreachable from a classic xref table), verifying page count and
  catalog reachability before reporting success. Surfaces: Tauri
  `pdf_compress`, CLI `pdf compress [--no-object-streams]
  [--no-stream-compression] [--max-objects-per-stream N]`, and a
  Reduce-file-size panel in `PdfEditor.svelte`. Images are deliberately
  untouched.
  Only linearization and xref repair still need qpdf; deferred, since
  they are conveniences rather than correctness and qpdf is the only
  non-Rust dependency in the whole plan.  Unblocks the P27.13 deferral:
  `encrypt_pdf` ships RC4-128 because lopdf does not expose
  `CryptFilter` publicly, and RC4 is broken.  QPDF gives AES-256 today,
  plus linearization for fast web view, object-stream compression, and
  structural repair of damaged files.  One Apache-2.0 C++ dependency,
  statically linked.

- [x] **P32.7 — Redaction hardening.**  ✅ SHIPPED (2026-07-29).  `pdf_ops::redact_regions` only
  overlays black rectangles; the text objects survive underneath and are
  recoverable by copy/paste or `pdf-extract`.  The doc comment says so
  but the command is named `pdf_redact_regions` and the UI offers it as
  Redact.  Immediate: relabel command and UI as black-out / visual-only.
  Then: scrub the intersecting `Tj`/`TJ` operators so it is real.

- [x] **P32.8 — On-page text editing.**  ✅ SHIPPED tiers 1-2
  (2026-07-29). Tier 3 (reflow) still deferred.  Tier 1: cover-and-overprint,
  using the black-rect primitive from `redact_regions` and the base-14
  Helvetica text drawing already in `add_watermark`.  Tier 2: `Tj`/`TJ`
  string substitution via `lopdf`, restricted to same-font same-metrics
  replacements.  Tier 3 (full reflow with font subsetting and line
  breaking) is a substantially larger project and is deferred.

- [x] **P32.9 — Text regions.**  ✅ SHIPPED (2026-07-29; CLI + GUI
  2026-07-30).  `pdf_ops::add_text_box_doc` places text at a point and
  breaks only where the caller already put a newline, so laying out a
  paragraph meant knowing in advance where every line ends.
  `pdf_text_region` takes a rectangle instead and wraps into it, with
  horizontal alignment (including justification via `Tw`), vertical
  alignment, line spacing, colour and an optional positioning border.
  Wrapping needs real glyph widths, so `pdf_base14` carries per-glyph
  widths for the base-14 faces keyed by **WinAnsi code via glyph name** —
  the AFM metrics are Adobe-Standard-encoded, and indexing them by code
  made every accented character (`Ü ä ß ï`) look unsupported.
  Overflow is reported and *not* drawn, and characters the face has no
  glyph for are reported rather than silently dropped.  Surfaces: Tauri
  `pdf_draw_text_regions` + `pdf_measure_text_region` (the measure half
  is what lets the GUI preview the fit before committing), CLI
  `pdf text-region --rect page,x,y,w,h --text T [--font …] [--size N]
  [--align left|center|right|justify] [--valign …] [--line-height N]
  [--color r,g,b] [--border]`, and a Text-region panel in
  `PdfEditor.svelte` with a live line-count / overflow preview.
  Tier 3 reflow (line breaking across existing content, font subsetting)
  remains out of scope.

- [x] **P32.10 — Plain text extraction surface.**  ✅ SHIPPED
  (2026-07-30).  Text extraction existed only behind `crispsorter ocr`,
  which reads an embedded text layer when there is one but is named for
  the job it does when there is not.  `pdf text <file> [--out F]` reads
  the text layer through `extractors::extract_text_from_path` and says
  so when it finds none, pointing at `ocr` for scans.

### P33 — Native cloud drives, so Filen/Internxt can leave the Python CLI (planned, 2026-07-31)

The Filen and Internxt drives work by spawning a user-installed Python
`cli.py` (`drives/{filen,internxt}.rs`).  That design has a hard ceiling:

* **iOS cannot run them at all.**  The sandbox denies `fork`/`exec`, so the
  `posix_spawn` behind `std::process::Command` fails with EPERM, and there
  is no interpreter we are permitted to execute — App Review 2.5.2 also
  bars shipping one to run third-party code.  Until 2026-07-31 the drive
  picker offered both kinds on iOS and they failed at use time with a raw
  spawn error; both are now gated off mobile in Rust
  (`drives::ensure_subprocess_drives_supported`) and in the UI
  (`isDesktop()` in `IndexIngest.svelte`), with `platform.ts` taught to
  recognise iPadOS' Macintosh user-agent via `maxTouchPoints`.
* **The Mac App Store build is doubtful.**  A sandboxed child inherits the
  sandbox, and exec'ing a user-chosen interpreter outside the container
  needs a temporary-exception entitlement reviewers dislike; 2.5.2 applies
  again because the functionality lives in a binary we neither ship nor
  sign.
* **Even on desktop it costs the user a Python install** plus a patched
  `cli.py`, which is the single worst step in our setup instructions.

Native Rust clients fix all three.  The licence gate from P32 applies: a
third-party **AGPL** dependency permanently blocks App Store distribution,
because the AGPLv3 §7 extra permission we grant Apple can only be granted
for code we own.

**Where this landed, after a longer detour than it needed
(2026-07-31):**

* **Filen → port from [`filen-sdk-go`](https://github.com/FilenCloudDienste/filen-sdk-go)**,
  Filen's own SDK under **MIT**.  Their TS and Rust SDKs are AGPL and look
  like a hard block; the Go one is not.  Nothing else about Filen matters
  once you know that.
* **Internxt → port from our own `../internxt-dart`**, cross-checked
  against [`internxt-core`](https://github.com/Bebbssos/internxt-core-rust)
  (MIT).  There is no official Internxt Rust or Go SDK; theirs is
  TypeScript.

The clients we already own stay useful either way — as the Internxt source,
and as black-box oracles for both.  Four repos, all `CrispStrobe`, sole
author:

| Ours | Licence | Size | Notes |
|---|---|---|---|
| `../internxt-dart` | MPL-2.0 | 9,184 LOC | most complete: `auth`, `internxt_client`, `drive`, `upload`, `download`, `cache`, `paths` |
| `../filen-dart` | MPL-2.0 | 6,882 LOC | `auth`, `filen_client`, `credential_crypto`, `aes_gcm_backend`, `bcrypt_aesgcm`, `openssl_aesgcm` |
| `../internxt-cli` (= `CrispStrobe/internxt-python`) | AGPL-3.0 | 3,306 LOC | what `drives/internxt.rs` shells out to today |
| `../filen-python` | AGPL-3.0 | 1,802 LOC | what `drives/filen.rs` shells out to today |

Because we hold the copyright, the AGPL on the two Python clients is a
grant we made to the public and does not bind us — the Rust port can carry
whatever licence the App Store needs.

#### Are our own clients clean enough to port from?

Asked because it matters: if `filen-dart` were a derivative of Filen's
**AGPL** TS SDK, porting it to Rust would carry the taint into an App Store
binary — and its 2026-07-16 relicence AGPL-3.0 → MPL-2.0 would itself be a
step only an original author may take.

Audited 2026-07-31, and it comes out **moot for both drives**, for a reason
better than a favourable reading of the evidence: *both vendors publish MIT
references.*  Filen's own Go SDK is MIT and is now the Filen port source, so
`filen-dart`'s history stops mattering.  Internxt's own
[`internxt/sdk`](https://github.com/internxt/sdk) is **MIT** (TypeScript,
pushed 2026-07-29) and so is `internxt/drive-desktop` — so even if
`internxt-dart` did derive from them, MIT permits exactly that.  The only
combination that could ever have bitten was Filen-derived-from-AGPL, and
that is the one we are no longer relying on.

For the record, the evidence pointed to independent work anyway: no commit
message or doc claims a port; `filen-dart`'s HISTORY describes its modules
being *extracted from its own earlier monolith* "following internxt-dart's
architecture pattern", i.e. from another of our repos; the single
acknowledged borrowing is a constant ("Mirrors filen-sdk-ts's
`MAX_UPLOAD_THREADS`"); and names like `encryptMetadata002` track Filen's
own on-wire metadata format version.  Constants, endpoints and format
identifiers are functional protocol facts — the unproblematic category.
What could not be recovered from the repository is the one fact that would
have settled it: what was open while the code was written.

So the discipline is cheap insurance rather than a load-bearing assumption:
port from the vendors' MIT SDKs, and use our own clients **black-box** — run
them, compare bytes.  Observing behaviour creates no derivative work, and an
oracle is more valuable that way regardless, because it catches wire-format
mismatches that reading the code would faithfully reproduce.  CrispSorter
already talks to the Python ones in production, which makes them convenient.
(Separately worth confirming some day: if
`filen-dart`'s provenance is *not* clean, the exposure is that relicence,
not this port.)

Third-party crates therefore drop to optional cross-references, and one is
a trap:

| Third-party | Licence | Use |
|---|---|---|
| **[`internxt/sdk`](https://github.com/internxt/sdk)** | **MIT** | Internxt's *own* SDK (TypeScript, pushed 2026-07-29) — the authoritative reference for their protocol and crypto; `internxt/drive-desktop` is MIT too.  No official Rust or Go equivalent, which is why P33.1 ports from our Dart rather than from a vendor SDK directly |
| [`internxt-core`](https://github.com/Bebbssos/internxt-core-rust) 0.1.3 | MIT | current (updated 2026-07-24), `reqwest ^0.13`, pure-Rust crypto, good seams (`ProgressSink`, injected 2FA). Viable shortcut or cross-check |
| [`rust-filen`](https://github.com/EnoughTea/rust-filen) 0.3.0 | MIT | 352 commits, real crypto (master keys, metadata, RSA, link keys) — but targets `/v1/`; author: *"there is /v3/ API already… chances are, it's even more janky than before"*.  Useful as Rust-shaped prior art, not as a base |
| **[`filen-sdk-go`](https://github.com/FilenCloudDienste/filen-sdk-go)** | **MIT** | ⭐ **The one to port from.** Filen's *own* SDK, MIT, current (pushed 2026-04-03), 61 Go files / 202 KB: `filen/crypto/crypto.go` (21 KB) is the authoritative crypto, `client/v3_login.go` the v3 auth, `upload.go`+`download.go` the transfers, and `main_test.go` (32 KB) supplies expected-behaviour vectors.  Official *and* permissive *and* small |
| [`filen-rclone`](https://github.com/FilenCloudDienste/filen-rclone) | MIT | Filen's rclone backend, also theirs — a second MIT usage example of the SDK above |
| [`go-filen`](https://github.com/ybkimm/go-filen) | MIT | third-party `/v3/` client (2023) — superseded as a reference by Filen's own Go SDK |
| [Filen API docs](https://docs.filen.io/docs/api/specs/) + [auth guide](https://docs.filen.io/docs/api/guides/authentication/) | official docs | endpoints and concrete params: `/v3/auth/info` → PBKDF2-SHA512, **200,000 iterations, 512-bit output**, hex-split — first half is the master key, second half SHA-512'd into the login password.  Hosts: `gateway.filen.io`, `ingest.filen.io`, `egest.filen.io` |
| `filen-sdk-rs` / `filen-sdk-ts` | **AGPL-3.0** | ⛔ **Do not read while writing the port.**  Not ours, so it cannot ship on the App Store, and deriving from it would taint an otherwise clean implementation |

#### Items

- [x] **P33.0 — Gate the subprocess drives off mobile.**  ✅ SHIPPED
  (2026-07-31).  Guard in `drives::ensure_subprocess_drives_supported`,
  called from the single spawn site in each drive before the `cli.py`
  existence check so the message is about the platform rather than a
  missing path; UI options hidden on mobile; WebDAV named in the error as
  the mobile route to the same storage.

- [x] **P33.1 — Internxt native: continue on our own
  `crates/crisp-internxt-native`.**

  *Settled 2026-07-31 after reading both implementations.* This item was
  written twice before, wrongly: first "port from our `internxt-dart`"
  (drift — the Filen frame applied without re-deriving), then "vendor a fork
  of `internxt-core`" (an evaluation of the crate against a *fragment* of
  our code, before the crate we already had was read).  The comparison that
  matters:

  **What `crisp-internxt-native` already has** (840 LOC + a 190 LOC CLI,
  29 unit tests plus HTTP-harness coverage): the full three-step login (`/auth/login` → `/auth/login/access`
  with 2FA → `/users/refresh`), the OpenSSL `EVP_BytesToKey`/MD5 `Salted__`
  envelope, password→mnemonic decryption, an `InternxtSession` shaped for the
  keychain, `bridge_pass = sha256(user_id)`, NFKD-normalised BIP-39 seed
  derivation, verified file crypto, paginated listing tolerant of both
  `result` and legacy `folders`/`files` shapes, network-bridge download and
  upload (`files/start` → PUT → `files/finish` → `POST /files`,
  `encryptVersion: "Aes03"`), `create_folder` and `trash`.

  **Why it is the better base**, not merely the incumbent:
  * **Sync** (`reqwest::blocking`) — `trait CloudDrive` is sync, so there is
    no runtime bridge.  `internxt-core` is async throughout.
  * **`resolve_path`** — the trait is path-addressed; `internxt-core` is
    UUID-addressed and has no path resolution at all.  That is the single
    largest piece it would not give us.
  * **Dependency alignment** — `aes 0.8`, `ctr 0.9`, `pbkdf2 0.12`,
    `sha2 0.10` match the tree, so no duplicate RustCrypto majors, no
    `tokio` `full`, no `pgp`, no `safe_pqc_kyber`.
  * A **standalone CLI** exercising the same code path, which is exactly the
    lever P33.2 needs.

  **Where [`internxt-core`](https://github.com/Bebbssos/internxt-core-rust)
  (MIT) is genuinely ahead — crib from it, do not adopt it wholesale:**

  | Gap here | Look at |
  |---|---|
  | `upload_file` refuses ≥100 MiB; single-part only | `transfer::upload_stream_to_network` (multipart + streaming) |
  | Whole files buffered in `Vec<u8>` | `transfer::download_file_to_writer` |
  | No token refresh, so sessions expire | `api::refresh_user_token`, `auth::refresh_credentials` |
  | No move/rename | `api::move_file`/`move_folder`, `rename_*` |
  | `resolve_path` re-lists every component, uncached | (nothing — but cache it) |

  Also worth a look when the need arises: SSO (`sso::login`), workspaces
  (`decrypt_workspace_key`), thumbnails, and `ProgressSink` as a model for
  progress reporting.  It is MIT, so reading and lifting individual
  functions is unencumbered.

  The implementation now wires `InternxtDrive` to the crate behind
  `drive-internxt-native`, stores the native session through the OS-keychain
  adapter, supports streaming and multipart transfers with refresh/retry,
  caches and invalidates path listings, and builds with rustls for desktop
  and `aarch64-apple-ios` without OpenSSL.

  `../internxt-dart` stays the **oracle** (P33.2), not a port source.

  (This pass fixes active-token selection after refresh and makes native
  overwrite resolution use the complete parent path. The desktop
  `desktop,drive-internxt-native` Tauri check passes; omitting `desktop` is
  intentionally unsupported because the desktop capability owns shell/process
  permissions. The corresponding iOS check also passes with
  `--no-default-features,drive-internxt-native`.)

- [x] **P33.2 — Verify the crypto against the reference client, not our own
  tests.**  `internxt-core` claims byte-for-byte compatibility with the
  official Node implementation "checked against reference test vectors".
  That claim is exactly what must not be taken on faith: a KDF or metadata
  mismatch produces uploads that succeed and are then unreadable by
  Internxt's own clients, with nothing failing on our side.  Cross-client
  round-trip in both directions — write with Rust, read with the Python
  CLI, and the reverse — as a live-marked test alongside
  `verify_pdf_independent.py`.  This is the zpdf lesson: our
  extract-and-compare test passed a linearizer that emitted a malformed
  xref, because both sides of the comparison were ours.

- [x] **P33.3 — Filen native, ported from Filen's own MIT Go SDK.**  Shipped
  as `crisp-filen` with the `crisp-filen` CLI, keychain-backed Tauri
  adapter, v1/v2/v3 crypto handling, chunked transfers, and both-direction
  live tests against `../filen-python`.  The native crate passes its unit
  tests and builds for `aarch64-apple-ios` with rustls.  The full Tauri
  feature checks now pass for both desktop (`desktop,drive-filen-native`) and
  iOS (`--no-default-features,drive-filen-native`); the capability split keeps
  desktop-only shell/process permissions out of mobile builds.
  Transfers use a pooled rustls client with bounded four-way chunk upload and
  download concurrency, ordered reassembly, request timeouts, and cache
  invalidation after mutations. `TransferConfig` exposes chunk size, worker
  count, file-worker count, retry count, and exponential retry backoff to Rust
  consumers. Reader-based uploads and writer-based downloads expose progress
  callbacks without requiring whole-file buffering. Recursive path transfers,
  bounded path listings, and actionable 2FA gateway errors are also covered.
  Batch transfer APIs, true range downloads, and serializable
  resumable upload state (UUID/upload key/file key/completed chunks) are also
  covered by the native client. Vendor filename-hash vectors from the MIT Go
  SDK's `crypto_test.go` are pinned in the Rust unit suite.
  Source rationale:
  from [`filen-sdk-go`](https://github.com/FilenCloudDienste/filen-sdk-go),
  not from our Dart and not clean-room: it is the vendor's *own*
  implementation, **MIT**, current, and small (202 KB of Go; the subset we
  need — `crypto/crypto.go`, `client/v3_login.go`, `client/v3_dir_content.go`,
  `upload.go`, `download.go` — is well under half of that).  MIT means we
  may read it, port it and ship the result on the App Store, so every
  awkwardness above evaporates: no AGPL block, no clean-room discipline, no
  dependence on `filen-dart`'s provenance, and no reviving the stale
  `/v1/`-era `rust-filen`.  `main_test.go` (32 KB) is a bonus — port its
  vectors alongside the code so the crypto is pinned by the vendor's own
  expectations rather than only by our round-trip.  Keep the
  [API docs](https://docs.filen.io/docs/api/specs/) open for the endpoints
  the SDK leaves implicit, and `filen-rclone` (also theirs, also MIT) as a
  second usage example.  The AGPL `filen-sdk-rs`/`filen-sdk-ts` are now
  simply unnecessary — do not open them; there is no reason to.

  Note this is the *same vendor* shipping the same functionality under two
  licences: TS and Rust are AGPL-3.0, Go and the rclone backend are MIT.
  Whichever one you happen to find first determines whether the feature
  looks impossible or easy.

- [x] **P33.4 — Document WebDAV as the mobile route.**  Both vendors'
  WebDAV gateways run as local daemons, so they do not help on iOS, but any
  *remote* WebDAV works through `drives/webdav.rs` today and is already the
  fallback named in the new guard's error message.  The drive picker now
  explains this on mobile, and the README names remote WebDAV as the route.

- [x] **P33.5 — Close the published `crisp-cloud-rs` parity gaps**
  (Internxt, Filen, and shared facade).

  **Internxt (`crisp-internxt`) — retain its stronger path and CLI surface:**

  - [x] Add generic reader-based upload and writer-based download APIs so
    library callers never need whole-file `Vec<u8>` buffering.
  - [x] Make multipart part size, retry count, backoff, timeout, and worker
    count configurable through a validated `TransferConfig`; keep serial
    multipart as the default and retain explicit concurrent workers.
  - [x] Add persisted resumable download state and resumable recursive
    download state, not only resumable multipart upload state.
  - [x] Add end-to-end content/hash verification and byte-level progress
    callbacks for single-file upload/download operations.
  - [x] Add automatic expired-token detection, refresh, and one safe retry;
    preserve the explicit CLI `refresh` command.
  - [x] Use explicit rustls-only Reqwest features for the published crate and
    verify desktop, macOS, Linux, and iOS builds without OpenSSL.
    (Checkpoint: Internxt now uses explicit rustls and HTTP/1.1 transport;
    desktop, aarch64 macOS, and aarch64 iOS cargo checks pass; no
    `openssl-sys` or `native-tls` dependency is present.)
  - [x] Add unit, local HTTP-harness, cross-client Python/Dart, and live tests
    for every new transfer and refresh path.
    (Rust endpoint/refresh and streaming harness passes with multipart resume
    coverage; presigned PUT diagnostics report per-attempt elapsed time. The
    CZE Internxt live suite passed 4/4, including 100 MiB multipart and
    Python↔Rust interoperability. The Dart↔Rust cross-client test passed 1/1.)

  **Filen (`crisp-filen`) — retain its stronger crypto and transfer engine:**

  - [x] Add token/session refresh and expiry-aware retry behavior, matching
    Internxt's refresh semantics where the Filen gateway permits it.
  - [x] Expand the CLI to expose the library's recursive transfers, filters,
    conflict policies, dry-run inspection, timestamp preservation, progress,
    resumable state, and verbose diagnostics.
  - [x] Add a serial-safe transfer mode and make it the default for fragile
    gateways; keep configurable chunk/file worker concurrency as opt-in.
  - [x] Complete trash parity with listing filters/limits and empty-trash
    behavior where supported by the Filen API.
  - [x] Add a provider-neutral metadata/path result model or conversion layer
    so callers do not have to translate `NativeItem` and `PathListing` by hand.
  - [x] Rename remaining public `FilenNativeClient`/native wording where this
    is source-compatible, while preserving a deprecation path for users of
    0.x APIs.
  - [x] Run the ignored Rust↔Python live matrix against the configured Filen
    account: 3/3 passed (both cross-client directions plus large transfer,
    recursive/timestamp/resume, and mutation coverage; 2026-07-31). No
    gateway-specific failures observed.

  **Shared `crisp-cloud-rs` facade and release quality:**

  - [x] Define a small provider-neutral `CloudDrive` capability trait for
    path resolution, listing, transfers, progress, and mutations; keep
    provider-specific crypto and advanced operations outside the trait.
  - [x] Add shared transfer types for conflict policy, filters, progress,
    cancellation, resume state, and structured error classification.
  - [x] Add cooperative cancellation and bounded concurrency guarantees to
    both backends, with identical progress semantics.
  - [x] Add an async API or an explicitly documented blocking-only boundary;
    do not let the facade imply async portability it does not provide.
  - [x] Move session persistence behind a caller-supplied secret-store trait;
    keep JSON serialization available for CLI/testing but document its
    sensitive contents.
  - [x] Add CI for formatting, clippy, hermetic tests, package manifests,
    cross-platform builds, and ignored live tests gated by explicit secrets.
  - [x] Publish coordinated versions of `crisp-internxt`, `crisp-filen`, and
    `crisp-cloud-rs`; verify README install commands, crates.io metadata,
    GitHub releases, and license notices after every release.

### P34 — CrispCloud capability inventory and scoped file-manager roadmap

Audited against the current `../CrispCloud` `main` branch (pulled/fetched
2026-07-31), its source tree, `README.md`, `HANDOVER.md`, and `PLAN.md`.
CrispCloud is a general-purpose dual-panel cloud file manager; CrispSorter is
primarily a document-intelligence/indexing application.  The goal is not to
clone the whole product indiscriminately.  The goal is to add the file-manager
surface where it makes CrispSorter's search, duplicate, archive, catalog, and
cloud-drive results actionable.

#### Inventory — what CrispCloud provides

- **Provider ecosystem:** Filen, Internxt, SFTP, WebDAV, S3, FTP/FTPS,
  Google Drive, OneDrive/SharePoint, Dropbox, Nextcloud, pCloud, Azure Blob,
  Backblaze B2, and Hetzner Storage Box.  Native capabilities include OAuth,
  shared drives/libraries, content hashes, provider versions, S3 signing and
  presigned URLs, chunked uploads, and provider-specific delta sync.
- **File-manager operations:** dual panels, tabs, tree/list/grid/column views,
  breadcrumbs, bookmarks, recursive browsing, create-folder, copy, move,
  rename, delete, archive browsing, checksums, permissions, symlink handling,
  drag/drop, file associations, reveal-in-Finder/Explorer, and operation
  history/undo.
- **Transfer engine:** streaming upload/download, bounded concurrent queue,
  retry/backoff, cancellation, pause/resume, persistent multipart resume,
  foreground mobile transfers, progress/speed/ETA, and a transfer drawer.
- **Sync/backup:** two-way sync, selective sync, folder watching, offline
  replay, five conflict policies, scheduled versioned backups, integrity
  verification, restore, system-tray status, and sync-pair persistence.
- **Delta sync:** Adler-32 + SHA-256 block maps with end-to-end provider
  integrations for Nextcloud, pCloud, and S3 range reads.
- **Sharing/versioning:** provider-native Google Drive, OneDrive, and Dropbox
  share links, expiry/password options where supported, shared-folder
  management, version listing/restoration, and text version diff.
- **Preview/editing/search:** image/SVG/PDF/code/Markdown/CSV/DOCX/XLSX/PPTX/
  ODT/font/audio/video preview, in-place remote editor, provider full-text
  search, saved searches, virtual search-result folders, duplicate finder,
  and LCS diff viewer.
- **Security/privacy:** AES-GCM provider wrapper, Cryptomator, VeraCrypt,
  certificate pinning, custom CAs, proxy/SOCKS5, TLS policy, app lock,
  biometrics, secure clipboard, screenshot blocking, audit log, and privacy
  indicators.
- **Extensibility/platform:** Dart CLI with cloud operations and completions,
  local REST API, plugins, automation rules/webhooks/cron, FUSE, Android
  DocumentsProvider, iOS FileProvider/share extension/Siri, desktop shell
  integrations, system tray, PWA/File System Access/OPFS/offline support,
  crash reporting, auto-update, and nine languages.
- **Quality:** roughly 4,500 Flutter tests (including provider mocks, widget,
  golden, fuzz, benchmark, integration, and gated live tests), plus CI builds
  for its supported platforms.  The README/HANDOVER numbers differ slightly;
  treat the repository's current test run as authoritative.

#### CrispSorter strengths to preserve

CrispSorter is ahead for local and federated document intelligence: LanceDB +
Tantivy hybrid/RRF search, dense/sparse/ColBERT/multimodal retrieval, OCR and
layout extraction, audio/video transcription, translation, document-type
classification, PDF manipulation, DOCX tooling, DMS metadata/audit/retention,
`.caf`/`.cidx` archives, cb-api remote search, and the native Internxt/Filen
Rust clients.  These remain the product centre of gravity.  A file-manager
surface is valuable because it turns those results into operations: show a
hit in its remote/local context, compare or open duplicates side-by-side,
move/rename/archive the selected result, promote an archive entry, and start
a transfer without leaving the search/catalog workflow.

#### P34.1 — Foundations, P0/P1 (do before broad UI)

- [x] **Capability-aware CloudDrive API (first slice).** Explicit capability
  discovery now exists, with safe unsupported-operation errors and LocalDrive
  implementations for recursive copy, move/rename, and create-directory.
  Remaining providers and streaming fields still need implementation; the
  follow-up item below owns that work.
- [ ] **Complete provider capability-aware API.** Add the remaining safe
  primitives (`create_dir`, `rename`, `move`, `copy`, optional recursive
  listing, streaming reader/writer) while preserving the current synchronous
  trait boundary for legacy providers.  Do not fake support: unsupported
  operations must be reported by capability, not discovered by a late HTTP
  failure.
  - [x] WebDAV now advertises and implements `create_dir`, `move_path`, and
    `copy_path` via RFC 4918 MKCOL/MOVE/COPY, with destination and overwrite
    semantics covered by mock-server unit tests. ✅ 2026-07-31
  - [x] Google Drive now advertises and implements folder creation, rename/
    move, and copy through Drive API v3 `files` mutations. ✅ 2026-07-31
  - [x] OneDrive now advertises and implements folder creation and
    rename/move through Graph mutations. Server-side copy now resolves the
    destination folder id, submits Graph's asynchronous copy, and polls its
    monitor URL to completion with bounded timeout/error handling. ✅ 2026-08-01
  - [x] Filen’s Python CLI adapter now exposes folder creation, rename,
    move, and copy using its `mkdir`/`rename`/`mv`/`cp` commands. Internxt’s
    Python CLI now exposes `rename` and `mv`; the Rust subprocess adapter
    advertises create/rename/move, while copy remains false because neither
    the Python CLI nor the official Go adapter provides native copy. ✅ 2026-07-31
  - [x] Native Internxt Rust adapter now advertises and implements folder
    creation, rename, move, and copy by delegating to the already-tested
    `InternxtNativeClient` mutations. File copies preserve the provider’s
    plain-name/type split and apply a follow-up rename when the destination
    leaf changes. ✅ 2026-07-31
  - [x] Native Filen Rust adapter now advertises and implements folder
    creation, rename, move, and recursive copy through the native client;
    its capability declaration is covered without credentials or network.
    ✅ 2026-08-01
  - [x] **Official Internxt adapter comparison.** The official Go
    [`internxt/rclone-adapter`](https://github.com/internxt/rclone-adapter)
    implements file/folder create, delete, rename, and move, but has no
    provider-level copy operation. Its multipart/streaming/range transfer
    code is materially ahead of the subprocess wrapper. The separate
    [`internxt/rclone`](https://github.com/internxt/rclone) fork supplies
    generic rclone `copy`/`sync` orchestration; that does not mean Internxt’s
    provider API has native copy. Our Python and Dart Internxt clients do
    expose copy, so the Rust subprocess adapter now matches create/rename/
    move; copy remains the next intentional gap if we choose to add
    client-side orchestration.
- [x] **One shared application TransferQueue (GUI slice).** AppState now owns
  one bounded queue shared by GUI drive reads/writes, cloud-backup upload/
  restore, and index archive promotion. ✅ Shipped 2026-07-31.
- [x] **Queue job registry and cancellation surface.** The shared queue now
  retains a bounded recent-job snapshot and exposes Tauri status/cancel
  commands for the future transfer drawer. ✅ Shipped 2026-07-31.
- [x] **Complete shared TransferQueue integration.** ✅ 2026-08-01. CLI, FUSE,
  and provider-facing boundaries now use the process-wide queue; registration,
  polling snapshots, retry/backoff, cancellation, and the bounded synchronous
  adapter are wired. Serial multipart defaults remain provider-controlled.
  - [x] Added a process-wide `TransferQueue::shared()` accessor and routed
    AppState, CLI cloud-backup transfers, and FUSE construction through it;
    they now share semaphore/backoff/cancellation/job snapshots. ✅ 2026-08-01
- [x] **Bounded synchronous queue adapter.** `upload_blocking` and
  `download_blocking` run synchronous provider/FUSE operations on an isolated
  Tokio worker while sharing the queue semaphore, retry policy, cancellation
  registry, and terminal-job snapshots. ✅ 2026-07-31
- [x] **FUSE read boundary.** `FuseDriveFs` now routes full-file reads through
  the blocking download adapter while retaining its synchronous read-only
  filesystem contract. ✅ 2026-07-31
- [x] **Streaming and durable resume.** ✅ 2026-08-01. Reader/writer transfers,
  persisted provider/session/key/chunk checkpoints, compatibility refusal, and
  100 MiB+ native-provider coverage are now exposed through the app facade.
  - [x] Object-safe reader/writer methods now exist on `CloudDrive`; native
    Filen and Internxt adapters use their bounded streaming APIs and advertise
    `streaming`, while legacy providers retain checked fallbacks. ✅ 2026-07-31
  - [x] Native resume state is now exposed through `CloudDrive::upload_file_resumable`
    and `drive_upload_resumable`; Filen and Internxt validate the persisted
    destination/source identity before continuing, while legacy providers
    fail explicitly. ✅ 2026-08-01
  - [x] Per-chunk native download checkpoints are now exposed through
    `CloudDrive::download_file_resumable` and `drive_download_resumable`.
    Internxt uses ranged encrypted downloads; Filen persists its remote file
    identity, metadata, partial destination, and completed decrypted chunks.
    Incompatible state is discarded rather than applied to a replacement.
    ✅ 2026-08-01
  - [x] Added an ignored Filen live 100 MiB+1 byte resumable upload/download
    round-trip with persisted upload and download checkpoints; it requires
    explicit `FILEN_EMAIL`/`FILEN_PASSWORD` and never discovers credentials.
    ✅ 2026-08-01
- [x] **Provider capability matrix and test harness.** ✅ 2026-08-01. Add local mock HTTP
  servers and contract tests for listing, mutation, streaming, retries,
  resume, expired auth, share/version behavior, and unsupported operations.
  Keep CZE live tests for Internxt and the configured Filen account gated by
  explicit environment variables; never use keychain discovery in unit tests.
  - [x] Added a pure provider capability-matrix contract covering all
    non-native drive constructors without network, subprocess, or credential
    access. ✅ 2026-07-31
  - [x] Capability discovery now explicitly advertises native resumable
    upload/download support alongside streaming; Filen and Internxt native
    adapters report all three, while subprocess and legacy providers report
    unsupported instead of requiring a late operation probe. ✅ 2026-08-01
  - [x] Provider-neutral contract tests now pin the legacy fallback boundary:
    streaming readers reject short/overlong input, and resumable operations
    return explicit unsupported errors without probing a provider. ✅ 2026-08-01
  - [x] Google Drive and OneDrive now have ignored-by-default live round-trip
    tests covering write/read/stat/version listing and cleanup; they require
    explicit access-token environment variables and never consult keychains.
    ✅ 2026-08-01
  - [x] The contract suite now includes a credential-free stub backend that
    verifies every default unsupported mutation/share/version operation fails
    or returns the documented empty result in lockstep with capabilities.
    ✅ 2026-08-01
- [x] **Offline queue integration.** ✅ 2026-08-01. On exhausted transfer/network failure,
  persist a replayable operation with provider/path/state and expose retry,
  cancel, inspect, and purge commands.  Add reconnect replay through the
  shared queue with exponential polling backoff.
  - [x] Durable inspect/list/retry-failed/purge-failed Tauri commands now
    expose the SQLite offline queue without requiring keychain or network
    access. ✅ 2026-08-01
  - [x] Failed GUI drive uploads now stage their bytes durably and
    `offline_queue_replay` retries them through the shared TransferQueue.
    ✅ 2026-08-01
  - [x] App startup maintenance now replays pending staged operations every
    30 seconds; failures remain pending for the next connectivity window.
    ✅ 2026-08-01
  - [x] Offline replay now records unknown operation types as terminal
    failures instead of silently leaving them pending forever; the original
    payload and diagnostic remain inspectable for a newer replay handler.
    ✅ 2026-08-01

#### P34.2 — Core file-manager surface, P1

- [ ] **Contextual dual-panel mode.** Add a focused dual-panel workspace,
  not a separate generic app: left panel is a local/registered-drive folder
  or search-result context; right panel is the selected document, duplicate
  group, archive, cloud folder, or comparison target.  Support panel source
  types `LocalPath`, `CloudDrive`, `SearchResults`, `DuplicateGroup`,
  `CatalogArchive`, and `RemoteSearchResults`.
  - [x] The right context pane now renders provenance details for all six
    typed panel sources, including local paths, catalog archives, and remote
    search provider/query context. ✅ 2026-08-01
- [ ] **Actionable search results.** From any result: reveal/open in context,
  select related chunks/duplicates, compare files, copy/move/rename/delete,
  share, promote remote L1 rows, download for offline use, and send selected
  items to the batch sorter.  Preserve provenance (`crisp+drive://`,
  `crisp+cb-archive://`, local path) through every action.
  - [x] Search results with `crisp+drive://` provenance can open the
    registered-drive browser at the exact remote path through a shared typed
    context request. ✅ 2026-08-01
  - [x] Local search results now expose a provenance-safe action that adds
    the local file to the existing batch sorter; cloud/archive/HTTP URIs are
    rejected instead of being treated as local paths. ✅ 2026-08-01
  - [x] Remote cloud-backup result panes now emit typed provider/query context
    requests into the dual-panel browser. ✅ 2026-08-01
  - [x] The Catalog browser now emits typed `CatalogArchive` context for the
    currently opened `.caf`, preserving archive provenance in the right pane.
    ✅ 2026-08-01
  - [x] Local search hits now emit typed `LocalPath` context requests alongside
    the existing external-open and batch-sorter actions. ✅ 2026-08-01
  - [x] Local result groups now emit typed `SearchResults` context requests
    carrying the active query into the dual-panel browser. ✅ 2026-08-01
- [ ] **Duplicate workflow.** Show duplicate groups side-by-side with size,
  hashes, locations, provider, indexed state, and document metadata.  Offer
  safe keep/delete/move/archive actions with dry-run, conflict policy,
  trash-first behavior, and an undo/audit record.
  - [x] Duplicate-match rows now emit typed `DuplicateGroup` context requests
    into the browser surface; group identity is derived from the source path
    and result row, preserving provenance for the forthcoming side-by-side
    duplicate actions. ✅ 2026-08-01
  - [x] Duplicate context requests now carry source/destination paths, sizes,
    hashes, and roles; the browser pane renders the complete group for review.
    ✅ 2026-08-01
  - [x] Duplicate context now also renders modification times and provides
    safe open-file and copy-path actions for every candidate. ✅ 2026-08-01
  - [x] Duplicate context now records a non-destructive dry-run decision
    (review, keep source, keep destination, or keep both) before mutation
    policies are implemented. ✅ 2026-08-01
  - [x] The browser persists a bounded versioned audit of decision changes in
    local storage and supports undoing the latest decision for the active
    duplicate group; destructive mutation support remains deferred. ✅ 2026-08-01
  - [x] Reopening a duplicate context restores the latest persisted decision
    for that group instead of resetting to review. ✅ 2026-08-01
  - [x] The duplicate pane exposes persisted decision history with timestamps
    and an explicit clear-audit control. ✅ 2026-08-01
- [ ] **Minimal file-manager operations.** Implement folder context, create
  directory, rename, move/copy, delete/trash, refresh, breadcrumbs, and
  selection.  Defer the full Double Commander keyboard surface until these
  operations work across LocalDrive and at least Internxt/Filen/WebDAV.
  - [x] DriveBrowser now exposes capability-gated create-folder, rename,
    move, copy, delete, refresh, breadcrumb navigation, and selection actions;
    rename delegates to the provider's tested move/rename primitive.
  - [x] Added the missing `drive_delete_path` Tauri command, distinct from
    drive-registration deletion, with capability gating and provider error
    propagation. ✅ 2026-08-01
  - [x] Added the first registered-drive browser tab with breadcrumbs,
    refresh, folder creation, move/copy, and delete actions. It consumes the
    capability-aware Tauri boundary; dual-panel context and search-result
    integration remain separate follow-up work. ✅ 2026-08-01
  - [x] Added typed contextual panel sources and a right-hand selection pane
    to the drive browser. Cloud-drive provenance is retained; search results,
    duplicate groups, catalog archives, and remote-search sources are modeled
    for the next integrations. ✅ 2026-08-01
- [ ] **Transfer drawer and status surface.** Show queued/active/retrying/
  failed/completed jobs, provider, path, bytes, speed, ETA, retry state,
  cancellation, and resume availability.  Reuse the existing frontend log
  and i18n infrastructure rather than creating a second notification system.
  - [x] Drawer now includes provider identity and live ETA alongside bytes,
    speed, state, cancellation, and offline retry details. ✅ 2026-08-01
  - [x] Drawer now queries and caches the registered drive's capability
    declaration and shows "resume available" only for the matching native
    upload/download direction. Unknown providers remain unmarked. ✅ 2026-08-01
  - [x] Queue-native resume-state metadata is now carried as optional
    provider checkpoint paths in `TransferProgress` and rendered separately
    from the capability badge; existing callers remain non-resumable unless
    they explicitly submit a checkpoint-aware job. ✅ 2026-08-01

#### P34.3 — Sync, backup, and collaboration, P1/P2

- [ ] **General sync pairs.** Add local-folder ↔ CloudDrive pairs with
  include/exclude globs, one-way/two-way modes, watcher integration, and
  persisted watermarks.  Keep cloud-backup shard sync as a separate mode.
  - [x] Persisted sync-pair definitions now cover local/remote roots,
    registered drive, direction, filters, enabled state, and a resumable
    watermark; Tauri list/upsert/delete commands and SQLite round-trip tests
    are in place. The transfer runner and watcher dispatch remain the next
    scoped step. ✅ 2026-08-01
  - [x] Added a read-only deterministic local planner with `**`, `*`, and `?`
    filter semantics plus a `sync_pair_plan` Tauri command. It emits sorted
    file metadata without contacting a provider or advancing the watermark;
    transfer execution remains the next step. ✅ 2026-08-01
  - [x] Added explicit local→cloud `sync_pair_push` execution for `ToCloud`
    and `TwoWay` pairs, with dry-run mode, provider write-capability checks,
    shared TransferQueue retries, and watermark advancement only after each
    successful upload. Remote comparison and reverse direction remain
    deferred to conflict-aware sync. ✅ 2026-08-01
  - [x] Pushes now use the persisted watermark as an incremental cutoff;
    first-run watermark `0` uploads all matching files, while later runs
    recheck entries at the inclusive second-resolution boundary so same-second
    edits cannot be missed, advancing only after success. ✅ 2026-08-01
  - [x] Folder watchers now emit a debounced `folder-watch:sync-pair-candidate`
    event carrying the changed path and watched root. It is advisory only;
    remote mutation still requires an explicit sync-pair push. ✅ 2026-08-01
  - [x] Explicit sync-pair pushes now persist a bounded audit ledger with
    dry-run, no-change, and completed outcomes, counts, watermark, and
    timestamps; recent runs are exposed through `sync_pair_runs`. ✅ 2026-08-01
  - [x] The run ledger now records upload and download counts; existing
    databases migrate the new `downloaded` column automatically, and Tauri/
    CLI pulls record dry-run/no-change/completed outcomes. ✅ 2026-08-01
  - [x] Headless CLI parity now exposes `sync pair list`, `plan`, and `runs`
    for inspecting configured pairs, filtered local snapshots, and audit
    history without touching cloud-backup commands. ✅ 2026-08-01
  - [x] CLI now also supports `sync pair push <id> [--dry-run]`, using the
    same incremental watermark and shared TransferQueue upload path as the
    Tauri command. ✅ 2026-08-01
  - [x] CLI now exposes `sync pair remote-plan` and `sync pair compare
    --policy`, reusing the provider inventory and metadata comparison path
    for headless conflict review. ✅ 2026-08-01
- [ ] **Conflict policies.** Wire newest/local/remote/keep-both/manual into
  sync and file-manager mutations.  Add a manual conflict review panel with
  local/remote metadata, hashes, preview, and explicit resolution actions.
  - [x] `ConflictPolicy` is now persisted in `IndexConfig` with a backward-
    compatible default, and Tauri get/set commands expose one authoritative
    policy for sync integrations. ✅ 2026-08-01
  - [x] Settings now loads and edits the five policies and persists the choice
    through both the index config and the dedicated sync command. ✅ 2026-08-01
  - [x] Sync-pair pushes now accept an explicit conflict policy and reject
    remote-wins, keep-both, manual, and newest-wins until remote metadata is
    available; only local-wins may safely overwrite in the current local-only
    comparison boundary. ✅ 2026-08-01
  - [x] Added read-only `sync_pair_remote_plan` inventory for providers that
    advertise listing/stat, collecting filtered remote paths, sizes, and
    modification times without download or mutation. This is the metadata
    input for the remaining conflict-policy resolver. ✅ 2026-08-01
  - [x] Added pure metadata comparison and `sync_pair_compare`, classifying
    local-only, remote-only, unchanged, and divergent paths under explicit
    newest/local/remote/keep-both/manual policies without mutating either side.
    ✅ 2026-08-01
  - [x] Added credential-free recursive remote-inventory coverage using
    `LocalDrive` as a provider double; filters, sorting, size, and mtime are
    asserted without network or keychain access. ✅ 2026-08-01
  - [x] Added explicit remote→local `sync_pair_pull` for `ToLocal` and
    `TwoWay` pairs. It requires remote-wins, uses provider read/list/stat and
    the shared transfer queue, applies the remote watermark cutoff, and writes
    local files only after successful downloads. ✅ 2026-08-01
  - [x] CLI parity now exposes `sync pair pull <id> [--dry-run]` with the
    same remote-wins guard, inventory cutoff, shared queue, and local-write
    behavior. ✅ 2026-08-01
- [ ] **End-to-end delta protocol.** Complete cb-api blockmap/changed-block/
  finalize endpoints and `push --delta`; integrate providers only where their
  APIs support random access or range reads.  Keep whole-file fallback for
  Internxt, Filen, and generic WebDAV until proven safe.
  - [>] Nextcloud / ownCloud WebDAV boundary. Both providers are usable
    through WebDavDrive (remote.php DAV roots, Basic/app-password auth,
    PROPFIND, MKCOL, MOVE, COPY, and OCS sharing). The actual CrispCloud
    delta protocol is now implemented: detect
    /index.php/apps/crispcloud_delta/api/status, fetch
    /api/blockmap/{path}, POST changed blocks to /api/blocks/{path}, and
    POST /api/finalize/{path}?size=N. ETag-cached server maps select changed
    blocks; absent app/map falls back to normal full WebDAV transfer.
    Strict Range validation protects delta download. The shared
    crispcloud_delta server now accepts optional If-Match validators and Rust
    sends the fetched ETag on every block/finalize mutation; stale maps return
    HTTP 412 without mutation.
  - [x] Add gated live Nextcloud and ownCloud coverage for app detection,
    authenticated blockmap fetch, one-block replacement, shrink/grow
    finalize, strict range delta download, and round-trip content
    verification. The tests use an SSH tunnel to the isolated VPS instances
    and explicit environment credentials; they never discover credentials
    from keychains. Plain-WebDAV full-upload fallback is unit-tested.
  - [x] Extend the live matrix with stale ETag/concurrent-update behavior:
    both Nextcloud and ownCloud return HTTP 412 for stale block and finalize
    mutations, while the existing resize and round-trip cases remain green.
    ✅ 2026-08-01
  - [x] Add OCS share-link coverage for both Nextcloud and ownCloud. The
    client now requests `format=json` (required by ownCloud) and accepts both
    OCS success status codes used by the two servers; hermetic request/response
    and live create/delete tests pass. ✅ 2026-08-01
  - [x] Complete server-side staged finalize semantics in the patched desktop
    clients' shared PHP handler. Changed blocks are staged under a per-file
    lock, every mutation validates If-Match, and finalize applies the complete
    result with one content write before rebuilding the block map and clearing
    staging. The live 9/9 delta suites pass on both Nextcloud and ownCloud,
    including replacement, shrink, grow, and stale-ETag rejection.
    ✅ 2026-08-01
  - [x] Validate staged-finalize visibility from desktop-client readers. The
    live suite now confirms on both providers that readers see the committed
    old file between block staging and finalize, then see all changed bytes
    after finalize; both suites pass 11/11. ✅ 2026-08-01
  - [x] Add an in-flight concurrent-reader stress test that races eight
    readers against finalize. The PHP handler retries transient provider file
    locks for a bounded period, and the live suite confirms that all readers
    observe either the complete old or complete new file: 12/12 on both
    Nextcloud and ownCloud. ✅ 2026-08-01
- [ ] **Share/version commands.** Expose `drive_list_versions` and
  `drive_restore_version`; add Google/OneDrive response mocks, then add
  WebDAV/Nextcloud detection only when the server advertises OCS sharing.
  Add expiry/password options only to providers that actually support them.
  - [x] `drive_list_versions` and `drive_restore_version` are now exposed
    through the capability-aware Tauri boundary; unsupported providers fail
    before network access. Google Drive and OneDrive now also have hermetic
    response/request contract coverage for listing and restore; Google’s
    upload endpoint is injectable alongside its API endpoint. ✅ 2026-08-01
  - [x] WebDAV capability discovery now probes only Nextcloud/ownCloud-style
    `remote.php` roots and enables `share_links` only for an OCS success
    response. Generic WebDAV, offline servers, and rejected OCS requests
    remain unsupported; hermetic positive and negative probe tests pass.
    ✅ 2026-08-01
- [x] **Backup UX.** Add scheduled local/cloud backup configuration, integrity
  verification, restore selection, retention, and visible history.  Reuse
  cloud-backup shard machinery where possible instead of duplicating it.
  - [x] Persist validated backup-job definitions (source root, drive, remote
    root, manual/interval/daily schedule, retention, integrity flag, enabled
    state) alongside shard watermarks; expose Tauri list/upsert/delete and
    CLI `sync backup-job list|upsert|delete`. Execution remains explicit until
    the scheduler and provider-independent restore contract are specified.
  - [x] Add scheduler/execution service with crash-safe run records and
    retention enforcement; reuse `cloud_backup` shard export/import and the
    shared transfer queue.
    - [x] Added durable run lifecycle records, bounded history inspection, and
      restart recovery (`running` → `interrupted`) with Tauri and CLI history
      surfaces. Scheduler/execution wiring remains deferred.
    - [x] Added explicit CLI `sync backup-job run <id> [--dry-run]` execution:
      recursive local enumeration, capability checks, queued uploads, optional
      post-upload size verification, and durable success/failure accounting.
    - [x] Explicit runs now write to UTC date snapshots; CLI
      `sync backup-job prune <id>` previews retention and requires `--apply`
      before deleting only date-named snapshot trees.
    - [x] Completed/failed runs now update job last-run status, and
      `sync backup-job due` exposes enabled interval/daily jobs ready for a
      future background scheduler; manual jobs are never auto-due.
    - [x] Tauri `backup_job_due` exposes the same read-only due calculation to
      the GUI/coordinator without duplicating schedule logic.
    - [x] Run start is now single-flight per job, and restart recovery updates
      affected jobs to `interrupted`, preventing duplicate scheduler ticks.
      A SQLite partial unique index enforces the one-running-run invariant
      across concurrent processes as well.
    - [x] CLI `sync backup-job run-due [--dry-run]` now provides an explicit
      external-scheduler entry point that evaluates due policy and reuses the
      single-flight execution path.
    - [x] Extracted deterministic UTC `next_due_at` schedule calculation for
      interval/daily/manual policies, giving a future background coordinator a
      precise wake-up basis instead of duplicating timing logic.
    - [x] Added provider-independent `BackupScheduler` snapshots and Tauri
      `backup_job_scheduler_snapshot`, returning due IDs plus the next future
      wake-up timestamp without spawning implicit background work.
    - [x] Added opt-in CLI `sync backup-job watch` with `--once`, `--dry-run`,
      and bounded `--max-cycles`; it sleeps to the scheduler wake-up and runs
      due jobs through the guarded execution path without implicit startup.
  - [x] Add integrity verification and restore-selection UI/CLI with visible
    backup history; do not mark a run successful before verification completes.
    - [x] Added CLI `sync backup-job snapshot-list` and `restore`: users select
      a dated snapshot/file, downloads use the shared queue, remote/local byte
      sizes are verified, and destination writes are atomic via a partial file.
      Relative-path validation blocks traversal; GUI restore selection remains
      for a later frontend slice.
    - [x] Tauri `backup_job_snapshot_list` now exposes capability-checked,
      relative snapshot inventory for GUI restore selection.
    - [x] Settings now shows persisted backup-job configuration and recent
      durable run history with an explicit refresh action; restore picker and
      job editing remain separate frontend work.
    - [x] Added Tauri `backup_job_restore` and a Settings snapshot/file picker
      with atomic verified restore; job editing and richer restore history
      remain deferred.
    - [x] Settings now edits and persists backup-job source, drive, remote
      root, schedule, retention, verification, and enabled state using the
      same validated Tauri contract as the CLI.
    - [x] Settings also removes job configuration explicitly without deleting
      any local or remote snapshot data.
    - [x] Verified backup uploads now compare SHA-256 of the local payload
      with a remote read-back (plus size/stat), rather than treating matching
      byte counts as full integrity verification.

#### P34.4 — Security and provider expansion, P2

- [>] **Provider authentication and credential hygiene.** Authentication is
  part of the product boundary, not an implementation detail: `drives.json`,
  the frontend settings store, logs, URLs, crash reports, and the shipped app
  must never contain passwords, TOTP codes, refresh/access tokens, or OAuth
  client secrets.  OAuth public client IDs are not secrets; desktop/mobile
  flows must use PKCE and a system browser.  Native Filen/Internxt login must
  accept an optional one-time 2FA code and persist only the encrypted/native
  session in the OS keychain. WebDAV must use keychain-backed credentials or
  provider app passwords.
  - [x] Drive metadata serialization now skips every auth field; legacy
    plaintext fields are migrated once into the OS keychain and redacted.
  - [x] Added separate keychain credentials and disconnect/status commands;
    IPC exposes presence booleans only.
  - [>] Add Google and Microsoft authorization-code + PKCE browser flows,
    loopback/deep-link callback handling, token refresh, and revocation. Never
    ship a client secret; support user-supplied public client IDs where needed.
    Desktop loopback PKCE, keychain-only token exchange, explicit refresh,
    Google revocation, and Microsoft local credential clearing are implemented;
    mobile deep links and Microsoft’s provider-side logout remain.
  - [ ] Add the desktop/mobile login UI: browser sign-in for Google/OneDrive,
    native email/password plus conditional TOTP for Filen/Internxt, and
    WebDAV username/password or app-password entry without persistence in UI
    settings. Add explicit disconnect/re-auth states.
    - [x] The desktop drive dialog now displays credential/session presence and
      explicit disconnect/re-auth actions for WebDAV, Filen, Internxt, Google,
      and OneDrive. Only boolean presence data crosses IPC; secrets remain in
      the OS keychain. ✅ 2026-08-01
    - [x] Native login UI now detects structured `enter_2fa`/`wrong_2fa`
      responses and clearly promotes the TOTP field to required state without
      persisting the code.
  - [ ] Add unit and hermetic HTTP coverage for PKCE/state validation, token
    exchange/refresh/revocation, redaction, 2FA challenge/error mapping, and
    keychain behavior; add gated live auth/read/write tests with no automatic
    credential discovery.
    - [x] OAuth callback parsing now rejects duplicate `code`, `state`, and
      `error` parameters; PKCE challenge generation is extracted and covered
      against the RFC 7636 verifier vector.
    - [x] Added hermetic refresh-token preservation/malformed-response tests
      and Google-style revocation success/failure HTTP tests.
    - [x] Internxt native login now preserves structured gateway TFA codes and
      messages in actionable errors, with a secret-free unit test.
    - [x] The same structured error mapping now covers security-detail and
      session-hydration failures across the full Internxt login flow.
    - [x] Keychain-backed credential/session set/get/delete APIs now have
      isolated mock-keyring round-trip coverage; no OS keychain is consulted
      by tests.

- [ ] Wire proxy configuration and certificate pinning through every cloud
  connector; add custom CA and TLS policy only after the common HTTP client
  boundary exists.
  - [x] Cloud-backup and remote-index clients now consume the shared proxy
    builder; cloud-backup has hermetic valid/invalid proxy construction tests.
- [ ] Add client-side encrypted-drive wrapping and Cryptomator interoperability
  as a separate security project; do not mix it with ordinary provider links,
  indexing, or share-link semantics.  Encrypted filenames disable provider
  search/share capabilities explicitly.
- [ ] Add Dropbox, S3, Nextcloud, SFTP, pCloud, Azure Blob, B2, FTP/FTPS, and
  Hetzner adapters in that order only when a real CrispSorter workflow needs
  them.  Prioritize S3/Nextcloud/Dropbox before the less-used long tail.

#### P34.5 — UX/platform expansion, P2/P3

- [ ] Add archive browsing, generic text/code editing, saved searches as
  virtual folders, richer provider full-text search, and file associations.
- [ ] Add FUSE write support only after the mutation API and queue are stable;
  retain read-only FUSE for indexing during the transition.
- [ ] Add local REST API, plugin hooks, cron/webhook automation, and system
  tray/cloud-transfer status after the core queue and sync state are durable.
- [ ] Improve mobile file-provider/SAF flows and investigate PWA support only
  after the desktop contextual dual-panel mode is stable.

#### Explicitly deferred for now

- [ ] Full CrispCloud provider parity (all 14 providers) — defer until the
  core six-provider workflow proves which providers users actually need.
- [ ] Dropbox-style password/expiry/share-recipient management — defer until
  provider capability discovery and basic share/version commands are stable.
- [ ] Full Cryptomator/VeraCrypt mounting, Tor/onion routing, crash analytics,
  auto-update, app-store packaging, and every platform shell integration —
  valuable but not prerequisites for contextual document operations.
- [ ] A complete independent Double Commander clone — defer.  We do want the
  useful subset: two contextual panels, selection, comparison, safe mutation,
  transfer queue, and keyboard shortcuts tied to search/catalog results.
- [ ] Generic remote full-text search across every provider — defer where the
  provider lacks a reliable API; CrispSorter's local/cb-api extracted-text
  index remains the preferred search path.

#### Priority order and definition of success

1. **P0:** capability API, shared queue, streaming/resume, offline replay,
   provider contract tests.
2. **P1:** contextual dual-panel workspace, actionable search/duplicate
   results, safe mutations, transfer drawer, version commands.
3. **P1/P2:** general sync pairs, conflicts, delta protocol, backup UX.
4. **P2:** security wiring and the highest-value missing providers.
5. **P2/P3:** archive/editor/platform/plugin/REST expansion.

The milestone is successful when a user can search a document or duplicate,
see both its local/cloud context, compare it, choose a safe action, and watch
that action complete or resume through one durable queue — without leaving
CrispSorter's indexing and document workflow.
