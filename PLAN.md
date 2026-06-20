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

Run `cargo test --workspace --lib` for the exact Rust unit-test count.
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
- [ ] **Auto-process toggle on watch detection** — UX design pass complete (2026-05-16): per-folder three-mode dropdown (off / analyse / sort), opt-in initial scan, debounced queue, hourly file cap + daily cost cap, tray status surface, fail-soft error path.  6-slice implementation arc spec'd in `handover-prompts/session-prompt-auto-process-toggle.md` (~16 h total).
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
- [ ] **Finish audio cross-modal search.** `encode_audio` is wired but barely
  used (1 call site). The omni goal — "podcast about Bosnia" → audio hits
  without transcription — needs audio embeddings actually indexed into the omni
  vector space + surfaced in search. Likely completing a partial wiring, not a
  new build.
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
- [ ] **(Minor)** `encode_tokens` for token-level match highlighting.

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
- [ ] **LLM-suggested topical clustering** for read-later corpora
  with no real author metadata — auto-build a folder hierarchy by
  topic so the "sort into Author/Year/Title" workflow has
  something to render.
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
  Both via `spawn_blocking`; soft-fail (log + continue with None).
  Audio omni deferred (needs decoded PCM not currently available
  in the omni path).

- [x] **`images/vit_embed.rs` no longer dead.**  Called from bg_ingest
  for image files; embeddings stored in the `embedding_vit` column.

- [x] **`index/omni_embed.rs` no longer dead.**  Called from bg_ingest
  for image files; embeddings stored in the `embedding_omni` column.

### Still pending (follow-up work)

| Item | Effort | Notes |
|------|--------|-------|
| Audio omni embedding | ~4 h | Needs decoded PCM from the audio extractor piped into `encode_audio_omni` |
| Omni/ViT RRF search channel | ~6 h | Add ANN query over `embedding_omni`/`embedding_vit` columns in `search.rs`, mix into existing RRF fusion |
| Cross-modal search UI | ~2 h | "Search by image" button in IndexSearch that embeds an uploaded image and queries the ViT/omni columns |

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
Two modules (`vit_embed.rs`, `omni_embed.rs`) remain dead pending
schema migrations + ingest pipeline work (~20 h combined).  These
are tracked in the "Still dead" table above.
