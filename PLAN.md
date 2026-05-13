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
- P13 Bilder vertical: image-row filtered Übersicht tab, lazy thumbnails, EXIF preview pane, SHA-256 + perceptual-hash dup grouping, **CrispLens Tier 2** connector (Keychain-stored session, 4-state health banner, People + watchfolder + by-hash + semantic-search v4 endpoints live-verified against `https://<crisplens-host>`)
- P13.5 Audio + Translation vertical: symphonia + ffmpeg decode, 24 ASR / 5 TTS / 4 MT / 4 LID backends through the `crispasr` Rust crate, `chat transcribe` + `chat tts` CLI, index-time audio/video extraction (22 file types become searchable), audio-LID routing (`BackendFallback` policy switches backend on language mismatch), text-LID at index time populates `language` LanceDB column, on-demand translation (`translate_text` Tauri command, SQLite-cached), index-time batch translation (`text_translated` + `text_translated_lang` columns added via the migration framework)
- P15 Batch pre-processing: content-dedup (SHA-256), book-chapter grouping (ISBN-13)
- OCR: Tier 1 Tesseract, Tier 2 ocrs, Tier 3 PaddleOCR (`--features paddle-ocr`)
- `.cidx` offline archives: LanceDB + Tantivy FTS export/mount, Archiv tab in Übersicht, background-promote per row
- Schema-migration framework: versioned `Migration` trait with SQLite ledger at `<data_dir>/.crispsorter_migrations.db`, gap/duplicate detection, idempotent reruns; `AddTextTranslatedColumns` (v100) is the first real consumer
- `crisp+cb-archive://` URI scheme for cloud-backup archived files
- `crisp+drive://` URI scheme for any registered CloudDrive (Local / Filen / Internxt / WebDAV)
- macOS arm64 packaging: `scripts/bundle_macos_native_libs.sh` co-bundles `libcrispasr.dylib` + `libcrispembed.dylib` + ggml backends + homebrew transitives into `.app/Contents/Frameworks/` with rewritten LC_RPATH entries

For per-feature deep-dives, see [HISTORY.md → "Phase ship index"](HISTORY.md).

---

## In Progress

**P13.6 Multimodal UX + L1/L2/L3 integration** — surfaces the
P13.5 audio capability + the P13 image capability through the GUI
end-to-end, fills the L1/L2/L3 gaps for media files, and adds the
missing Settings panel.  See "P13.6 plan" below for the
step-by-step execution order; we are working through this batch
before the next release tag.

Just shipped this session (`8206afb`): audio/video drag-drop +
file-picker accepts the 22 audio/video extensions in both Stapel
(BatchReview) and Kataloge (IndexIngest); JS-side `extractText`
dispatches the audio/video extensions through the new
`audio_extract_text` Tauri command, keeping PCM in Rust.

**Test coverage:** ~415 unit tests in `tauri-app` (+2 `#[ignore]`'d
WebDAV-live integration tests gated by
`WEBDAV_TEST_URL`/`USER`/`PASS`), 20 in `crispcat`, 29 in
`crisplens-protocol`, 5 in `crisp-index-protocol`.  Run
`cargo test --workspace --lib` for the exact current count.

---

## Open TODOs

Only `[ ]` items live here.  Shipped items are in HISTORY.md.

### P13.6 — Multimodal UX + L1/L2/L3 integration

The 2026-05-12 P13.5 vertical landed full audio+video extraction
end-to-end on the Rust side (symphonia tier-1 + ffmpeg tier-2 +
CrispASR, 22 file extensions) and `8206afb` opened the drop-zone
gates in both ingest panels.  But the UI still treats audio
files as second-class citizens: the status badge says "Extracting"
instead of "Transcribing", the detected source language doesn't
surface in any column, and audio-specific L2 metadata
(duration / codec / bitrate) is invisible because no extractor
populates it.  Meanwhile the P13 Bilder vertical lives in its
own tab and doesn't feed images into the search index at all,
even though CrispEmbed has the encoder we'd need.

There's also no Settings panel for any of this — no
activate/deactivate toggle for audio extraction, no ASR backend
selector, no LID method choice, no image-indexing toggle, no
CrispLens-for-search bridge.

Eleven steps planned, ordered so that the small UX wins ship
first and the big architectural pieces (L2 schema + image
indexing pipeline) only land after the foundations are in.
Audio+video and images share Steps 1, 6, 10, 11 explicitly —
parallel work is more efficient there than splitting into
duplicate slices.

