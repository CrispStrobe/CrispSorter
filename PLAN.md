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

### P13.5 follow-ups (remaining after the 2026-05-13 batch)

Five P13.5 follow-ups shipped on 2026-05-13 (see HISTORY.md):
`--stream` flag, LID/MT model auto-resolution,
`SearchFilters::prefer_translated_lang` + snippet swap,
`IndexConfig.translate_to` persistence + Settings UI, frontend
`translate_text` integration in the search-results panel.

Still open:

- [ ] **SRT / VTT output formats for `chat transcribe`** — current
      `AsrHandle::transcribe_with_language` concatenates segments
      into a `String`; SRT/VTT need timestamps.  Add
      `AsrHandle::transcribe_segments` returning
      `Vec<crispasr::Segment>` and format from there.
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
- [ ] **Audio-LID auto-resolution** — the text-LID side is
      resolved; audio LID still requires explicit `--lid-model`
      paths because Silero / Ecapa / Firered models aren't in
      CrispASR's registry.  Either add registry entries upstream
      (`lid-silero`, `lid-ecapa`, `lid-firered`) or wire the
      Whisper-method LID path to reuse the loaded ASR ggml file.
- [ ] **FTS-over-translated body** — Tantivy schema currently
      only indexes the original `full_text`.  An English query
      `"hello"` against a Bosnian doc with English translation
      `"hello, how are you?"` doesn't hit BM25.  Fix: add a
      `body_translated` Tantivy field + a per-index schema
      migration on the Tantivy side, then wire
      `SearchFilters::prefer_translated_lang` into the FTS query.
      Multilingual embeddings already handle the vector channel
      reasonably; this would close the FTS gap.

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
