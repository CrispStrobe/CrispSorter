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
- [x] **Phase 3 — mobile** ✅ SHIPPED v0.4.0 (2026-06-04).  Full feature parity on Android aarch64 + iOS.  Desktop-only code behind `--features desktop`; vendored OpenSSL; lance-linalg Android patch; responsive UI; CI jobs for APK + IPA.  See [HISTORY.md](HISTORY.md) 2026-06-04 session log.
  - [ ] **Android SAF handler** — port CrispCloud's `SAFHandler.kt` for Downloads folder access via `DocumentsContract`.
  - [ ] **iOS security-scoped bookmarks** — Swift bridge for persistent folder access via `startAccessingSecurityScopedResource()`.
  - [ ] **Native lib bundling** — CrispEmbed/CrispASR `.so` into APK `jniLibs/arm64-v8a/`; xcframework into iOS Xcode project.  Build scripts exist in both sibling repos (`build-android.sh`, `build-ios.sh`).

### P5 — Future / planned

- [x] **Batch session persistence → SQLite** — ✅ SHIPPED (commits `06e0282` → `00e9962`).  Fixed the "we LOST all the files?!" data-loss + UI-hang-at-53/196 bugs by replacing the single JSON-blob-in-`settings.json` persistence with a transactional SQLite store (`src-tauri/src/batch_session/`, one row per item, WAL, bulk upserts).  All 5 slices landed plus extras the handover prompt didn't spec (processed-history dedup → skip re-extraction of previously-sorted files: `record_processed`/`lookup_history`/`history_count`; full `extractedText` stripped from the IPC payload + lazy-loaded from SQLite on resume).  15 `batch_session` unit tests green (roundtrip, bulk 100+, interleaved upsert/clear, migration sentinel, processed-history).  See [HISTORY.md](HISTORY.md) + `handover-prompts/session-prompt-batch-sqlite-persistence.md` for the original spec.
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

- [ ] **Cross-corpus deduplication by canonical URL** — same
  article saved twice (wallabag import + manual "papers" folder)
  produces two rows with different sha256s but the same `url`.
  Detect, offer to fold.
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