#### Step 1 — Status label "Transcribing" for media files (audio + video)
- [ ] **~30 min** — BatchReview + IndexIngest status badges.
  When the entry's extension is in `AUDIO_EXTENSIONS`, render
  `i18n.t.batch.status_transcribing` / `…_extracting` instead
  of the generic `status_extracting`.  Per-row state machine
  doesn't need to change; only the label switch.
  EN: "Transcribing", DE: "Transkribiere".

#### Step 2 — Detected-language column in Stapel
- [ ] **~45-60 min** — `extractors::audio::extract` already
  returns `language: Option<String>`; today it's discarded in
  the JS-side return.  Pass it through `audio_extract_text` →
  `extractText` → `ExtractionResult.metadata.language` →
  BatchReview's per-item record.  Add a `language` column to
  the BatchReview table (default-hidden via column visibility
  toggle, defaults visible only when at least one audio item is
  in the batch).  Shows as "EN" / "DE" / "BS" badge.

#### Step 3 — Audio L2 metadata (duration / codec / bitrate)
- [ ] **~1-1.5 h** — new `audio_metadata(path)` Tauri command
  using `symphonia`'s codec params (no decode pass; the format
  reader's `tracks()` exposes sample rate / channels / bitrate
  in O(1)).  Pre-fill in BatchReview entry rows the same way
  `extract_pdf_metadata` pre-fills title/author/year for PDFs.
  Surfaces in two places: (a) hover-tooltip on the row, (b) a
  derived `duration_seconds` column (default-hidden).  New
  LanceDB columns `audio_duration_seconds`, `audio_codec`,
  `audio_bitrate_kbps` via a schema migration (v101), populated
  by the bg_ingest audio path so search results show them too.

#### Step 4 — "Drop area" empty-state strings mention audio/video
- [ ] **~20 min** — IndexIngest's `empty:` i18n string says
  "Keine Dateien. Dateien hierher ziehen oder Hinzufügen." —
  doesn't communicate that audio/video are now accepted.
  Update strings in EN+DE.  Settings → "Supported formats"
  panel (which lists the extension list to the user) is the
  other place to mention the multimodal set.  Add an
  "Audio & Video" sub-list there with the 22 extensions
  grouped (audio / video-containers / ffmpeg-tier-2).

#### Step 5 — Multimodal Processing Settings panel
- [ ] **~3-4 h** — new Settings → "Multimodal" sub-panel
  (sits beneath the existing "Index" sub-panel).  Persisted as
  three new `IndexConfig` fields + the matching
  `index_config.json` ledger entries:
  - `audio_extraction_enabled: bool` (default `true` iff
    `crispasr` feature is compiled in).
  - `audio_asr_backend: AsrBackend` enum (`Whisper` /
    `WhisperLargeV3` / `Parakeet` / `Qwen3Omni` etc.) — wired
    into `extractors::audio::shared_asr_handle()` which today
    hard-wires `AsrConfig::default()`.
  - `audio_lid_method: AudioLidMethod` enum (`Whisper` /
    `Silero` / `Ecapa` / `Firered`) + auto-resolution
    (whisper-method already done in `2b80345`).
  - Reuses the existing `translate_to` field for "translate
    transcripts to target language at index time".
  Settings UI: 4 selects (enable / backend / lid / translate)
  + a description tooltip per dropdown.  i18n keys
  `settings.multimodal.*` (EN+DE).

#### Step 6 — Multimodal Processing Settings panel — image side
- [ ] **~2-3 h, batched with Step 5** — same panel adds image
  controls.  Persisted IndexConfig additions:
  - `image_extraction_enabled: bool` (default `true` iff
    `paddle-ocr` OR Tesseract is available).
  - `image_ocr_tier: OcrTier` enum (`Tesseract` / `Ocrs` /
    `PaddleOcr`) — already exists in the bg_ingest settings
    but isn't surfaced in the IndexConfig Settings panel
    (lives in a separate "OCR" subsection today).  Move it
    here for consistency under the multimodal umbrella, or
    cross-link.
  - `image_indexing_enabled: bool` — separate from
    `image_extraction_enabled`: extraction = OCR text;
    indexing = also embed for semantic search.  Today
    images aren't ingested into LanceDB; this flag gates
    the new Step 9 pipeline.
  Doing this in the same Settings panel as Step 5 means one
  Svelte file edit + one i18n batch + one IndexConfig
  migration.

