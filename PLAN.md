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

No major vertical currently in flight.  The most recent ship (2026-05-12)
closed out **P13.5 Audio + Translation** end-to-end — all 9 phases plus
the schema-migration framework that unblocks future column-adds.  See
[HISTORY.md → "Session log — 2026-05-12"](HISTORY.md) for the commit
trail.

**Test coverage:** ~415 unit tests in `tauri-app` (+2 `#[ignore]`'d
WebDAV-live integration tests gated by
`WEBDAV_TEST_URL`/`USER`/`PASS`), 20 in `crispcat`, 29 in
`crisplens-protocol`, 5 in `crisp-index-protocol`.  Run
`cargo test --workspace --lib` for the exact current count.

---

## Open TODOs

Only `[ ]` items live here.  Shipped items are in HISTORY.md.

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