#### Step 7 — Audio L1: file-system-only "lightweight" ingest
- [ ] **~1 h** — bg_ingest's audio path currently always runs
  the full transcription (L3).  Add an L1-only mode that
  records the file in LanceDB with just path/size/mtime/ext
  and no transcript, so users can index a huge media folder
  fast and have the option to L3-promote individual rows.
  Mirror the P11 cloud-drive `manifest-only` flow:
  `ingest_audio_level: L1 | L2 | L3` enum in IndexConfig,
  default L3.  L2 = also runs Step 3's audio_metadata; L3 =
  full transcription.

#### Step 8 — Audio L3 promote command (search-side action)
- [ ] **~1 h** — when a row is at L1/L2 and the user clicks
  "Transcribe" in the search-results context menu (already
  exists for cloud-drive L3 promote), run
  `audio_extract_text` and patch the row's `full_text` +
  `text_translated` (if `translate_to` is configured).
  Re-embed via the existing on-demand promote pipeline.

#### Step 9 — Image indexing pipeline
- [ ] **~3-4 h** — close the gap where P13 Bilder lives in
  its own tab but images don't feed the search index.
  Steps:
  - bg_ingest classifier: route image extensions to the
    existing `extractors::ocr_*::extract` chain (Tesseract /
    Ocrs / PaddleOcr by `image_ocr_tier`).
  - The extracted OCR text becomes L3 `full_text` (same
    pipeline as PDFs); EXIF goes into L2 (new schema columns
    via a migration v102:
    `image_camera_make` / `image_camera_model` /
    `image_iso` / `image_lens` / `image_taken_at_seconds`).
  - SHA-256 + perceptual-hash already computed by the P13
    Bilder vertical — reuse those if the row is already in
    that table; otherwise compute fresh in bg_ingest.
  - Surface a "Bilder im Index" badge in the IndexIngest
    panel.

#### Step 10 — CrispLens-for-search bridge
- [ ] **~2-3 h** — when CrispLens is configured (P13 already
  wired the connector + UI), use it as a Tier-2 enrichment
  during image indexing:
  - For each image, query CrispLens `images/by-hash` first;
    if the server already has it indexed, lift its
    `people`, `tags`, `caption` fields into our L2.
  - Otherwise upload the image + fetch the enrichment on
    completion.  Gated by a new
    `IndexConfig.crisplens_image_enrichment_enabled: bool`.
  Sensible to batch with Step 9 since both pieces touch the
  image-ingest path.

#### Step 11 — Documentation pass + tests
- [ ] **~1-2 h** — after Steps 1–10 land:
  - README: capabilities table update (audio/video drop,
    multimodal Settings, image search).
  - HISTORY.md: session log entry per the usual format.
  - At least 4 new unit tests:
    1. `extractText` audio dispatch path
    2. `audio_metadata` Tauri command roundtrip
    3. IndexConfig multimodal-fields persistence
    4. bg_ingest classifier L1/L2/L3 routing for audio.

#### Suggested execution order

1. **Steps 1, 2, 4** — small UX polish, can ship in one commit
   per step (or one batch commit).
2. **Step 3** — audio L2 metadata (new schema migration, new
   command).  Lays groundwork that Steps 7 + 8 build on.
3. **Steps 5, 6 (paired)** — Settings panel for both audio
   and image processing.
4. **Steps 7 + 8 (paired)** — audio L1/L2/L3 ingest level +
   promote.
5. **Steps 9 + 10 (paired)** — image indexing + CrispLens
   enrichment.
6. **Step 11** — docs + tests.

After all 11 steps land, cut **v0.1.41** with HISTORY entry.

### P3.5 — CrispEmbed / CrispASR bundling

- [x] Phase 1 — macOS arm64 (see HISTORY.md)
- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session)
      RPATH / DLL colocation; each platform needs 1-2 release iterations.
      Opening prompt: `handover-prompts/session-prompt-crispembed-ci-matrix.md`
      (local-only — see .gitignore).
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

### CrispEmbed — leverage unused capabilities (survey 2026-05-13)

CrispEmbed (sibling repo, v0.3.2 as of 2026-05-13) exposes several
features CrispSorter doesn't yet consume.  The on-disk model
collection has gained reranker entries (bge-reranker-v2-m3,
jina-reranker, gte-base/large-en-v1.5).

**Already wired (this session)**:
- CrispEmbed sparse encoding for GGUF backend (5e0eab1) — closes
  the gap where GGUF users lost the RRF sparse channel.
- Embedder-as-bi-encoder reranker (6bfedbe) — re-scores top-N
  hybrid candidates by cosine similarity against the query, using
  the already-loaded dense embedder.  Activates when
  `IndexConfig.use_embedder_as_reranker = true` and no dedicated
  cross-encoder is configured.  Settings UI checkbox lands in
  the same commit.
- `index/reranker.rs` routed through `CrispEmbedBackend`
  (ebd511f) — no more direct `crispembed::CrispEmbed::new`
  import outside `index::embedder`; opens the door to future
  shared knobs (Matryoshka / prefix / cache_dir).
- `crispembed::list_models()` registry helper surfaced via the
  `embedder_registry_list` Tauri command + a disclosure panel in
  Settings (b0ebc23).  Informational only for now: selecting a
  non-`EmbedderModel`-enum entry still needs the String-keyed
  selection refactor below.

**Still unused**:

- [ ] **ColBERT multi-vector retrieval** (`encode_multivec`)
      (~1 session) — per-token L2-normalised embeddings (BGE-M3
      ColBERT head).  Needs a new LanceDB column for the
      per-token vectors (FixedSizeList of variable length is
      awkward; might need a separate `chunk_multivec` table joined
      by `id`) + a late-interaction MaxSim scorer in the search
      pipeline.
- [ ] **Omnimodal cross-modal search** (`encode_audio` /
      `encode_image`) (~2 sessions) — BidirLM-Omni encodes text,
      audio, and images into a shared 2048-d space.  Unlocks:
      type "photo of a sunset" → image hits without OCR; type
      "podcast about Bosnia" → audio file hits without
      transcription required.  Needs a new model class
      (BidirLM-Omni isn't in the existing `EmbedderModel` enum), an
      image-patch preprocessing pipeline (pixel patches +
      grid_thw), and a decision about how the 2048-d cross-modal
      vector coexists with the existing per-backend dense column
      (separate column? per-index dim selection at init?).
- [ ] **Registry-driven embedder selection** — the
      `embedder_registry_list` Tauri command surfaces the full
      CrispEmbed registry, but the dropdown still keys off the
      `EmbedderModel` enum.  Wiring a parallel String-keyed
      selection path (or refactoring `EmbedderModel` to String)
      would let new upstream registry models be picked without a
      CrispSorter release.  ~1 session.

### P13.5 follow-ups (remaining after the 2026-05-13 batch)

Ten P13.5 follow-ups shipped on 2026-05-13 (see HISTORY.md):
`--stream` flag, LID/MT model auto-resolution,
`SearchFilters::prefer_translated_lang` + snippet swap,
`IndexConfig.translate_to` persistence + Settings UI, frontend
`translate_text` integration in the search-results panel, SRT /
VTT output formats for `chat transcribe` (`63ec866`),
Audio-LID auto-resolution for whisper-family backends
(`2b80345`), `index/reranker.rs` routed through
CrispEmbedBackend (`ebd511f`), `crispembed::list_models()`
registry helper + Settings disclosure (`b0ebc23`), and the
FTS-over-translated-body Tantivy schema slice (`be73321`).

Still open:

- [ ] **Per-language reranker selection** — `language` LanceDB
      column is populated (Phase 7); routing the reranker model
      by it is the next slice.  Likely shape: `IndexConfig` gets a
      `Map<Language, RerankerModel>` (per-language pick) or a
      simpler "use multilingual reranker when language differs
      from the embedder's primary" toggle.
- [ ] **Per-chunk vs per-doc translation storage** — today we
      replicate the full translation across every chunk row of a
      doc, matching the existing `full_text_md` convention.  For
      very long docs (100 KB translation × 100 chunks = 10 MB
      replicated) this is wasteful.  Alternative: store only on
      `chunk_index = 0` and JOIN at search time — needs a careful
      migration on shipped data + decisions around the
      `record_batches_to_search_results` snippet path.
- [ ] **FTS body_translated migration on legacy indexes** —
      `be73321` adds the field for fresh indexes and gracefully
      degrades for legacy ones (`IndexFields.body_translated =
      None`).  A proper "rebuild Tantivy from LanceDB to upgrade
      the schema" migration is needed for users with shipped
      indexes to get the FTS-over-translated-body benefit
      without re-ingesting from disk.  Should go through the
      migration framework with a fresh version > v100.
- [ ] **Non-whisper audio-LID auto-resolution** — `2b80345`
      handles the whisper-method case by registry-resolving
      `whisper`.  Silero / Ecapa / Firered still require explicit
      `--lid-model` paths because they aren't in CrispASR's
      registry.  Add upstream registry entries
      (`lid-silero`, `lid-ecapa`, `lid-firered`) to close this.

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
