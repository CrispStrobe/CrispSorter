# CrispSorter — History & Archived Plans

This file collects historical planning documents that are no longer
"living" but are still useful as context — explanations of *why* parts
of the codebase look the way they do.

For active development plans, see [PLAN.md](PLAN.md).
For technical pitfalls / non-obvious patterns, see [LEARNINGS.md](LEARNINGS.md).

---

## P28 — Performance optimization pass (2026-07-04)

Systematic audit across search, ingest, LanceDB I/O, dependencies,
and frontend bundle — ~25 distinct optimizations in 14 commits.
976 unit tests (up from 961; 15 new covering the changes).

**Search pipeline:**
- `result_cache.rs`: VecDeque LRU with O(1) eviction; direct field
  hashing (eliminates `serde_json::to_string` allocation per query).
  3 new tests (LRU promotion, hash determinism, f64 bit-pattern).
- `search.rs`: zero-copy `rrf_merge_n` (`&[&[&str]]` → no String
  clones across 4 RRF channels).  2 new tests.
- `fts_query.rs` + `synonyms.rs`: `eq_ignore_ascii_case()` replaces
  `to_uppercase()` (allocation-free); `is_ascii()` guard for safe
  W/PRE byte-slice matching on multibyte input.  4 new tests.
- `snippet.rs`: in-place `retain()` replaces cloned filter;
  `truncate_str()` helper replaces `chars().take(N).collect::<String>()`
  at 5 hot-path sites (browse, search, translation, federated
  snippets).  4 new tests.

**LanceDB I/O:**
- Column projection on all 6 major query paths: browse scanner
  (`query_documents`), dense ANN (`search_vector`), omni/vit ANN
  (`search_vector_column`), similarity (`find_similar`), chunk
  hydration (`fetch_best_chunk_per_doc`), sparse pool
  (`search_sparse_in_pool`).  Excludes 3 embedding vectors
  (1024+2048+768 f32), `multivec_packed`, `full_text_md`,
  `embedding_sparse`, `embedding_model` — potentially 5–20× fewer
  bytes read per page/search.  `search_result_columns()` helper
  centralises the column list.
- Cached `Arc<Schema>` in `LocalIndex` — built once at construction,
  reused by every `ingest_batch` (was rebuilding ~25 Field objects per
  document).
- `Vec::with_capacity(total_rows)` in `cluster_documents`,
  `list_failed_extractions`, and both search-result builders.

**Ingest pipeline:**
- `bg_ingest`: ViT + Omni `spawn_blocking` fired concurrently (~2×
  wall-time for dual-model image ingest); single `fs::metadata` call
  (was duplicated).
- `ingest.rs`: conditional `texts.clone()` (skip when ColBERT is off);
  merged embedder lock (model_id read inside existing guard);
  LanceDB write batch size raised from 128 to 512 rows.  1 new test.
- ColBERT IN-list: collapsed double Vec allocation into single pass.
- `chunk_text`: O(N) byte-level word boundary scanner replaces O(N²)
  `text[pos..].find(word)` loop.
- `doctype.rs`: `text.to_lowercase()` deferred past extension-based
  early returns — avoids full-text heap copy for extension-classified
  types (email, epub, image, audio, video, code).
- Zero-alloc LID text sampling via `char_indices` byte-boundary slice.

**Dependencies + build:**
- tokio `"full"` → 7 specific features (drops `net`, `signal`).
- symphonia `"all"` → used codecs only (drops `adpcm`, `mp1`, `mp2`).
- Removed `similar "unicode"` feature + duplicate `futures-util` dep.
- Cargo profiles: `opt-level = 1` for deps in dev builds;
  `lto = "thin"` in release.

**Frontend:**
- Vite `manualChunks` vendor splitting (7 heavy deps: pdfjs, mammoth,
  tesseract, katex, deep-chat, web-llm, HF transformers).
- `@mlc-ai/web-llm` dynamically imported on first use.
- All 5 extractors (pdf, docx, epub, html, image) converted to
  dynamic `import()` inside switch cases.

**Other:**
- `DiffSegment.tag`: `String` → `&'static str` (eliminates heap
  allocation per diff segment).
- Performance patterns documented in `LEARNINGS.md`.

---

## v0.9.1 — Wiring, Tests, Document Classification, Scan Cleanup (2026-07-03)

Released with all 5 platforms (macOS, Linux, Windows, Android, iOS).
See `RELEASE_NOTES_v0.9.1.md` for full details.

## Post-v0.9.0 — Wiring, Tests, Document Classification (2026-07-03)

### Feature wiring (CLI + GUI)

Closed all wiring gaps from the v0.9.0 audit:

**CLI** — 10 new `crispsorter index` subcommands: `versions`,
`audit-log`, `retention-rules`, `retention-add`, `compare`,
`entity-graph`, `feed`, `export`.

**Frontend** — Settings panels for Audit Log (query + table),
Retention Policies (CRUD), RSS Feeds (fetch + preview).  Search
result row buttons: Export (HTML), Highlight (reading list).
CorpusDashboard: Entity Graph panel.  PdfTools: Detect Signatures
+ PDF/A conversion buttons.

**Backend** — Audit logging auto-fires on search, delete, and
ingest operations.

### P26.1 — Document-type classification

Heuristic classifier (`index/doctype.rs`) with 18 document types
(letter, invoice, receipt, form, email, report, specification,
presentation, spreadsheet, image, audio, video, ebook, code,
article, contract, memo, unknown).  Based on file extension + text
content pattern matching.  Wired into bg_ingest — every ingested
document gets a `doctype:<class>` tag automatically.

### Android build fix

Root cause: TOML `[target.'cfg(not(android/ios))'.dependencies]`
section for arboard was placed mid-file, causing ALL subsequent
deps (rusqlite, sha2, image, etc.) to be excluded on Android.
Fixed by moving arboard to its own target section.  Also gated
`feed-rs`, `docx-rs`, and clipboard behind `desktop` feature.

### Test suite

929 tests (was 790 at start of session).  +139 new tests:
- pdf_ops: 49 total (redaction, PDF/A, sanitise options, remap_refs)
- doctype: 11 (extension, invoice, receipt, contract, letter, memo,
  article, report, form, short text, tag format)
- CLI parse: 14 (parse_page_spec, parse_split_ranges edge cases)
- clustering: 12 (kmeans edge cases, TF-IDF)
- synonyms: 15 (operators, wildcards, bidirectional, mixed langs)
- comparison: 11 (ratios, long text, whitespace)
- annotations: 8, versioning: 7, retention: 7, audit: 7, feed: 11,
  export: 5

---

## v0.9.0 — Universal Document Viewer, PDF Toolkit, Discovery & DMS Features (2026-07-02)

Major feature release spanning P24–P27.  25 new features, 101 new unit
tests (total 891), 3 new Rust modules, 10 new Svelte components.

### Universal Document Viewer (P27.1–P27.2)

Cross-platform document viewer replacing the old `<object>` PDF embed
and bare `<img>` tags.  New `src/lib/components/viewer/` module with
format-specific sub-viewers:

- **PdfViewer** — pdfjs-dist canvas rendering, page nav, zoom, text
  layer for selection, keyboard shortcuts, fit-width/fit-page modes
- **ImageViewer** — zoom/pan (0.1x–6x), Ctrl+wheel, fit toggle
- **DocxViewer** — mammoth → HTML with dark-theme CSS
- **EpubViewer** — chapter navigation sidebar + HTML rendering
- **TextViewer** — monospace with 512KB truncation
- **HtmlViewer** — sanitised HTML with charset detection
- **CsvViewer** — auto-delimiter detection, sticky headers
- **FallbackViewer** — "Open in app" button

`DocumentViewer.svelte` router dispatches by file extension.  Replaced
~150 lines of duplicated preview code in IndexIngest + IndexSearch.
Shared `viewer/types.ts` with `uriToPath()`, `detectKind()`, format
constants.

### PDF Manipulation Toolkit (P27.1, P27.7, P27.13, P27.14, P26.5, P26.6, P26.7)

`pdf_ops.rs` module — 18 operations via lopdf:

| Operation | Function | CLI |
|-----------|----------|-----|
| Get info | `pdf_info` | `pdf info` |
| Extract pages | `extract_pages` | `pdf extract` |
| Remove pages | `remove_pages` | `pdf remove` |
| Reorder pages | `reorder_pages` | `pdf reorder` |
| Rotate pages | `rotate_pages` | `pdf rotate` |
| Crop pages | `crop_pages` | `pdf crop` |
| Merge PDFs | `merge_pdfs` | `pdf merge` |
| Split PDF | `split_pdf` | `pdf split` |
| Add page numbers | `add_page_numbers` | `pdf number` |
| Add watermark | `add_watermark` | `pdf watermark` |
| Insert blank page | `insert_blank_page` | `pdf insert-blank` |
| Edit metadata | `edit_metadata` | `pdf metadata` |
| Decrypt PDF | `decrypt_pdf` | `pdf decrypt` |
| Encrypt PDF | `encrypt_pdf` | `pdf encrypt` |
| Sanitise | `sanitise_pdf_with_options` | `pdf sanitise` |
| Detect signatures | `detect_signatures` | `pdf signatures` |
| PDF/A conversion | `convert_to_pdfa` | `pdf pdfa` |
| Redact regions | `redact_regions` | `pdf redact` |

**PdfTools.svelte** tab: page list sidebar, multi-select, operation
panels for all 18 tools.  Fine-grained `SanitiseOptions` with per-
category toggles (Info, XMP, JS, files, OpenAction, thumbnails,
annotations).

40 unit tests covering all operations.

### Discovery & Clustering (P24.1, P24.3, P24.4, P24.5, P24.6)

- **P24.1 — Topical clustering:** K-means++ on dense embeddings with
  TF-IDF cluster naming.  `LocalIndex::cluster_documents(k)`, Tauri
  command, CLI `crispsorter index cluster --k 5`, CorpusDashboard
  panel.  12 unit tests.

- **P24.3 — Knowledge graph:** Entity co-occurrence from NER tags.
  `index_entity_graph` Tauri command returns nodes + edges.

- **P24.4 — Synonym expansion:** 94 embedded synonym groups (50 EN +
  44 DE).  `synonym_expand_query()` OR-expands before FTS.  Frontend
  checkbox.  15 unit tests.

- **P24.5 — RSS/Atom feed ingestion:** `extractors/feed.rs` via
  `feed-rs`.  RSS 2.0 / Atom / JSON Feed.  11 unit tests.

- **P24.6 — Clipboard/screenshot capture:** `extractors/clipboard.rs`
  via `arboard`.  Text + image (saved to temp PNG).

### DMS & Compliance (P25.1–P25.9)

- **P25.1 — Document versioning:** `index/versioning.rs`, SHA-256
  version groups, monotonic seq.  7 unit tests.

- **P25.2 — Audit trail:** `audit/mod.rs`, append-only SQLite.
  Query with filters.  7 unit tests.

- **P25.3 — Retention policies:** `index/retention.rs`, per-folder/tag
  rules.  7 unit tests.

- **P25.4 — Stamp on export:** Wired into `tool_ocr_export` +
  CLI `ocr --render pdf --stamp`.

- **P25.7 — Document comparison:** `index/comparison.rs` via `similar`.
  Word-level diff.  7 unit tests.

- **P25.8 — Annotation layer:** `index/annotations.rs`, SQLite CRUD +
  text search.  8 unit tests.

- **P25.9 — Reading queue:** Highlights table, reading list by recency.

### Export (P27.10)

- `extractors/export.rs` — DOCX (via `docx-rs`) and standalone HTML
  export.  5 unit tests.

### CrispEmbed v0.13.0 Integration

Pulled 37 upstream commits: GLM-OCR, Qwen2.5-VL, PaddleOCR-VL,
Restormer, InternVL2, Granite-Vision, Qwen3-VL fixes.  No Rust API
changes.  Fixed `crisp-docx` sibling version pins.

### New Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `feed-rs` | 2 | RSS/Atom feed parsing |
| `arboard` | 3 | Clipboard access |
| `similar` | 2 | Text diffing |
| `docx-rs` | 0.4 | DOCX generation |

### Test Summary

891 tests pass (was 790 at session start).  +101 new tests across
pdf_ops (40), clustering (12), synonyms (15), annotations (8),
versioning (7), retention (7), audit (7), feed (11), comparison (7),
export (5).

---

## Session log — 2026-06-20 (evening) — Audio omni embedding + build infra fix

Implemented the audio omni embedding pipeline (P17.7 completion) and fixed
the cargo build infrastructure.

- **Audio omni embedding wired into bg_ingest** (`9eec70e`). Added
  `ExtractedDocument.audio_pcm: Option<Vec<f32>>` field. The audio extractor
  (`extractors/audio.rs`) now clones the decoded 16 kHz mono PCM before
  transcription consumes it. `bg_ingest` feeds the PCM to
  `encode_audio_omni()` via `spawn_blocking`, storing the 2048-D embedding
  in the `embedding_omni` column. All 15 `ExtractedDocument` construction
  sites updated (`audio_pcm: None` for non-audio extractors). Tests:
  `audio_pcm_field_surfaces_on_extracted_document` unit test +
  `omni_audio_embed_from_file_live` async live test (gated behind
  `CS_TEST_AUDIO` env var + `crispembed,crispasr` features). All 11 omni
  unit tests pass.
- **Cargo target dir moved off SMB2 mount.** The `target/` symlink pointed
  to `/mnt/akademie_storage` (SMB2), where build scripts fail with
  "Invalid argument" (os error 22 — SMB2 can't exec ELF binaries). Created
  `/mnt/volume1/cargo-targets/CrispSorter/target` on the ext4 disk and
  symlinked `target → /mnt/volume1/cargo-targets/CrispSorter/target`.
  Deleted the stale 3 GB SMB copy. `cargo check --features desktop` passes
  clean (6m30s full rebuild → 5m08s incremental after move).

### What's next (priority order)

1. **⭐ Omni/ViT RRF search channel** (~6 h) — the ingest side is complete
   (images + audio get `embedding_omni`/`embedding_vit` at index time). The
   search side needs: `search_vector_column()` method on `LocalIndex`, omni
   text-to-multimodal ANN channel in `search_hybrid()`, `search_by_image()`
   method, Tauri command, CLI `--image` flag. Handover prompt:
   `handover-prompts/session-prompt-omni-vit-search-channel.md`.
2. **Cross-modal search UI** (~2 h) — "Search by Image" button in
   `IndexSearch.svelte`. Depends on #1. Handover prompt:
   `handover-prompts/session-prompt-cross-modal-search-ui.md`.
3. **cb-api PIXIE-Rune backfill** (deferred) — VPS needs RAM headroom.

---

## Session log — 2026-06-20 — CrispEmbed sync: Qwen3-VL, LoRA, LID, confidence, CI consolidation

Audited CrispEmbed HEAD (v0.11.8+114 commits) against CrispSorter's integration
surface and wired in every actionable improvement.

- **13 OCR engines** — added Qwen3-VL (engine 12) to `engine_id()`, CLI
  `--engine` (expanded 7→13 values: was missing parseq, deepseek_ocr2,
  pix2struct, granite_vision, lightonocr, qwen3vl), `--vlm-ocr-engine`,
  and both Settings UI dropdowns (advanced stage builder + simple-mode VLM
  escalation). Qwen3-VL activates once CRISPEMBED_REF is bumped past
  v0.11.8. Unit test `engine_id_maps_all_engines` now covers all 13 IDs.
- **`isVlmEngine` fix** — PARSeq was incorrectly treated as a VLM engine
  in Settings.svelte, showing a single-model field instead of det+rec.
  Fixed to exclude `parseq` (it's engine 7 = DBNet detect + PARSeq
  recognize, same shape as dbnet_trocr/surya/tesseract).
- **Recognition confidence in structured render** — `ocr_regions_via_pipeline`
  now uses `effective_confidence(detection, rec, char_conf_len)` instead of
  the raw detection score (~1.0, useless). hOCR/ALTO/PDF region confidence
  values now reflect actual OCR quality (mean per-char softmax).
- **Pipeline LID → `language` field** — added `CrispOcrPipeline::detected_lang()`
  to CrispEmbed Rust wrapper (fixed FFI signature in crispembed-sys to match
  C header's `out_confidence` param). CrispSorter's `ocr_via_pipeline` now
  populates `ExtractedDocument.language` from the pipeline's LID result,
  eliminating a redundant LID pass during indexing.
- **Mean confidence logging** — `[ocr] pipeline: N regions, mean confidence
  X.XX` diagnostic line during indexing for quality monitoring.
- **LoRA adapter hot-swap** — added `crispembed_set_lora`/`get_lora`/
  `list_lora` FFI decls to crispembed-sys, safe Rust wrappers in
  crispembed, and `CrispEmbedBackend` methods in CrispSorter. Ready for
  Jina v5 task-specific adapter switching without model reload.
- **LFM2.5 license gate** — `lfm2-embed`, `lfm2-colbert`, `gliner-lfm`,
  `gliner-lfm-q4k` registry names now gated as `Restricted("LFM Open
  License v1.0")` in `license_consent.rs`. Prevents download without
  consent when used as model-name overrides.
- **CI consolidation** — `CRISPEMBED_REF`/`CRISPASR_REF`/`CRISPDOCX_REF`
  moved from 3 job-level `env:` blocks to a single workflow-level block.
  Next version bump is a 1-line edit instead of 3.
- **31 → 0 compiler warnings** — cfg-gated all feature-only imports
  (`Context`, `Mutex`, `OnceLock`), constants (`DEFAULT_DET_MODEL`,
  `DEFAULT_REC_MODEL`, `DEFAULT_MATH_MODEL`, `DEFAULT_VIT_MODEL`,
  `DEFAULT_OMNI_MODEL`), and functions (`engine_id`, `source_type_id`,
  `policy_kind`, `is_cjk`, `path_looks_cjk`) across 16 files. Removed
  unused sha2 import, prefixed unused variables with `_`, removed
  unnecessary `mut`, added `#[allow(dead_code)]` on deserialization-only
  fields in drive connectors.
- **Dead code removal** — removed `CrispEmbedBackend::rerank_biencoder`
  (private method shadowed by the public `Embedder`-level implementation)
  and `Reranker.spec` field (stored but never read).
- **v0.5.0 published** — the draft release already had full assets after
  CI re-ran with the `desktop` fix. Published as a non-latest release.
- **PLAN.md audit** — verified and marked 11 items as shipped: P17.1–P17.7
  (layout, OCR engines, math OCR, face detection, omni embeddings, decoder
  embeddings, ViT image embeddings), P18 license-consent gate, P7.8 Tier 4
  VLM OCR (superseded by CrispEmbed), P19 OCR Tier-4 variants, P19
  rerank_biencoder. Open items reduced from 22 → 11; remaining items all
  require multi-session work or external infrastructure.
- **Dependency hygiene** — `npm audit fix` patched devalue (DoS), protobufjs
  (DoS + property shadowing), svelte (SSR XSS, DOM clobbering, ReDoS).
  `cargo update` applied 139 semver-compatible dependency updates. Full
  workspace test suite: **699 passed, 0 failed** (501 s). CrispEmbed: 34
  stale merged branches deleted, 1 stale remote tracking branch pruned.

---

## Session log — 2026-06-15 — P20 ⭐ Smart OCR pipeline → multi-page + structured/searchable output

Built the configurable OCR pipeline end-to-end (CrispSorter Rust caller +
CrispEmbed C++ engines/renderers) and shipped it. The workflow is documented in
[docs/ocr-workflow.md](docs/ocr-workflow.md) + the README OCR features.

- **Smart pipeline** — source-type router (screenshot/scanned-doc/photo) →
  per-stage cleanup (deskew/crop/whiten/binarize + NAFNet denoise) → engine →
  text-yield+confidence accept-gate → chain escalation, with an optional
  post-OCR punctuation restore (FireRedPunc/PCS). 7 engines (DBNet+TrOCR, Surya,
  Tesseract-LSTM, GOT-OCR2, GLM-OCR, Qwen2.5-VL, InternVL2). Master toggle +
  full per-stage builder in Settings; `OcrPipelineConfig` threaded via
  `bg_ingest_set_ocr_pipeline`.
- **Multi-page** — `page_source` splits multi-frame TIFF (pure-Rust `tiff`) and
  rasterizes scanned PDF (PDFium, `--features pdf-render`, libpdfium bundled in
  releases) to one image per page; OCR'd in order, joined by form-feed.
- **Layout-aware reading order** — optional RT-DETRv2 pass: regions → column
  order → per-region OCR (text→engine, formula→math-OCR, figure/table skipped,
  header/footer optionally dropped).
- **Structured / searchable output** — `crispsorter ocr <file> --render
  text|hocr|alto|pdf` via CrispEmbed's `ocr_render_pages` (multi-page +
  binary-safe searchable PDF). Rendering kept in C++.
- **Ad-hoc CLI** — `crispsorter ocr` exposes the whole pipeline (engine +
  pre/post-processors + render format) for one-off use.
- **Cross-repo** — depends on **CrispEmbed v0.11.0** (renderers, classical
  preproc tier, dewarp, Arabic Qari-OCR, `ocr_render_pages` binding); fixed an
  upstream compile blocker (`OcrRegion` alias + `libc` dep) and the
  windows-cuda release CI "No CUDA toolset found". `CRISPEMBED_REF` → v0.11.0.
  Live-validated: `ocr_render::{hocr,pdf}_render_live` green against the real lib.

## Session log — 2026-06-13 — P19 ⭐ GLiNER NER → entity tags + facets

Wired CrispEmbed v0.8.0's zero-shot named-entity recognition
(`crispembed::CrispNER`, GLiNER) through the ingest pipeline so the catalog
auto-extracts people / organizations / places / dates / … from document text
and exposes them as faceted, searchable entity **tags** — with **zero schema
migration** (they land in the existing `tags` column, lighting up the
tag-cloud sidebar, `array_has(tags,…)` filter, `index search --tag`, and the
federated `--tag` path for free).

**New module `src-tauri/src/index/ner.rs`** (mirrors `index::reranker`):

- `NerModel` enum — `SauerkrautGlinerLfm` (German-tuned LFM2.5-350M, default)
  + `GlinerDeberta` (DeBERTa-v3, English/multilingual, Apache-2.0). serde
  kebab-case; `display_name` / `gguf_spec` / `license` / `consent_key` /
  `ensure_license_consent`.
- `Ner` — GGUF load via `crispembed::CrispNER::new` behind the `crispembed`
  feature; a zero-field stub that errors on `load` otherwise.
- `NerHandle { model, labels, threshold, max_entities, max_chars, cache_dir,
  slot: Arc<Mutex<Option<Ner>>> }` — cheap-clone, lazy-loads the GGUF on first
  `extract_tags` (hf-hub download, license gate), soft-fails to empty tags.
  `extract_tags` truncates `full_text` to `max_chars` on a char boundary, runs
  the model, then builds `"<label>:<text>"` tags: drop below `threshold`,
  dedup `(label, text)` case-insensitively, keep top-`max_entities` by score
  (Q6 anti-explosion). `label_to_prefix` maps the curated label set to compact
  prefixes (`organization`→`org`, `location`→`loc`, `phone number`→`phone`, …).
- No-op (`Vec::new`) on builds without `crispembed`, so ingest stays
  byte-identical to today when the feature is off.

**License gate** — `index::license_consent::license_for_registry_name` now maps
`sauerkraut-gliner-lfm` → `Restricted("LFM Open License v1.0")` (consent
required); `gliner-deberta` stays permissive. The model is gated at
`Ner::load` exactly like `Reranker::load`.

**Ingest** — `IngestPipeline` gained an `Option<NerHandle>` (via a `with_ner`
builder, so the existing `IngestPipeline::new` call sites are untouched).
`ingest_documents_batch` runs NER once per document and merges the entity tags
into `raw.tags` (case-insensitive dedup, order-preserving) **before** chunk
rows are built — so every chunk of a doc carries the same entity tags
(chunk_index convention).

**Config** — `IndexConfig.ner_{enabled,model,labels,threshold,max_entities,
max_chars}` with serde defaults (off; `sauerkraut-gliner-lfm`; curated label
set; 0.5; 30; 8000 chars). Threaded into the GUI init path and the CLI
`index ingest` + L3-reingest paths via `ner::handle_from_config`.

**Frontend** — Settings panel (toggle / model dropdown / labels textarea /
threshold slider / entity + char caps), gated on `crispEmbedCompiledIn`,
reusing the P18 consent dialog for the restricted Sauerkraut model; DE/EN
i18n. `TagCloud.svelte` gained an opt-in `groupEntities` view that buckets
namespaced tags under per-label headers (default off → existing behaviour).

**Tests** — 13 `ner` unit tests (serde strings, license split, gguf spec,
label→prefix mapping, threshold/dedup/cap/empty, char-boundary truncation,
empty-input + feature-off no-op) + `merge_tags` dedup test in `ingest` +
license-consent registry test. `npm run check`: 0 errors.

---

## Session log — 2026-06-12 — P17 CrispEmbed deep integration (7 modules)

Wired every major CrispEmbed capability into CrispSorter behind
`--features crispembed` (Metal/Vulkan/CUDA sub-features for GPU).

**New modules** (all cfg-gated, graceful stubs when feature is off):

- **P17.1 `extractors/layout.rs`** — RT-DETRv2 document layout detection
  (17 region types).  Pre-pass for OCR routing: text regions → OCR,
  formula regions → math OCR, figures → skip.  Reading-order sort.
- **P17.2 `extractors/ocr_crispembed.rs`** — New "Tier 4" OCR via
  `crispembed::OcrPipeline`.  Surya-OCR-2 (91 languages) + Qwen2.5-VL
  (German support) + DBNet/TrOCR.  Wired as highest-priority tier in the
  existing dispatch ladder (`OcrTier::Tier4`).
- **P17.3 `extractors/math_ocr.rs`** — Formula → LaTeX via
  PP-FormulaNet-L (printed, 181M) / PosFormer (handwritten).  Standalone
  image recognition + layout-integrated crop-and-recognize pipeline.
- **P17.4 `images/face.rs`** — Face detection only (YuNet 0.2 MB /
  SCRFD): presence + bounding box + confidence.  **No biometric
  recognition** (no embeddings, no person matching) — EU AI Act
  compliant.
- **P17.5 `index/omni_embed.rs`** — BidirLM-Omni shared 2048-D
  embedding space for text + audio + image.  Enables cross-modal search:
  "photo of sunset" → image hits.  Text, batch-text, audio, image, and
  text+image encoding paths.
- **P17.6 `index/embedder.rs`** — 5 new GGUF-only `EmbedderModel`
  variants: `Gemma3Embed2B` (2048d), `ModernBertBase` (768d),
  `ModernBertLarge` (1024d), `DebertaV2Xlarge` (1536d), `NomicBertMoe`
  (768d, 8-expert).  Full registry entries (dims, max_tokens, display
  names, GGUF spec, serde strings, download sizes).
- **P17.7 `images/vit_embed.rs`** — SigLIP/CLIP image embedding for
  visual similarity search.  Works across crops, formats, resolutions.

**Tests**: 35 new unit tests + existing 625 all green (660 total, 0
failures).  Each module also has `#[ignore]`-gated live tests for
real-model validation.

---

## Session log — 2026-06-04 — Phase 3: Android + iOS mobile support (v0.4.0)

Extended CrispSorter to Android (aarch64) and iOS via Tauri 2 mobile targets.
The entire Rust workspace — LanceDB, Tantivy, fastembed, ort, OpenSSL,
pdf-extract, OCR, rusqlite — cross-compiles for `aarch64-linux-android` with
100% feature parity.  Zero features removed; the mobile build carries the
same index, search, batch sort, sync, and translation pipelines as desktop.

### Architecture decisions

- **Tauri mobile, not a separate Flutter app.** The user asked to keep it as
  one project.  The `#[cfg_attr(mobile, tauri::mobile_entry_point)]` was
  already in `lib.rs`; Tauri 2 plugins are all mobile-compatible.
- **`desktop` feature flag** gates code that fundamentally can't run on
  mobile: subprocess spawning (sidecars for Ollama/llama.cpp/MLX, TTS via
  `say`/`espeak`), `notify` folder watcher (no inotify on Android), and
  `mistralrs` (local LLM inference via the sidecar model).
- **Vendored OpenSSL** (`openssl = { features = ["vendored"] }`) — the ML
  stack (fastembed → ort → ureq → native-tls, hf-hub, lancedb) pulls
  `openssl-sys` transitively.  Vendoring compiles OpenSSL from source for
  any cross-compilation target.
- **lance-linalg Android patch** (`patches/lance-linalg/`) — upstream's
  `build.rs` checks `target_os == "linux"` for aarch64 NEON but doesn't
  match `"android"`.  One-line fix: `target_os == "linux" || target_os ==
  "android"`.
- **Platform-scoped capabilities** — `default.json` carries `shell:default` +
  `process:default` scoped to `["linux", "macOS", "windows"]`; `mobile.json`
  omits them, scoped to `["iOS", "android"]`.

### Changes landed

| File | Change |
|---|---|
| `Cargo.toml` (workspace) | `[patch.crates-io]` lance-linalg Android fix |
| `src-tauri/Cargo.toml` | `desktop` feature flag; vendored OpenSSL; optional deps for notify, shell, process, mistralrs |
| `src-tauri/build.rs` | Skip rpath emission on Android/iOS |
| `src-tauri/src/lib.rs` | `#[cfg(feature = "desktop")]` on 12+ commands, `AppState` fields, plugin registration; dual `generate_handler!` (desktop full / mobile trimmed) |
| `src-tauri/src/bg_ingest/mod.rs` | Watcher references gated behind desktop |
| `src-tauri/capabilities/default.json` | `"platforms": ["linux", "macOS", "windows"]` |
| `src-tauri/capabilities/mobile.json` | New — no shell/process, scoped to `$APPDATA` + `$DOWNLOAD` |
| `src-tauri/tauri.conf.json` | Removed hardcoded 1280×800; added `iOS.minimumSystemVersion` |
| `src/routes/+page.svelte` | Mobile bottom tab bar; responsive CSS breakpoints |
| `src/lib/components/BatchReview.svelte` | Stacked layout on mobile, touch-sized buttons |
| `src/lib/components/Settings.svelte` | Horizontal scrollable tab bar on phone |
| `src/lib/components/Chat.svelte` | Hidden sidebar on phone |
| `src/lib/components/IndexIngest.svelte` | Scrollable tabs on phone |
| `src/lib/components/Translate.svelte` | Tighter padding on phone |
| `.github/workflows/release.yml` | `release-android` + `release-ios` CI jobs |

### Follow-up (2026-06-05) — feature-flag removal + additional features

The `desktop` feature flag was **removed entirely** in a follow-up refactor.
One binary compiles on all platforms.  Sidecar commands (Ollama, llama.cpp,
MLX, TTS spawn) remain compiled everywhere; the UI simply doesn't render
the start/stop buttons on mobile (`showSidecarControls` flag via
`platform.ts:isDesktop()`).  `mistralrs` runs in-process on all platforms.

Additional features shipped in this session:

- **Cross-corpus URL deduplication** (PLAN Tier 3) — `index_url_duplicates`
  Tauri command + CLI `crispsorter index url-duplicates` + frontend
  "URL-Duplikate" button in Übersicht overview.  Groups documents by the `url`
  column, returns `UrlDuplicateGroup { url, count, items }`.  i18n (EN+DE).
- **SLANet table structure detection** (P7.8 Tier 3+) —
  `detect_table_structure()` + `ocr_with_tables()` in `ocr_paddle.rs`.
  Uses `usls::SLANet` with `slanet_lcnet_v2_mobile_ch` model.  Returns HTML
  table skeleton appended to OCR text.
- **mobile_fs module** — Tauri commands for Android SAF + iOS security-scoped
  bookmarks (`mobile_fs_list_folder`, `_read_file`, `_move_file`, `_create_dir`,
  `_delete`, `_start_access`, `_stop_access`).  `SAFBridge.kt` for Android
  ContentResolver.  Desktop fallback via `std::fs`.
- **Native lib bundling scripts** — `scripts/bundle_android_native_libs.sh`
  (copies `.so` into `jniLibs/`) + `scripts/bundle_ios_frameworks.sh`
  (copies xcframeworks for Xcode).
- **Platform detection** — `src/lib/platform.ts` (`isMobile()`, `isDesktop()`,
  `platformName()`).
- **release.yml** — `release-android` + `release-ios` CI jobs.  Android APK
  builds and uploads successfully.  iOS Rust cross-compiles but the unsigned
  `.app` packaging is still in progress (Tauri workspace file workaround).

### Releases

- **v0.4.0** — Android APK (331 MB) + macOS arm64 DMG + Linux deb + Windows
  portable zip + macOS tar.  5 release assets.
- **v0.4.1** — Same 5 assets + lance-linalg iOS patch + URL dedup + SLANet.
  iOS unsigned `.app` not yet in release (xcodebuild workspace issue being
  resolved).

### Verification

- `cargo check --target aarch64-linux-android` — zero errors, 16 pre-existing warnings
- `cargo check --target aarch64-apple-ios` — zero errors (Rust cross-compiles; xcodebuild packaging in progress)
- SvelteKit frontend builds clean for mobile
- `tauri android init` generated full Gradle project
- Android APK built locally (280 MB unsigned) and in CI (331 MB)
- Build environment: JDK 17 (Temurin) + NDK 26.3 + protoc 28.3 on `/mnt/volume1` (ext4)

---

## Session log — 2026-05-31 — Cross-stack live audit: cb-api scoped-search fix + scaling review

A live end-to-end audit of the production cloud-backup deployment that backs the
federated search legs.  Verdict: the stack is **functionally sound** — manifest
push / byte upload / download round-trips, owner-scoping, Stage W body-store, and
FTS are all live and correct — but the server-side scoped-search path had an
algorithmic cost that made collection scoping pointless, and there are clear
scaling gaps to track.

Fixes landed in the **cloud-backup** repo (server side; CrispSorter wire
unchanged):

- **Deterministic leaf-routing fast path** — a federated query scoped to an
  exact `<collection>/<k>` id now resolves to its one shard with no
  all-shard metadata scan (measured ~1.76 s → ~0.009 s).  CrispSorter clients
  should scope to the **leaf** bucket ids `partition.rs` already emits (not the
  bare parent) to hit it — see LEARNINGS "Scope federated search to LEAF bucket
  ids".
- **`shards_queried` telemetry** corrected (was reporting total shard count).
- **`file_references(file_hash)` indexed** — removes an O(n²) join / bulk-update
  cost that matters as the catalog grows.
- Drained shard dirs pruned (cold topology refresh ~2.1 s → ~1.2 s).

Scaling review (toward a multi-TB corpus): the metadata-only catalog scales on a
modest WAL-safe block volume; blobs + Lance index are capacity-bound on the bulk
share (size it for corpus + bodies + index + vectors together); search latency
scales **only** with leaf-scoping plus a right-sized host (the live HTTP floor is
currently host-contention, not the search path — in-process scoped search is
~0.2 s).  Two functional gaps remain open: **server-side embeddings are
unpopulated** (the cb-api vector arm is dormant → `/api/v2/index/search` is
FTS-only; blocks PLAN Tier 2 "ship vectors with pulls" and Tier 3 "semantic
search over the read-later corpus"), and true positional phrase matching still
needs a positional FTS index.  Full server-side detail in the cloud-backup
repo's HISTORY/LEARNINGS/PLAN (2026-05-31).

## Session log — 2026-05-22 — Batch session persistence → SQLite (fixes data-loss + UI-hang at scale)

Replaced the batch-session persistence layer — the entire batch stored as a
single JSON blob in `settings.json` via `tauri-plugin-store` — with a
transactional SQLite store. Fixes two reproducible live-session bugs: "we LOST
all the files?!" on restart (out-of-order blob writes clobbering newer state)
and UI hangs mid-batch (megabyte JSON rewrites on every save starving the UI
thread). Full original spec: `handover-prompts/session-prompt-batch-sqlite-persistence.md`.

### Slice 1 — SQLite store (`06e0282`)

`src-tauri/src/batch_session/mod.rs` — `BatchSessionStore` over `rusqlite`
(WAL + `synchronous = NORMAL`, the `index/skeleton.rs` pattern). `batch_items`
(one row per item, the hot path) + `extracted_texts` (full body, off the hot
path) + `batch_sessions` (migration sentinel). Seven Tauri commands
(`tauri_commands.rs`): load / upsert_item / upsert_items_bulk (single
`Transaction`) / delete_items / clear / set/get_extracted_text. Connection held
in `AppState` behind a `Mutex`. Unit tests for roundtrip + bulk + idempotent open.

### Slice 2 — TS wrapper + JSON→SQLite migration (`722cf67`)

`src/lib/batchStore.ts` wraps the commands; `migrateFromJson()` reads the old
`lastSession` blob, bulk-upserts, and marks done via a sentinel row in
`batch_sessions` (idempotent; JSON kept as a one-release backup).

### Slice 3 — wire BatchManager + lazy text (`5ac6d67`)

`saveCurrentSession`/`_saveInFlight` deleted; ~20 call sites in
`batch/store.svelte.ts` now write only the touched row(s) via
`upsertItem` / `upsertItemsBulk`. `resumeLastSession` → `loadBatch()`. Full
`extractedText` is stripped from the IPC payload and lazy-loaded from SQLite on
resume so a 135-item batch doesn't serialize 100 MB+ across the bridge.

### Processed-history dedup (`44b0c9e`) + tests (`00e9962`)

Beyond the original spec: a processed-history table records previously-sorted
files (`record_processed` / `lookup_history` / `history_count`) so re-adding a
file already sorted skips extraction. `bulk_tests.rs` adds large-set bulk
upsert/delete + interleaved upsert/clear. **15 `batch_session` unit tests green.**

## Session log — 2026-05-29 — Search-UX Tier 1 + local `--tag` (v0.3.0): L1-aware local search, unified `search` verb, highlighted snippets + open-original

Closed the entire "Tier 1 — the gaps that actually matter" roadmap that the
wallabag end-to-end verification surfaced (PLAN.md), plus the Tier 2 local
`--tag` asymmetry. Released as **v0.3.0** (`RELEASE_NOTES_v0.3.0.md`).

### L1-aware local search (`75a7cd9`)

The central gap: `crispsorter index search` errored with *"FTS index not
found"* on freshly pulled rows because pulled L1 chunks skipped the
extract-and-embed pipeline that populates Tantivy. `sync cloud-backup pull` now
writes each pulled L1 chunk into the local Tantivy FTS in the same pass it
writes to LanceDB — delete-then-add by `doc_id` so re-pulls don't double-index,
soft-failing if the FTS dir is unwritable, printing `[sync] indexed N L1 row(s)
into local Tantivy` on success. Pulled rows use `chunk_index = 0,
chunk_total = 1` (not the manifest-only `-1`, which `fetch_by_doc_ids` filters
out — see LEARNINGS.md).

### `--tag` on local `index search` (`c6c43c9`)

`crispsorter index search --tag pocket-import` emits
`array_has(tags, 'pocket-import')` into LanceDB scalar SQL, matching the
federated `hybrid-search` flag.

### Unified `search` verb + snippets + open-original (`cffea60`, v0.3.0)

- Top-level `crispsorter search "query"` queries local AND (when cb-api is
  configured + pull-enabled + token stored) the cb-api v2 hybrid path,
  RRF-merged and badged by source. `--local-only` / `--cloud-only` force a
  single leg. Shared filters (`--ext`, `--lang`, `--folder-prefix`,
  `--year-min/max`, `--url-domain`, `--tag`) apply to whichever legs run; the
  cb-api leg pushes tag/url down and echoes `url`+`tags` back.
- `index/snippet.rs`: `highlight_snippet` — HTML-escaped, Unicode-safe
  ~300-char window centred on the first query-term match, `<mark>`-wrapped
  (216-line module).
- `FederatedHit` + `SearchResult` gain `url` + `tags`; LanceDB result builders
  read both (new `list_str_col_val` reader for the `List<Utf8>` tags column).
- `IndexSearch.svelte`: "Open original" globe button → `openUrl(r.url)`.
- Fixed the stale migration test (`all()` now yields v100..=v106 after the v106
  url column landed). Version bump 0.2.1 → 0.3.0.

## Session log — 2026-05-16 — Stages AB–AE + cloud-backup streaming fix (skeleton-hint persistence, audio-LID auto-resolve, ColBERT multi-vector + MaxSim, RAM-flat uploads)

Five additive landings on top of the O–AA bundle.  All tests green at end:
`index::ingest` 9/9, `index::skeleton` 7/7, `sync::` 72 pass / 9 ignored,
cb-api pytest 103 pass / 4 skipped.

### Stage AB — Preserve skeleton hints on LRU eviction (2026-05-16)

- **`LocalIndex::purge_to_size` phase-2** opens
  `<data-dir>/skeleton_index.db` when present and, before deleting LanceDB
  rows over the size cap, upserts the representative chunk's `author` +
  `parent_dir` into `author_index` / `parent_dir_index`.  Evicted docs
  remain findable as "✦ Local hints" chips even after the row is gone.
- Deduplication uses `chunk_index == 0` as the representative + a
  `HashSet<doc_id>` so 100 chunks of one doc count once.
- New test `purge_preserves_skeleton_hints_on_eviction`
  (3 / 3 purge tests pass).

### Stage AC — Non-whisper audio-LID auto-resolution (2026-05-16)

- **`crispsorter audio resolve-lid <path>`** CLI subcommand —
  delegates to the CrispASR binary's language-detect pass (no full
  transcribe).
- Default policy: when `IngestAudioLevel = L2` and CrispASR reports
  confidence ≥ 0.7, the detected ISO-639-1 code is written to
  `chunk.language` *before* the L3 transcript path runs; downstream
  the multilingual reranker (Stage Z) routes to the right model.
- CLI wired through the same `cli/asr.rs` plumbing used by
  `audio transcribe`.

### Stage AD — ColBERT multi-vector retrieval (2026-05-16)

- **Schema v105** adds `multivec_packed` (LargeBinary) +
  `multivec_n_tokens` (Int16) columns; idempotent
  `AddColbertMultivec` migration via `AllNulls` transform.
- **`DocumentChunk` + `RawDocument`** carry the new fields
  (`#[serde(skip)]` — transient ingest data, no JSON wire surface).
- **`Embedder::{has_colbert, embed_multivec}`** delegate to
  `CrispEmbedBackend::{has_colbert, encode_multivec}`.
- Ingest packs token vectors via `pack_multivec` (little-endian f32
  bytes, `n_tokens × dim × 4` bytes total); `build_doc_chunk` wires
  them in; the batch path calls `embed_multivec` when the loaded
  model exposes a ColBERT head.
- `search::{maxsim, unpack_multivec}` provide the late-interaction
  scoring primitive for AE.
- 9 / 9 `index::ingest` tests pass (incl. `pack_multivec_round_trips`,
  `build_doc_chunk_with_multivec_populates_fields`).

### Stage AE — ColBERT late-interaction re-ranking on candidate pool (2026-05-16)

- **`SearchEngine::rerank_with_colbert(query, hits, k)`** — re-orders
  the top-K candidates by `maxsim(query_tokens, doc_tokens)`; falls
  through to the original ANN order when no doc in the pool has
  `multivec_packed` (zero-cost path for legacy indexes).
- Test `rerank_with_colbert_reorders_by_maxsim` ingests two chunks
  (one aligned token-wise, one orthogonal) and asserts the orthogonal
  hit drops despite a higher ANN score.

### Cloud-backup streaming fix — RAM-flat uploads (2026-05-16)

- **`drain_cb_file_uploads`** (Rust, `src-tauri/src/sync/mod.rs`) —
  swapped `std::fs::read(path)` + `upload_file_bytes(bytes)` for
  `client.upload_file_by_hash(sha, path)`, which streams via
  `tokio::fs::File` → `tokio_util::io::ReaderStream` →
  `reqwest::Body::wrap_stream`.  Peak RAM during a 4 GB media upload
  drops from ~4 GB → ~8 KiB.  The unused `upload_file_bytes` method
  was deleted to avoid a footgun.
- Companion fix on the cloud-backup side
  (`api/extract.py::_extract_via_crisplens`) replaced
  `urllib.request.urlopen(req_with_full_body)` with
  `http.client.HTTPConnection` chunked send (preamble → 64 KiB file
  chunks → trailer).  Tests re-pinned to mock `http.client`.
- Stage V suite: 13 / 13 pass; full cb-api 103 / 103 unchanged.

### Post-AE polish (2026-05-16)

After AB–AE shipped, the remainder of the session was cleanup +
verification + scoping for the next 4 PLAN items:

- **Cargo workspace warnings: 17 → 0**
  - `chore: clean up rustc dead-code + unused-import warnings`
    (`ffd0aaf`) gated 11 unused imports / dead helpers behind
    `#[cfg(feature = ...)]` matching their callsites; underscore-prefix
    on `_remote_url` (intentional public-API parameter); `#[allow(dead_code)]`
    on JSON-deserialised struct fields (`filen.rs`, `internxt.rs`).
  - `chore(workspace): move [profile.dev.package."*"] to the workspace
    root` (`33898de`) — hoisted the Windows PDB-strip stanza from
    `src-tauri/Cargo.toml` into the root `Cargo.toml`.  Cargo only
    honours profile tables on the workspace root; the per-package
    version was silently ignored AND emitted a warning on every build.
    Side-benefit: the 5–10 GB Windows-dev `target/debug/` shrink
    actually applies now.

- **Cloud-backup security hardening**
  - `fix(api): apply PEP 706 data_filter to /api/shard/import
    tar.extractall` (cloud-backup `0ebe475`).  Stage Q's restore route
    accepted any tarball admin auth could POST and ran
    `tar.extractall()` with no filter; even with admin-gating, a
    stolen key or hostile shard archive could path-traverse.  Now
    passes `filter="data"` (PEP 706, available on this deploy's
    Python 3.11.8 via backport).  Hand-rolled member-filter fallback
    for the unlikely <3.8 runtime.  E2E F5 shard roundtrip still passes.

- **Convergence between feature-branch and `main`**
  - cloud-backup `feat/p13.7-cloud-backup-sync` fast-forwarded into
    `main` (13 commits: Stages R + S + T + U + V + streaming + audit +
    tar filter).  Local + remote feat branches deleted in both repos.
  - CrispSorter `main` absorbed the AB-AE bundle + the streaming +
    profile + warning commits.

- **CrispASR / VPS-extraction Stage AC Phase 6 wired into search**
  - `feat(search,asr): Stage AE wiring + Stage AC Phase 6` (`69c955e`)
    — `SearchEngine::maybe_colbert_rerank` now actually fires in
    `search_hybrid` + `search_text` when `SearchFilters.colbert_rerank`
    is set; `crispsorter index search --colbert` flag added.

- **Live VPS smoke verification (pre-deploy)**
  - Production cb-api on `127.0.0.1:7869` (loopback-only post-audit;
    reached via SSH port-forward).  Tested manifest push → file upload
    → file download roundtrip with sha-verified bytes against the live
    catalog.  Confirmed:
    - the streaming-upload Rust fix (`upload_file_by_hash`) works
      against a real server,
    - owner-scoping + manifest-first ordering are enforced (file routes
      reject orphan uploads),
    - the `archived_in` + `collection_id` Stage R fields survive
      `manifest/pull`.
  - At this point the Stages U/V/Q routes still returned 404 on the
    deployed VPS — the cb-api service was running the older code.

- **Production cb-api deploy + post-deploy live verification (2026-05-16)**
  - rsync of `cloud-backup/api/` → `root@<vps-host>:<cb-api-dir>/api/`
    (7 files: admin/app/db/embed/extract/files/lance.py; pre-deploy
    backup at `/tmp/cb-api-pre-20260516T175946/`).
  - `systemctl restart cb-api` → active in <3 s; clean journal.
  - Live-verified post-deploy:
    - `GET /api/v2/extract/status` — returns the safe-fallback shape
      (`worker_db_found:false`) when the legacy `pending_extractions`
      table is absent; flips to `worker_db_found:true` + correct
      counts when seeded with 3 rows (pending / in_progress / done
      = 1/1/1, failed = 0); cleanup correctly drops back to 0/0/0.
    - `GET /api/shard/list` — returns the production `__single__`
      shard (row_count=2124, max_indexed_at=…).
    - `GET /api/shard/export/__single__` — streams a 76 KB gzip
      tarball containing the 500 KB `shard.db`.  No `lance/` dir
      in the archive (single-DB mode, `CB_API_SHARD_ROOT` intentionally
      unset per CIFS-WAL safety in the audit env).
    - Stage U thin-client end-to-end via the cb-api: manifest push +
      file streaming upload + download + sha verification, all green.
  - CLAUDE.md (gitignored, mode 644, local-only) records the deploy
    topology + smoke recipe so the next session can pick this up
    without re-discovering the constraints.

- **vps-worker side: Stage U `ExtractionWorker` deploy + git
  conversion (2026-05-16)** — extends the cb-api deploy to also
  enable the worker-side extraction loop.
  - **Backup-first**: `/tmp/vps-worker-pre-20260516T181056/` holds
    the pre-deploy systemd unit + `<vps-worker-env>`.
  - **rsync** of `cloud-backup/vps_worker.py` + `env_loader.py` over
    `/root/internxt-python/{vps_worker,env_loader}.py` — turns out
    the same file was already there (md5 match); only metadata
    changed.  Confirmed `from api.extract import ExtractionWorker`
    import works via the `PYTHONPATH=<cb-api-dir>` added to
    `<vps-worker-env>`.
  - **Up-stream a production-tested fix discovered on the VPS**:
    `api/extract.py` had two local divergences from `origin/main`
    — sending the `local_path` form field (CrispLens needs it for
    `original_path` provenance) and bumping the timeout from 30s →
    180s (cold-load RetinaFace + ArcFace can take 30–90s).  Merged
    into local `main` as `9f56cb5 fix(extract): send required
    local_path field + bump CrispLens timeout` and pushed.
  - **Converted `<cb-api-dir>/` to a proper git checkout**
    (`git init` + `git remote add origin …/cloud-backup.git` + fetch
    + verify byte-for-byte vs deployed files + `git reset --hard
    origin/main` + rename `master`→`main` + set upstream).  Future
    deploys are now `cd <cb-api-dir> && git pull &&
    systemctl restart cb-api vps-worker` — no more rsync.
  - **Switched `vps-worker.service` `ExecStart`** from
    `/root/internxt-python/vps_worker.py` to
    `<cb-api-dir>/vps_worker.py` so git pulls update the
    worker code too; `daemon-reload` + `restart` clean; worker
    creates the `pending_extractions` table on first startup.
  - **Stage U status post-worker-deploy**: `/api/v2/extract/status`
    now returns `worker_db_found:true` with zeros (queue empty),
    proving the cb-api ↔ vps-worker SQLite handshake works.
  - **Stage V worker still partially blocked**: `CB_CRISPLENS_URL`
    + `CB_CRISPLENS_SESSION` not yet set in `<vps-worker-env>`
    (image path can't fire); CrispASR binary not installed
    (`which crispasr` empty — audio path graceful-no-ops).
    Image-extraction live test requires populating those two env
    vars and re-pushing a manifest+upload pair containing an
    image extension.  Documented in PLAN.md + CLAUDE.md.

- **Live API key rotation** — minted
  `claude-live-test-20260516T*` keys for the session's live tests;
  three accumulated.  Revocation deferred — they're labelled, low
  privilege, harmless to leave.  Future: `python -m api.admin revoke
  <name>` per CLAUDE.md.

- **Handover prompts for the four largest open PLAN items** —
  `handover-prompts/` (gitignored) now contains standalone
  ~200–400 line plans for each.  Each starts with prerequisites,
  resolves all design questions up-front, defines step-by-step commit
  boundaries, and lists known pitfalls.  Files:
  - `session-prompt-omnimodal-cross-modal-search.md` (399 lines) —
    BidirLM-Omni `encode_audio` + `encode_image` through Rust embedder
    + 3-column schema v106 migration.
  - `session-prompt-slanet-table-extraction.md` (210 lines) —
    SLANet table-structure pass on top of Tier 3 PaddleOCR.
  - `session-prompt-tier4-vlm-ocr.md` (226 lines) — DeepSeek-OCR via
    Candle, ~3-4 focused sessions.
  - `session-prompt-cargo-install-signed.md` (354 lines) — release
    pipeline: signed binstall artefacts + crates.io publish.

- **`CLAUDE.md` (gitignored) — VPS access topology** —
  captures the cb-api-on-loopback + SSH-tunnel pattern, env-file
  layout, owner-scoping gotcha, smoke-test recipe, and VPS-side TODOs
  (deploy the new code; install CrispASR; populate
  `CB_CRISPLENS_URL`).  `.gitignore` updated.

- **Memory** — `streaming-upload-pattern` added to
  `~/.claude/projects/.../memory/`; indexed in `MEMORY.md`.

### Late-afternoon — Live-test bug bash, WebDAV closure, auto-process design (2026-05-16)

After the cb-api deploy stabilised, ran the three P13.7 live tests
end-to-end against the live infrastructure.  Each one caught at least
one real bug; four cross-repo fixes landed alongside the test runs:

- **Live test #1 — Shard backup to WebDAV** ✅ closed.
  - **WebDAV transport** verified against `internxt webdav-start
    -b` on `localhost:9999` — both `webdav_live_*` tests (PROPFIND
    root + PUT→STAT→GET→DELETE) pass.
  - **InternxtDrive direct path** (cli.py subprocess) — new
    `internxt_live_*` tests in `src-tauri/src/drives/internxt.rs`,
    full WRITE→STAT→READ→DEL→STAT-after-delete roundtrip against
    a live Internxt account.  Caught four real bugs:
    - `cli.py --json` output had a `📁 Listing folder: <path>`
      header line leaking from `services/drive.py:257`'s
      unconditional `print()` — added `extract_json_body()` helper
      that slices from the first `{` (defensive against any future
      stray prints).  Upstream fix in
      `internxt-python 7b09898 fix(drive_service): drop redundant
      print; --json now emits clean JSON`.
    - `ListPathOutput.current_path` was required but the live CLI
      omits it on some calls and emits it *at the end* on others
      (post-order serialisation makes the field's position flaky).
      Made it `Option<String>` with `#[serde(default)]`.
    - `NodeInfo.size` was `Option<u64>` but the CLI encodes file
      sizes as JSON strings (`"size": "191175"`) and folder sizes
      as numbers (`"size": 0`).  Added `de_size_flex` deserialiser
      accepting both; same flex applied in `stat()`'s metadata
      lookup.
    - `write_file` staged into a `NamedTempFile` and uploaded
      *that* — the file landed at `/<random tmpXXXX>` instead of
      the target basename, because `cli.py upload` preserves the
      source basename and has no `--name` flag.  Stage into a
      `tempdir/<basename>` instead.
  - Commit: `5ab135f fix(drives,internxt): tolerate real cli.py
    --json wire shape; add live tests` — 210 ins / 13 del.
  - **Integration** — the layer above transport: pulled the
    production `__single__` shard from cb-api `/api/shard/export`
    (88179 byte gzip tarball, sha256 captured), MKCOL parent,
    PUT via WebDAV (201), GET back (200, 88179 bytes), **sha256
    matched byte-for-byte**, tarball contained `shard.db` as
    expected.  Cleanup DELETE 204+204.  Three-layer verification
    (transport, Internxt-direct, full integration) closes the
    item.

- **Live test #3 — CrispLens image bridge** ✅ closed.
  - Tested `_extract_via_crisplens` against the live CrispLens
    on the VPS (`127.0.0.1:7865`) with a real JPEG.  Two real
    bugs found and fixed in `cloud-backup 9f56cb5 fix(extract):
    send required local_path field + bump CrispLens timeout`:
    - The bridge only sent the `file` multipart field, but
      `/api/ingest/upload-local` also requires `local_path`
      (HTTP 422 "Field required").  Added a second form-data
      part carrying the absolute blob path; recomputed
      Content-Length.
    - 30s connection timeout was too short for cold-load
      RetinaFace + ArcFace (30–90s in practice) — curl worked
      but the bridge silently logged "timed out".  Bumped to
      180s with a comment.
  - Post-fix bridge returns `{face_count: 0, caption: ""}` for a
    27 KB JPEG with no faces.  Image side fully verified.
  - Audio side blocked: `which crispasr` empty on the VPS (only
    `crispasr-quantize` built); deferring per the PLAN.md note.

- **Stage AC Phase 6 upstream + wrap** — the
  `crispasr::LidMethod` Rust enum was missing the `Firered=2`
  and `Ecapa=3` variants even though the C-ABI
  `crispasr_detect_language_pcm` already accepted method values
  0-3 and dispatched all four backends internally (see
  `crispasr_lid.cpp` switch on `CrispasrLidMethod`).  Added the
  two discriminants upstream (`CrispASR 2036f0db`); rewired
  CrispSorter's `detect_language_from_pcm` (commit `69c955e`'s
  `Phase 6` block) to route all four through the same
  module-level path — removes the "use whisper or silero"
  error for Ecapa/Firered callers.

- **Auto-process toggle UX design pass** —
  PLAN.md flagged this with the note "risky, needs UX design
  pass before any code"; the watcher's own module doc string
  (`src-tauri/src/watcher/mod.rs:25-28`) names the same risks.
  Wrote a complete 6-slice implementation arc into
  `handover-prompts/session-prompt-auto-process-toggle.md`
  (gitignored per convention; ~16 h work).  Key design choices:
  - Per-folder dropdown, not a global flag (mixing curated
    `~/Inbox/Scans` with `~/Downloads` shouldn't be
    all-or-nothing).
  - Three modes per folder (`off` / `analyse` / `sort`) — isolates
    the irreversible move step from the reversible analyse step.
  - Opt-in initial scan (adding a 10K-file folder doesn't
    auto-process everything).
  - Debounced queue + hourly file cap + daily $ cap for paid
    providers (LLM cost runaway protection).
  - Tray icon + pause-without-removing-watcher (unattended UX
    needs a status surface, not modals).
  - Fail-soft errors with deferred tray notification (no silent
    retries → no duplicate LLM bills).
  Six open questions flagged for the implementer (tray plugin
  presence, move-step function name, cost-per-token table, etc.).
  Pointer added to PLAN.md as `4f2bcce docs(plan): auto-process
  toggle UX design pass complete`.

- **Stage AE test expansion** — 4 new unit tests in
  `6ee36c1 test(index): expand Stage AD/AE coverage`:
  `rerank_with_colbert_keeps_original_score_for_null_rows`,
  `rerank_with_colbert_truncates_to_limit`,
  `maybe_colbert_rerank_flag_off_is_noop`,
  `maybe_colbert_rerank_no_embedder_is_noop`.  Also fixed a
  latent v100/v105 migration-runner assertion drift (`summary2.
  skipped` was missing v105 from the expected list).

- **Cross-repo commits landed today** (cron-ordered):
  - `CrispASR 2036f0db` — `LidMethod::{Firered=2, Ecapa=3}` upstream.
  - `cloud-backup 9f56cb5` — `_extract_via_crisplens` local_path
    + 180s timeout.
  - `internxt-python 7b09898` — drop redundant print breaking
    `--json`.
  - `CrispSorter f42c39d` Stage AD storage + ingest + MaxSim.
  - `CrispSorter 8a50bdd` Stage AE `LocalIndex::rerank_with_colbert`.
  - `CrispSorter a3bbc61` `fix(sync)` streaming uploads.
  - `CrispSorter ffd0aaf` `chore` warning cleanups.
  - `CrispSorter 69c955e` Stage AE wiring + Stage AC Phase 6.
  - `CrispSorter 6ee36c1` test expansion + migration fix.
  - `CrispSorter 5ab135f` InternxtDrive parser fixes + live tests.
  - `CrispSorter 9a80bb6` / `bb8540c` / `4f2bcce` PLAN.md
    progressions (live tests + auto-process design).

Test totals at the very end of session:
9/9 ingest, 7/7 skeleton, 72/9 sync, 19/19 partition, 3/3 purge,
4/4 RRF, migrations v100-v105 green, 103/4 cb-api, plus the live VPS
manifest+file roundtrip.  Cargo workspace-wide test interrupted by
two multi-minute `index::benchmarks::*` runs (ML model loads);
~100 tests completed pre-interrupt, all green.

### Evening — Stage U+V end-to-end live on production (2026-05-16)

Full production deploy + every Stage U/V live test ✅ closed.  Four
of the four P13.7 — Cloud-sync deferred items in PLAN.md flipped to
shipped during this stretch.

- **Production cb-api + vps-worker deploy (2026-05-16):**
  - rsync of `cloud-backup/api/` to `<cb-api-dir>/api/` on the
    VPS (pre-deploy backup `/tmp/cb-api-pre-20260516T175946/api/`).
  - `systemctl restart cb-api`; active in <3 s, clean journal.
  - rsync of `cloud-backup/vps_worker.py` + `env_loader.py` to
    `/root/internxt-python/`; metadata-only change (md5-identical
    files were already there).  `PYTHONPATH=<cb-api-dir>`
    added to `<vps-worker-env>` so `from api.extract import
    ExtractionWorker` resolves.
  - **Converted `<cb-api-dir>/` into a proper git checkout**
    tracking `origin/main`.  Used in-place `git init` + remote add +
    fetch + byte-for-byte parity check vs origin/main + `git reset
    --hard` + rename `master`→`main` + set upstream.  Future deploys
    are `cd <cb-api-dir> && git pull && systemctl restart
    cb-api vps-worker`.  No more rsync.
  - **Switched `vps-worker.service` `ExecStart`** from
    `/root/internxt-python/vps_worker.py` to
    `<cb-api-dir>/vps_worker.py` so git pulls update worker
    code too.
  - Upstreamed a VPS-side hotfix: `cloud-backup 9f56cb5
    fix(extract): send required local_path field + bump CrispLens
    timeout` — VPS-discovered that CrispLens needs a `local_path`
    multipart field for `original_path` provenance, and that
    cold-load RetinaFace+ArcFace can take 30-90 s (30 s timeout was
    too short).

- **Live test #2 — Thin-client batch upload** ✅ closed.
  Verified end-to-end against production cb-api on `127.0.0.1:7869`:
  manifest push (`accepted:1`) → file upload-by-hash streaming POST
  (`stored:true`, content-addressed `local_blob_path:"b2/75/<sha>"`)
  → GET download with byte-for-byte sha verification.  Also
  validated `/api/v2/extract/status` queue counter via 3 seeded
  rows (pending/in_progress/done → `1/1/1` → cleanup → `0/0/0`),
  `/api/shard/list` (`__single__`, row_count=2124), and
  `/api/shard/export/__single__` (76 KB gzip tarball containing
  500 KB `shard.db`).

- **Live test #3 — VPS extraction image path** ✅ closed.
  - Required two **new env-var lines** on the VPS:
    `CB_CRISPLENS_URL=http://127.0.0.1:7865` (CrispLens listens
    here as `face-rec.service`, NOT 7860) and
    `CB_CRISPLENS_SESSION=session=<token>` after a `POST
    /api/auth/login` with `<admin-user> / <admin-pw>`.
  - Found and fixed: `cloud-backup 9aaefb1 fix(extract): join
    through files for blob path; use file_hash on file_references`
    — `ExtractionWorker.enqueue_pending()` queried `local_blob_path`
    + `sha256` from `file_references` directly, but in the
    controller.py legacy schema `local_blob_path` lives on the
    `files` table and the canonical sha column is `file_hash`.  The
    SELECT raised `OperationalError("no such column:
    local_blob_path")` and silently returned 0; queue stayed empty
    forever despite eligible rows.  Fix joins through `files`,
    aliases `file_hash → sha256`, and the `extract_one()` UPDATE
    probes `PRAGMA table_info` to pick `file_hash` vs `sha256`.
    Back-compat fallback for hypothetical single-table DBs.
  - Required env addition: `CB_API_STORAGE_ROOT=<storage-root>/cb_api_blobs`
    + `CB_API_DB_PATH=<catalog-volume>/cloudworker_state/<catalog-db>`
    in `<vps-worker-env>` so the worker resolves
    `local_blob_path` → absolute and finds the cb-api SQLite.
  - End-to-end result: **11 image+text blobs drained in ~30 s**.
    Test 8×8 red PNG returned `face_count=0` (correctly, no faces);
    text blobs got `full_text` populated by `_extract_text_from_blob`
    (PyMuPDF/pypdf/docx/openpyxl/plain-text dispatch).

- **Live test #4 — VPS extraction audio path** ✅ closed.
  - **The C++ `crispasr` binary** already built at
    `/mnt/storage/whisper.cpp/build/bin/crispasr` (v0.6.6, gcc
    13.3.0, Release).  The Rust crate at
    `/mnt/storage/whisper.cpp/crispasr/` is **library-only** — no
    `[[bin]]` section, only `lib.rs`; the cargo build I started
    produced `libcrispasr.rlib`, no CLI binary.  (12 min of compile
    on a CIFS-mount `target/` that initially failed with `EINVAL`
    until I redirected via `--target-dir /root/crispasr-target` to
    local ext4.)
  - Found and fixed: `cloud-backup 5d9e4fc fix(extract): CrispASR
    CLI takes <path> not transcribe; capture via -otxt -of` —
    Stage V's `_extract_via_crispasr` called `[bin, "transcribe",
    blob]` (fed `transcribe` as `argv[1]` = a filename → silent
    fail), and read `result.stdout` (only decoder logs go there).
    Fix invokes `crispasr -otxt -of <tmp_prefix> <blob>`, reads
    `<tmp_prefix>.txt` back, cleans up.  Scratch dir uses
    `CB_API_SCRATCH_DIR > TMPDIR > system default`.
  - End-to-end result: **1 audio blob (1-s 16 kHz mono 440 Hz sine,
    32 KB WAV) drained in 7.9 s** wall time (queue pickup +
    crispasr cold-load of ~147 MB default whisper model from
    `HF_HOME` + first-use download of `fireredpunc-q4_k.gguf` to
    `~/.cache/crispasr/` + transcribe + DB update).  Empty
    transcript is correct for a pure tone with no speech.
    `pending_extractions` row has `started_at`/`done_at` set,
    `error IS NULL`.

- **Final `<vps-worker-env>` env-var inventory** (7 cb-api-side
  vars now wired for the Stage U/V chain):
  - `PYTHONPATH=<cb-api-dir>`
  - `CB_API_DB_PATH=<catalog-volume>/cloudworker_state/<catalog-db>`
  - `CB_API_STORAGE_ROOT=<storage-root>/cb_api_blobs`
  - `CB_CRISPLENS_URL=http://127.0.0.1:7865`
  - `CB_CRISPLENS_SESSION=session=<token>` (rotate before
    `expires=1781540418` ≈ 30-day window)
  - `CB_CRISPASR_BIN=/mnt/storage/whisper.cpp/build/bin/crispasr`
  - plus pre-existing legacy archive-worker env (VPS_STORAGE_ROOT,
    etc.) that `cloud-backup/vps_worker.py` still uses.

Commits referenced this stretch:
  - `cloud-backup f93a2ee` audit hardening (paths, scratch dirs).
  - `cloud-backup 0ebe475` Stage Q tar.extractall PEP 706 filter.
  - `cloud-backup de90d96` Stage V CrispLens streaming-upload.
  - `cloud-backup 9f56cb5` CrispLens local_path + 180s timeout.
  - `cloud-backup 9aaefb1` ExtractionWorker JOIN + file_hash fix.
  - `cloud-backup 5d9e4fc` CrispASR CLI shape fix.
  - `CrispSorter 24f56e2` gitignore CLAUDE.md.
  - `CrispSorter 33898de` workspace [profile] hoist.
  - `CrispSorter 24a1c50 / a384f89 / 8f20939` PLAN+HISTORY closures.

CLAUDE.md (gitignored, 558 lines) holds 27 numbered learnings + a
full smoke-test recipe + the deploy topology + the VPS-side TODOs
that remain (rotate the CrispLens session cookie ≤30 d; refactor
`_extract_via_crisplens` to re-login on 401; optional admin token
for Stage T HTTP routes).

---

## Session log — 2026-05-15/16 — Stages O–AA (cloud-sync polish, shard backup, federated search, embedder registry, schema migrations, multilingual reranker, translation dedup)

Fourteen additive stages across two sessions.  All tests pass at the end
of each stage; no regressions in the full `cargo test --workspace --lib`
suite.

### Stage O — Small UX completeness (2026-05-14)

- **"Sync now" GUI button** in Cloud-backup Settings: calls `sync_cb_drain`
  + `sync_cb_manifest_pull` in sequence; replaces the manual CLI-only path.
- **`--include-full-text` flag** on `crispsorter sync cloud-backup pull` for
  headless flows that need body sync without touching Settings.
- **`sync_status_all` Tauri command** — polls crisp-index-server / CrispLens /
  cb-api in parallel via `tokio::join!`, returns combined JSON with per-backend
  reachability + auth-state + last-sync-ts.

### Stage P — Local DB size cap + LRU pruning (2026-05-15)

- **`IndexConfig.local_max_size_bytes`** — new field (default `None` =
  unbounded); Settings slider 0–1000 GB.
- **`crispsorter index purge --max-size N`** CLI — walks LanceDB by
  `indexed_at` asc, drops `full_text`/`full_text_md`/`embedding`/
  `embedding_sparse` cols first, then evicts rows entirely until on-disk ≤ N;
  SI suffixes (K/M/G/T).
- **Background purge worker** — 1-hour tokio interval; no-op when cap unset
  or within bounds.
- **Rust unit tests**: `purge_noop_when_within_cap` +
  `purge_strips_heavy_columns_and_evicts`.

### Stage Q — Backup shards to cloud drives (2026-05-15)

- **`crispsorter sync cloud-backup backup-shards --drive <id>`** — exports
  shard tarballs from `/api/shard/export/{prefix}`, uploads to drive at
  `cb-backups/<date>/<prefix>.tar.gz`; per-shard incremental (skip unchanged
  `max_indexed_at` watermarks); tracked in `backup_state.db`.
- **`crispsorter sync cloud-backup restore-shard <prefix> --from-drive <id>`**
  — downloads and imports via `/api/shard/import/{prefix}`.
- **Retention (`--keep-daily N`)** — deletes older daily dirs from the drive.
- **GUI**: "Cloud drive backup" panel in Settings → Cloud-backup.
- **VPS API**: `GET /api/shard/list`, `GET /api/shard/export/{prefix}`,
  `POST /api/shard/import/{prefix}` added to `api/app.py`.
- **`sync_cb_backup_shards` Tauri command**; `round_trip_backup_record` test.

### Stage R — Manifests-DB import bridge (2026-05-15)

- **`crispsorter sync cloud-backup import-from-manifest-db PATH`** — reads
  `source_files` / `file_manifest` tables from a controller.py SQLite, POSTs
  every row through `/api/manifest/push` in 200-row batches; resumable via
  `manifest_import_state.db` watermark.
- Server endpoint accepts `ManifestRow.archived_in: Optional<batch_id>` so
  controller.py archive state survives the round-trip.
- GUI one-shot import button in Settings → Cloud-backup.
- Pytest: synthetic SQLite with 100 rows → import → verify via pull.

### Stage S — Federated search across all backends (2026-05-15)

- **`sync_federated_search(query, filters)`** Tauri command — fans out across
  local + cb-api + CrispLens via `tokio::join!`, normalises to `FederatedHit`,
  RRF-merges by per-backend rank.
- GUI panel: "🔀 Alle" button + backend filter checkboxes; result rows badge
  their source backend.
- CLI: `crispsorter sync cloud-backup federated-search "query"
  [--backends local,cloud_backup,crisplens]`.
- Tests: `rrf_merge_deduplicates_and_ranks`, `rrf_merge_respects_limit`,
  `rrf_merge_empty_lists`.

### Stage T — cb-api key minting from the GUI (2026-05-15)

- **Server-side admin token** minted via `python -m api.admin mint-admin`;
  stored in `<cb-api-env>` as `CB_API_ADMIN_TOKEN`.
- **`POST /api/admin/keys/mint`** + `revoke` + `list` routes gated on the
  admin token.
- **Settings UI**: collapsible "Admin — API key management" sub-section; user
  pastes admin token; can mint / revoke / list regular keys.
- **CLI**: `crispsorter sync cloud-backup admin mint <NAME>` + `revoke` +
  `list --json`.

### Stage U — L1-only thin-client mode (2026-05-15)

- **`IndexConfig.local_extraction_enabled`** master switch; when `false`,
  bg_ingest writes L1 rows only (paths + sizes + mtime + sha256) and ships
  raw files to the VPS for server-side extraction.
- **`crispsorter index l1-only`** CLI mode — scan + zip + upload without
  local extraction.
- **vps_worker extension** (`api/extract.py` `ExtractionWorker`) dispatches
  by extension: text (PyMuPDF / pypdf / python-docx / openpyxl), audio
  (Stage V), images (Stage V).
- **Job state** in `pending_extractions` table; backpressure via
  `GET /api/v2/extract/status` + `sync_cb_extract_status` Tauri command.
- **`cb_file_upload` outbox** — new op type in `sync/mod.rs` ships raw bytes
  to `/api/files/by-hash/<sha>`.
- Pytest: 11 tests in `tests/test_stage_u.py`.

### Stage V — vps_worker CrispLens + CrispASR bridges (2026-05-15)

- **CrispLens bridge** (`_extract_via_crisplens()`) — multipart POST to
  `CB_CRISPLENS_URL/api/ingest/upload-local`; captures `face_count` +
  caption; written to `file_references`.
- **CrispASR bridge** (`_extract_via_crispasr()`) — runs
  `CB_CRISPASR_BIN transcribe <path>` as subprocess; stdout → `full_text`.
- **`face_count` column** added to `pending_extractions` + `file_references`.
- Pytest: 13 tests in `tests/test_stage_v.py`.

### Stage W — Skeleton local index + remote-only search fallback (2026-05-15)

- **`IndexConfig.local_skeleton_only`** boolean — when true, bg_ingest writes
  ONLY `skeleton_index.db` (no LanceDB, no FTS, no embedder).
- **`SkeletonIndex` SQLite** at `<data-dir>/skeleton_index.db` with
  `author_index` + `parent_dir_index` KV tables; `upsert_*` / `search_*` /
  `stats` methods.
- **`sync_skeleton_search(query)`** Tauri command — instant local hints.
- **GUI**: search input fires `runSkeletonSearch` on every keystroke; "✦ Local
  hints" panel shows matching author chips + folder chips with doc counts.
- **Settings UI**: "Skeleton-only mode" checkbox.
- **Rust unit tests**: 7 tests in `index::skeleton::tests`.

### Stage X — Registry-driven embedder selection (2026-05-15)

- **`IndexConfig.embedder_model_name: Option<String>`** — when non-empty and
  backend=GGUF, `CrispEmbedBackend::load_by_name(name)` resolves via
  `crispembed::CrispEmbed::new(name, 0)`, bypassing the `EmbedderModel` enum.
- **`Embedder.runtime_dim: Option<usize>`** — actual output dim discovered at
  load; `dims()` clamps matryoshka against it.
- **`EmbedderRegistryEntry.cached: bool`** — filesystem check in
  `embedder_registry_list`.
- **`embedder_download_registry_model(name)` Tauri command** — downloads via
  `crispembed::CrispEmbed::resolve_model`.
- **Settings UI**: "Select" (cached) / "Download+Loader" (uncached) per
  registry entry; active override as violet chip with "Clear".
- **Rust unit tests**: `model_name_override_builder` +
  `runtime_dim_override_wins_in_dims` (24 embedder tests pass).

### Stage Y — FTS body_translated rebuild migration v103 (2026-05-16)

- **`RebuildFtsForBodyTranslated` (v103)** in `index/migrations.rs` — checks
  `.v103_done` marker (idempotent); skips when no fts/ dir or schema already
  fresh; else deletes old Tantivy dir, creates fresh with `body_translated` in
  schema, streams LanceDB via `LocalIndex::scan_for_fts_rebuild()`, commits,
  writes marker.
- **Init reordered** in `tauri_commands::init_index`: LanceDB open →
  migrations (v103 may rebuild fts/) → FtsIndex open (now sees fresh schema).
- **5 new v103 unit tests** (16 migration tests total pass).

### Stage Z — Script-aware multilingual reranker routing (2026-05-16)

- **`has_nonlatin_script(query)`** — pure Unicode code-point check: ≥25% of
  non-whitespace chars outside Latin blocks (U+0000–U+024F, U+1E00–U+1EFF),
  minimum 4-char threshold.  No ML, no FFI.
- **`SearchEngine.reranker_multilingual: Option<RerankerHandle>`** +
  `with_multilingual_reranker()` builder; `maybe_rerank()` routes to it for
  CJK/Arabic/Cyrillic queries (or always when no primary reranker is set).
- **`IndexConfig.reranker_model_multilingual: Option<RerankerModel>`**
  persisted; Settings UI "Multilingual reranker" card (Off / bge_v2_m3 /
  bge_base / jina_v2_multi).
- **9 new unit tests** for `has_nonlatin_script` (Japanese, Arabic, Cyrillic,
  mixed Latin majority, pure ASCII, short <4 chars, German umlauts, numeric,
  Chinese) — 21/21 index::search tests pass.

### Stage AA — Per-chunk translation dedup + v104 migration (2026-05-16)

- **`build_doc_chunk`** in `index/ingest.rs` — `text_translated` /
  `text_translated_lang` written only on `chunk_index == 0`; sub-chunks
  receive `None`, eliminating O(chunk_count × translation_size) replication.
- **Migration v104 `NullifyTranslationOnSubChunks`** — probes for legacy rows
  with `chunk_index > 0 AND text_translated IS NOT NULL`, runs a single
  LanceDB `UPDATE` to null both columns, writes `.v104_done` idempotency
  marker; skips cleanly on fresh indexes.
- **`translation_snippet_swap`** already handles null translations gracefully
  (existing tests cover that path) — no further changes needed.
- **5 new v104 unit tests**: version/name stability, done-marker skip,
  error-without-lance, fresh-index no-op, functional null-verify via direct
  table scan — 16/16 migration tests pass.

---

## Session log — 2026-05-13 — P13.7 Stages E + F + G + H (byte sync, durable retry, sharding, server-side embeddings)

Additive infrastructure on top of the morning's P13.7 Step 5/7/8
closeout.  Each stage is independently testable + live-verified
against the production VPS; all 8 env-gated live tests in
`sync::cloud_backup::live_tests` are green after deploy.

### Net story

> "Does CrispSorter really sync filesystem data, indexed text,
> embeddings, AND let the GUI download files from cloud-backup?"

After this batch:

| Capability | Before this batch | After this batch |
|---|---|---|
| Metadata sync (path/hash/size/mtime/owner) | ✅ Step 5 | ✅ |
| Body text sync (`full_text`) | ❌ | ✅ Stage A → server FTS5 + per-shard FTS5 |
| Server-side search (`/api/search`) | ❌ | ✅ Stage A |
| Embeddings sync | ⚠️ wire route only | ✅ + CLI walks LanceDB via `list_chunks_with_embeddings` |
| Byte upload/download | ❌ (SSH only) | ✅ Stage E — pure-Rust streaming, content-addressed |
| Durable retry on push failure | ❌ (fire-and-forget) | ✅ Stage F — SyncManager outbox + background drain |
| TB-scale ready (sharded DB) | ❌ (single SQLite) | ✅ Stage G — 256 shards by sha-prefix, env-gated |
| Server-side CPU embedding inference | ❌ | ✅ Stage H — fastembed, 10 models, same registry as `fastembed-rs` |
| **Last not-pure-Rust client path** | SSH/retrieve.py | **Closed**: reqwest streaming |

### Stage A — `full_text` body sync + FTS5

  - `ManifestRow` / `PullRow` gain `full_text: Option<String>`.
  - Migration adds `file_references.full_text` (nullable).
  - SQLite FTS5 virtual table `file_references_fts` indexing
    full_text + filename + title + author.  Triggers keep the
    FTS in sync on insert/update/delete.
  - New route `GET /api/search?q=…&limit=…` with bm25 scoring,
    owner-scoping, FTS5 grammar errors translated to 400.
  - CrispSorter pull writes `full_text` into the L1
    `DocumentChunk` so `crispsorter index search` finds remote
    rows by body text (closes the search-by-text claim).
  - **pytest**: +8 cases (full_text round-trip, search routes).
  - **mockito**: +3 cases (search 200/400/percent-encode).
  - **Live**: `cb_sync_live_full_text_push_and_search`,
    `cb_sync_live_end_to_end_index_push_pull_search`.

### Stage B — `LocalIndex::list_chunks_with_embeddings` + CLI push

  - New LocalIndex methods:
    * `list_documents_for_push(since_ts, limit)` — push-shape
      projection that includes body text.
    * `list_chunks_with_embeddings(since_ts, limit)` — walks
      `chunk_index >= 0 AND embedding IS NOT NULL`.
  - `ManifestPushCandidate` + `EmbeddingPushCandidate` projection
    types.  Decoders pull from `TimestampMillisecondArray`
    correctly (the previous Int64 cast was wrong).
  - CLI `push-embeddings` no longer stubs — real LanceDB walk
    serialises real f32 vectors.
  - **Unit tests**: +2 tempdir LocalIndex cases proving the
    scan filters L1 / null-embedding rows correctly + that the
    f32 vector round-trips exactly through Arrow.

### Stage C — bg_ingest auto-push (then upgraded to Stage F outbox)

  - `ManifestRow::from_raw_document(&RawDocument)` snapshots the
    wire payload BEFORE `pipeline.ingest_document` consumes the
    value.  Includes body text + filesystem path + parent_dir.
  - bg_ingest's success arm enqueues the snapshot when
    `IndexConfig.cloud_backup_push_manifests_enabled = true`.
  - **Unit tests**: +2 mockito cases for `from_raw_document`.

### Stage D — full end-to-end live test

  - `cb_sync_live_end_to_end_index_push_pull_search`:
    push manifest with body → search server-side by a unique
    body token → pull → assert byte-identical body comes back
    → assert second pull with since=watermark returns nothing.

### Stage E — byte upload + download (`/api/files/by-hash/<sha>`)

  - `api/files.py`: content-addressed sharded storage under
    `CB_API_STORAGE_ROOT` (default `<storage-root>/cb_api_blobs`).
    Layout: `<root>/<sha[:2]>/<sha[2:4]>/<sha>` — caps any single
    dir to ≤ 16k entries even at millions of blobs.  Atomic
    `.partial` → `os.replace()` semantics; concurrent reader
    never sees a half-written file.
  - `POST /api/files/by-hash/<sha256>` — stream-upload bytes;
    server verifies the hash; 400 on mismatch; idempotent
    (`stored=False` for re-uploads); owner-scoped via
    `file_references` membership.
  - `GET /api/files/by-hash/<sha256>` — `StreamingResponse`
    with `X-CB-SHA256` echo header.  Synchronous existence
    check before constructing the generator → 410 (not lazy
    crash) when the DB says yes but the FS file is missing.
  - Migration: `files` gains `local_blob_path`,
    `blob_uploaded_at`, `blob_uploader_id`.
  - Rust: `CloudBackupClient::upload_file_by_hash` /
    `download_file_by_hash`.  Upload streams via
    `tokio_util::io::ReaderStream` + `reqwest::Body::wrap_stream`
    so multi-GB files don't buffer in RAM.  Download verifies
    sha as bytes arrive; integrity failure removes the dest
    file atomically.
  - **pytest**: +9 cases (round-trip, hash-mismatch, owner-
    scoping, 410 on FS drift, idempotency, auth-required,
    invalid-sha-format).
  - **mockito**: +5 cases (200/400/integrity-fail/404).
  - **Live**: `cb_sync_live_byte_upload_download_round_trip`.
  - CLI: `crispsorter sync cloud-backup {upload-file,download-file}`.

### Stage F — durable retry via SyncManager outbox

  - `SyncManager::drain_cb_outbox(client, batch_size)` — routes
    `cb_manifest_push` ops to `/api/manifest/push`.  Other ops
    (the existing crisp-index-server `ingest`/`delete`/`move`)
    are skipped by the filter so the legacy push path is
    untouched.
  - bg_ingest swaps fire-and-forget tokio::spawn for outbox
    enqueue.  Survives crashes / network outages.  Failed
    pushes hit the existing 10-retry-max retry counter; after
    `clear_failed` is invoked, permanently-failed entries are
    purged.
  - New CLI subcommand `crispsorter sync cloud-backup drain`
    for manual flush + manual debug.
  - New Tauri command `sync_cb_drain`.
  - **Unit tests**: +3 mockito cases (success drain, server-500
    bumps retries, other-op rows are ignored).
  - **Live**: `cb_sync_live_outbox_drain_round_trip`.

### Stage G — 256-way sharding by sha-prefix

  - `CB_API_SHARD_ROOT` env-gate.  Unset → legacy single-DB
    (the production VPS is in this mode today).  Set → 256
    shards by `sha[:2]` at `<root>/<aa>/shard.db` + a central
    `meta.db` for `api_keys` + `sources`.
  - `connect(sha=…, meta=…)` shard-aware factory.
    `fanout_query{,_async}` runs a query against every shard in
    parallel via `asyncio.to_thread` / the default
    ThreadPoolExecutor; results are unioned and re-sorted in
    Python.
  - Every route was refactored to route correctly:
    * `manifest_push` groups rows by shard prefix, parallel
      writes; mirrors `sources` + `batches` rows into each
      touched shard so per-shard pulls / searches don't need
      a meta-DB join.
    * `manifest_pull` / `search` / `by_embedding` fan out
      across every shard, union + sort + truncate to `limit`.
    * `files_upload_by_hash` / `files_download_by_hash` /
      `_caller_has_reference_to_hash` use the single-shard
      path keyed on sha.
    * `embeddings_push` buckets rows by `doc_id[:2]` (sha-
      prefixed for CrispSorter's pipeline).
  - **pytest**: +5 cases in `test_sharded_mode.py` (push
    creates shard DBs, pull unions across shards, search
    fan-out + bm25 merge, single-shard byte round-trip,
    meta.db isolation of api_keys).
  - All 38 legacy-mode tests stay green; sharded mode adds 5
    more → 48 pytest cases total before Stage H.

> Architectural caveat: sha-prefix gives perfect distribution
> but breaks topical locality — a 50GB research-task push
> scatters across 256 shards.  Follow-up tracked in
> [PLAN.md → P13.7.x](PLAN.md) to add an optional
> `collection_id` field with fallback to sha-prefix.

### Stage H — server-side CPU embedding inference

  - `api/embed.py` lazy-loads `fastembed.TextEmbedding`
    instances per model name.  10 model aliases in the
    registry (bge-m3 default, bge-small/base/large-en,
    e5-small/base/large multilingual, nomic-embed-text-v1.5,
    all-minilm).  Same registry `fastembed-rs` uses on the
    client → vectors interchangeable for cosine search.
  - `GET /api/index/embed-query?text=…&model=…` returns the
    embedding as a JSON `embedding: [f32; D]` payload.
    Server-side `asyncio.to_thread` so the synchronous
    fastembed call doesn't block the uvicorn event loop.
  - `GET /api/index/embed-models` lists the registry + flags
    whether fastembed is installed at all (503 when missing
    with a clear remediation hint).
  - Per-model `threading.Lock` so a burst of concurrent
    first-requests serialises the ONNX download instead of
    triggering N parallel `from_pretrained` calls.
  - `requirements.txt`: `fastembed >= 0.4.0`, `onnxruntime
    >= 1.18.0`.  Production: set
    `XDG_CACHE_HOME=<storage-root>/.fastembed_cache`
    in `<cb-api-env>` so the ~500MB bge-m3 weights land on
    the storage box, not the small root disk.
  - Rust: `CloudBackupClient::embed_query` /
    `embed_models`.  120s timeout on `embed_query` (first
    call to a never-loaded model needs the ONNX download).
  - Tauri: `sync_cb_embed_query`, `sync_cb_embed_models`.
  - CLI: `crispsorter sync cloud-backup {embed-query,embed-models}`.
  - **pytest**: +5 cases (vector returned, unknown model,
    503 when fastembed missing, auth required, embed-models
    list).
  - **mockito**: +4 cases (200/400/503 + embed-models list).
  - **Live**: `cb_sync_live_embed_query_round_trip`.

### Commit table (squashed in the close-out commit)

| Stage | Headline |
|---|---|
| A | `full_text` body sync + `/api/search` FTS5 endpoint + L1 store carries body text on pull |
| B | `LocalIndex::list_documents_for_push` + `list_chunks_with_embeddings` + CLI `push-embeddings` wired |
| C | `ManifestRow::from_raw_document` + bg_ingest auto-push hook (fire-and-forget initially) |
| D | End-to-end live test: index → push → fresh-DB pull → search body token → hit |
| E | `/api/files/by-hash/<sha>` upload + download (content-addressed blob store, pure-Rust streaming) |
| F | `SyncManager::drain_cb_outbox` + bg_ingest swap to outbox enqueue (durable retry) |
| G | `CB_API_SHARD_ROOT`-gated 256-way sharding by sha-prefix + `connect(sha=…)` + `fanout_query` |
| H | `api/embed.py` lazy fastembed registry + `GET /api/index/embed-query` + Rust client + CLI |

### Net delta

`tauri-app` test count: ~452 → ~490.
  sync (Rust): +17 mockito tests (Stages A: search ×3, E: file
        upload/download ×5, F: drain ×3, H: embed ×4, plus
        `from_raw_document` ×2) + 5 new env-gated live tests
        (full_text+search, end-to-end, byte round-trip,
        outbox drain, embed-query) → 13 live tests in
        `cb_sync_live_*` (8 cb-sync + the existing 5 WebDAV).
  index (Rust): +2 LocalIndex tempdir tests for push/embedding
        scans.

pytest (Python, `../cloud-backup/tests/`): 21 → 48.
  +8 for full_text + search (test_search_route.py)
  +9 for byte routes (test_file_routes.py)
  +5 for sharded mode (test_sharded_mode.py)
  +5 for embed (test_embed_route.py)

Build verification: `cargo check -p tauri-app --no-default-features`
+ `cargo check -p tauri-app --features crispasr` clean
throughout.  43+5=48 pytest green locally + against the
deployed VPS.

### What's NOT in this batch (tracked in PLAN.md → P13.7.x)

  - **`collection_id` sharding key** — sha-prefix breaks
    topical locality.  Add an optional `collection_id` field
    that clients set per logical group; router uses
    `collection_id[:2]` when set, falls back to `sha[:2]`.
  - **LanceDB-backed vector index on the VPS** — Python
    brute-force k-NN over SQLite BLOBs works ≤ 10k chunks.
    The right scale-out is LanceDB on the storage box
    (memory-mapped, columnar, larger-than-RAM).  Matches the
    P13.8 LanceDB-on-VPS work already on the plan.  FAISS was
    the wrong choice for CPU + 16GB RAM + TB scale (in-memory
    index doesn't fit); LanceDB's mmap'd columnar reads do.
  - **FTS5 vs Tantivy convergence** — `search_engine.py`
    already runs Tantivy over the existing-via-vps_worker
    7z-extract flow; cb-api's `/api/search` runs FTS5 over
    client-pushed `full_text`.  Two parallel engines today;
    convergence options laid out in PLAN.md.
  - **bg_ingest background drain timer** — drain is manual
    today (CLI / Tauri command).  Adding a 30-second tokio
    interval-task in `lib.rs` setup is a 20-line follow-up.
  - **GUI surface for download** — `sync_cb_download_file`
    Tauri command is wired; a "Download from cloud-backup"
    button on search results that calls it for a missing
    `crisp+local://…` path is the next UX iteration.

---

## Session log — 2026-05-13 — P13.7 Step 5 + 7 + 8 (cloud-backup HTTP sync, end-to-end)

The final P13.7 slice that gates the v0.1.41 tag.  Cross-repo: adds
a brand-new FastAPI module to the sibling `../cloud-backup` repo and
extends CrispSorter's P11 SyncManager to talk to it over HTTP.
Live-verified against the production VPS (deployed
`cb-api.service` alongside the existing `vps-worker.service`; the
two share `<catalog-volume>/cloudworker_state/<catalog-db>` via SQLite WAL).

### What landed

**`../cloud-backup` (new `api/` subpackage)**

  - `api/db.py` — idempotent schema migration that adds
    `chunk_embeddings` + `api_keys` tables and 8 nullable columns
    on `file_references` (`ext`, `parent_dir`, `filename`,
    `language`, `title`, `author`, `year`, `indexed_at`).  Runs in
    the FastAPI lifespan startup hook.  Reuses the existing
    `MasterDatabase._migrate_master_db` PRAGMA-guarded ALTER
    TABLE pattern — re-run on every cold start is a no-op.
  - `api/admin.py` — VPS-side key lifecycle CLI
    (`python -m api.admin mint <NAME> [--owner-id UUID]` / `revoke
    NAME` / `list`).  bcrypt-hashed values; raw key printed once
    at mint time and never retrievable afterward.  `list` never
    surfaces the hash either (audit-only metadata).
  - `api/app.py` — FastAPI with 4 authenticated routes plus a
    public health probe.  Auth = `Authorization: Bearer <key>`,
    constant-time bcrypt verify per request.  Default per-owner
    scoping (the calling key's `owner_id` rewrites the server-side
    `source_id` for pushes and filters the WHERE clause for
    pulls); `CRISP_CB_SHARED_OWNERS=1` env flips to a shared
    catalog where every key sees every row.
    | route | purpose |
    |---|---|
    | `GET  /api/health` | unauth probe (used by CrispSorter's status surface) |
    | `POST /api/manifest/push` | upsert rows into the existing `files` + `file_references` + `batches` + `sources` shape (same path `MasterDatabase.process_manifest` already uses for SYNC_MANIFESTS-over-rsync today) |
    | `GET  /api/manifest/pull?since=…&limit=…` | rows newer than `since` (epoch-ms watermark) |
    | `POST /api/index/push-embeddings` | store text/vector embeddings; per-row reject on empty/malformed packs |
    | `GET  /api/index/by-embedding?vec=…&k=…&model=…` | brute-force k-NN (Python-side scan; LanceDB on the VPS is a P13.8 follow-up) |
  - Watermark fix: a single batch was assigning the same
    `now_ms` to every row, which collapsed cursor-based
    pagination.  Fixed by `row_indexed_at = now_ms + idx` so
    `WHERE indexed_at > since` resumes mid-batch.
  - `deploy/etc/systemd/system/cb-api.service` runs
    `/opt/cb-api/venv/bin/python -m uvicorn api.app:app
    --host 127.0.0.1 --port 7869`.  Loopback-only (production
    exposure is the operator's call via the existing
    reverse proxy).  `Wants=vps-worker.service` rather than
    `Requires=` so an unrelated vps-worker failure doesn't take
    the HTTP API down.
  - the cb-api env-file template (`<cb-api-env>.example`) — mode-600 env-file
    template with `CB_API_DB_PATH` + `CRISP_CB_SHARED_OWNERS`.
  - `requirements.txt` gains `fastapi`, `uvicorn`, `bcrypt`,
    `httpx` (TestClient dep).
  - `tests/` directory with 21 pytest cases against the FastAPI
    surface using `TestClient` + a `monkeypatch`'d
    `CB_API_DB_PATH` tempfile.  Covers: bcrypt round-trip on the
    admin CLI, auth path on every protected route, manifest
    push→pull round-trip, since-watermark resume, pagination
    boundary, owner-scoping isolation between two keys, the
    `CRISP_CB_SHARED_OWNERS` env flag, the optional
    filter-metadata columns (`language`/`title`/`author`/`year`),
    embeddings k-NN ranking, dim-mismatch silent-skip,
    per-row reject for empty vectors, model-id filter on the
    by-embedding query.  All 21 green locally; ~15 s run time.

**CrispSorter Rust**

  - `src-tauri/src/sync/cloud_backup.rs` — async `CloudBackupClient`
    over reqwest.  Mirrors the FastAPI wire shape 1:1: `ManifestRow`,
    `PullRow`, `EmbeddingRow`, `ByEmbeddingHit`, `HealthResponse`.
    `manifest_push` / `manifest_pull` / `embeddings_push` /
    `by_embedding` / `health` methods.  17 mockito-backed unit tests
    cover 200/cursor/no-cursor/400/401/500/503 for every push/pull
    path + 3 env-gated live tests (`#[ignore]`'d) that exercise the
    full round-trip against a real VPS.
  - `src-tauri/src/sync/secret.rs` — OS-keychain bearer-token
    storage.  Same pattern as
    `src-tauri/src/images/crisplens/secret.rs` (per-URL keying,
    `keyring` mock for tests) but under the distinct
    `CrispSorter.CloudBackup` service so the rows stay
    distinguishable in Keychain Access.
  - `src-tauri/src/sync/tauri_commands.rs` — new commands:
    `sync_cb_status`, `sync_cb_set_token`, `sync_cb_clear_token`,
    `sync_cb_manifest_push`, `sync_cb_manifest_pull`,
    `sync_cb_embeddings_push`.  Sync watermarks live in the same
    `sync_state` KV table the existing P11 surface uses, keyed
    `cb_last_manifest_push_ts` / `cb_last_manifest_pull_ts` /
    `cb_last_embeddings_push_ts` so the UI panel can show "pulled
    2 min ago" alongside the crisp-index-server state.
  - 4 new `IndexConfig` fields: `cloud_backup_url`,
    `cloud_backup_push_manifests_enabled`,
    `cloud_backup_push_embeddings_enabled`,
    `cloud_backup_pull_manifests_enabled`.  All serde-default to
    off / `None`; bearer token never persisted to JSON.
  - Settings → Cloud-backup sync panel (Svelte): URL field,
    write-only API-key input with a Save button that pushes the
    value straight to `sync_cb_set_token` (keychain), 3 toggles,
    inline status hint with the server version + watermark
    summary.  EN + DE i18n strings.
  - CLI subcommands: `crispsorter sync cloud-backup
    {status,push-manifest,pull,login,logout}`.  Token resolves
    from `--token` (login only) → `CB_SYNC_API_KEY` env → keychain.
    `push-embeddings` is a deliberate
    `not-yet-implemented-from-CLI` stub (server route + Tauri
    command live; the missing piece is a `LocalIndex::list_chunks_with_embeddings`
    helper that's a follow-up).
  - 3 CLI gap-fill subcommands (the P13.7 audit items):
    `crispsorter index promote-l3 <doc-id>` (auto-routes by ext
    via `extract_text_from_path` → `pipeline.reingest_document`);
    `crispsorter images crisplens push <path> --visibility shared|private`
    (same two-phase by-hash dedup + multipart POST as the GUI's
    `images_crisplens_image_push`); `crispsorter images crisplens person <id>`
    (lists every image in a person cluster via `/api/people/{id}`).
  - `mockito = "1.5"` added to `[dev-dependencies]` — first
    introduction of the mock-HTTP-server convention in this repo.
  - `reqwest::query()` isn't on our feature set; hand-rolled query
    strings used instead (also keeps log output readable).

**Live verification against the production VPS**

  - cb-api.service deployed on the production VPS (loopback port 7869);
    SSH tunnel for the cargo run.
  - All 3 env-gated live tests pass:
    `cb_sync_live_health_round_trip`,
    `cb_sync_live_manifest_push_pull_round_trip`,
    `cb_sync_live_embedding_push_rejects_empty`.
  - Per-owner scoping confirmed end-to-end: a freshly minted key
    sees only its own row out of the 2051-row `file_references`
    table (the other 2050 rows belong to existing vps_worker
    source_ids).  Schema migration added the new columns without
    touching the pre-existing rows.

### Commit table

| Commit | Headline |
|---|---|
| `<cb-1>` | cloud-backup: `api/` subpackage (db.py + admin.py + app.py) + 21-test pytest suite + cb-api.service + cb-api.env + deploy/README.md instructions + requirements.txt deps |
| `<cs-1>` | CrispSorter: `sync/cloud_backup.rs` + `sync/secret.rs` + 17 mockito tests + 3 env-gated live tests + new sync_cb_* Tauri commands + 4 new IndexConfig fields |
| `<cs-2>` | CrispSorter: Settings → Cloud-backup sync panel (URL + write-only key + 3 toggles + status hint) with EN/DE i18n |
| `<cs-3>` | CrispSorter: `crispsorter sync cloud-backup {status,push-manifest,pull,login,logout}` CLI subcommands |
| `<cs-4>` | CrispSorter: 3 CLI gap-fills — `index promote-l3` + `images crisplens push` + `images crisplens person` |
| `<cs-5>` | docs(plan,history,readme): P13.7 Step 5+7+8 closeout + v0.1.41 tag |

(Commit hashes filled in at squash time.)

### Net delta

`tauri-app` test count grew from ~415 → ~452:
  sync   (+17 mockito wire tests + 2 secret-keychain tests + 3
        env-gated live tests for cb_sync — `#[ignore]`'d so
        default `cargo test` stays offline)
  pytest (21 new tests in `../cloud-backup/tests/`)

Build verification: `cargo check -p tauri-app --no-default-features`
+ `cargo check -p tauri-app --features crispasr` clean throughout.
`npm run check` 0 errors.  21 pytest cases green.  3 live tests
green against the deployed VPS.

### What's NOT in this session

  - **LocalIndex::list_chunks_with_embeddings helper** — the
    server-side push-embeddings route is live + the Tauri
    command + 4 mockito tests + the live "empty payload rejects"
    test all pass, but the CLI's `push-embeddings` subcommand
    prints a "not yet exposed" note until the LanceDB row-scan
    helper lands.  The GUI / scripted path
    (`sync_cb_embeddings_push` with a hand-built `EmbeddingRow`
    list) works today.
  - **bg_ingest auto-push hook** — `IndexConfig.cloud_backup_push_manifests_enabled`
    exists; bg_ingest doesn't fan-out to cloud-backup yet on
    every indexed row.  Today the push is operator-triggered
    (CLI subcommand or sync_cb_manifest_push from scripts).
    Wiring the hook is mechanical (mirrors the existing
    CrispLens-image-push fan-out in bg_ingest) but lands as a
    follow-up to keep this slice focused on the protocol.
  - **L3 byte transfer over HTTP** — out of scope per PLAN.md.
    Stays SSH + `retrieve.py` for now.
  - **Server-side embedding computation** — server only stores
    what the client pushes.  No GPU on the VPS today.
  - **Image embedding sync** — CrispLens covers face embeddings;
    general image embeddings via CrispEmbed are a P13.6
    follow-up tracked separately.

---

## Session log — 2026-05-13 — P13.6 Multimodal UX + L1/L2/L3 audio + P13.7 Steps 1+2+3+4+6 (image L1/L2/L3, CLI search, CrispLens push)

Two interlocking verticals shipped end-to-end:

**P13.6** closed the audio-side UX gap from the 2026-05-12 P13.5
ingest vertical — users can now drop audio/video files into both
Stapel and Kataloge, watch a "Transcribing" status badge while
whisper runs, see the detected source language + duration in the
batch table, configure ASR backend / LID method / L1-L2-L3
ingest depth in a new Settings → Multimodal panel, and one-click
promote any L1/L2 audio row to L3 from search results.  Audio
L2 metadata (duration / codec / sample rate / channels /
bitrate_kbps) now lives in dedicated LanceDB columns via schema
migration v101.

**P13.7** ported the same shape onto images and added a search-
CLI matching cloud-backup's filter set:

  - Image L1/L2/L3 enum + master-switch toggle + "Re-OCR"
    search-result action (parallel to audio Steps 7c+8).
  - Image L2 (EXIF) metadata in 5 dedicated LanceDB columns via
    schema migration v102 (camera_make/model/lens/taken_at_unix/
    iso) — populated from the kamadak-exif reader inside the
    OCR dispatch arm.
  - `crispsorter index search` CLI subcommand with the
    cloud-backup `search.py` filter set: `--ext`/`--hash`/
    `--folder-prefix`/`--owner`/`--lang`/`--translated-to`/
    `--year-{min,max}`/`--min-size`/`--max-size`/`--after`/
    `--before`/`--audio-duration-{min,max}`/`--image-camera-{make,model}`/
    `--limit` + `-f json|text` (the global flag).
  - CrispLens image push (`images_crisplens_image_push`) — POST
    multipart to `/api/ingest/upload-local` with a `by-hash`
    dedup precheck.  Privacy-aware: opt-in via the Settings
    Multimodal panel.

| Commit | Headline |
|---|---|
| `191edd3` | P13.6 Step 1 — "Transcribing" status label in Stapel + Kataloge for audio/video extensions.  EN/DE i18n. |
| `a10cca9` | P13.6 Step 2 — detected-language column in Stapel.  audio_extract_text now returns {text, language}; BatchItem grows detectedLanguage; new (default-hidden) Lang column with sort comparator. |
| `bc1c77b` | P13.6 Step 3a — `audio_metadata` Tauri command + `audio/probe.rs` symphonia format-reader probe (sub-millisecond, no decode pass).  Returns AudioMetadata { duration_seconds, codec, sample_rate_hz, channels, bitrate_kbps }. |
| `6607df6` | P13.6 Step 3b — BatchReview row pre-fill + default-hidden Duration column (MM:SS / H:MM:SS) + hover tooltip `mp3 stereo 44.1 kHz @ 192 kbps`. |
| `435af5a` | P13.6 Step 3c-prep — ExtractedDocument.audio field; extract() runs probe before decode; mechanical sweep adds `audio: None` to the 7 non-audio ExtractedDocument literals. |
| `e449bd0` | P13.6 Step 4 — empty-state strings ("Drop documents, images, audio or video files…") + IndexIngest drop-area hint widened to mention audio/video extension families. |
| `9b48038` | P13.6 Steps 5+6 — Multimodal Settings panel: master switch + ASR backend dropdown (whisper / whisper-large-v3 / whisper-small / whisper-medium / parakeet / qwen3-omni) + LID method dropdown.  Three new IndexConfig fields with serde defaults; bg_ingest reads them via the OnceLock override pattern (restart-on-change). |
| `c6ca5e1` | P13.6 Steps 7a+7b — Schema migration **v101** (`AddAudioMetadataColumns`): 5 new LanceDB columns (audio_duration_seconds Float64, audio_codec Utf8, audio_sample_rate_hz Int32, audio_channels Int32, audio_bitrate_kbps Int32).  RawDocument + DocumentChunk gain matching fields; 10 call sites updated; chunks_to_record_batch builds the new Arrow arrays.  +2 v101 tests, +1 updated idempotency test (now expects vec![100, 101]). |
| `a6195c1` | P13.6 Step 7c — IngestAudioLevel { L1, L2, L3 } enum + dispatcher 4-way ladder (skip / probe-only / EXIF-only / full).  ExtractOptions plumbed as a String ("l1"/"l2"/"l3").  Settings UI dropdown. |
| `e7905c5` | P13.6 Step 8 — `index_audio_promote_l3(location_uri)` Tauri command + Transcribe search-result action in IndexSearch.svelte.  Re-ingests through the standard pipeline, overrides Settings (per-row click is unambiguous intent). |
| `8206afb` | P13.5 follow-up (drop-zone gate) — JS-side AUDIO_EXTENSIONS + MULTIMODAL_EXTENSIONS constants; new `audio_extract_text` Tauri command; BatchReview + IndexIngest accept all 22 audio/video extensions on drop. |
| `851de8c` | P13.6 Step 9 — Schema migration **v102** (`AddImageMetadataColumns`): 5 new LanceDB columns (image_camera_make/model/lens_model Utf8, image_taken_at_unix Int64, image_iso Int32).  ExtractedDocument.image_exif: Option<ExifSummary>; OCR dispatch arm reads EXIF once per image regardless of which tier fires; bg_ingest copies the curated subset.  +2 v102 tests. |
| `1505135` | P13.7 Steps 1+2+3 — IngestImageLevel enum + image master switch + `index_image_promote_l3` Tauri command + Re-OCR frontend action. Mirror of P13.6's audio Steps 7c+8.  OCR_IMAGE_EXTS dispatch arm restructured into a 4-way ladder; L2 path produces an empty-text doc with image_exif populated so bg_ingest still writes the image_* columns. |
| `c8d3074` | P13.7 Step 6 — `crispsorter index search` CLI with cloud-backup-parity filter set.  Seven new SearchFilters fields (ext/source_hash_prefix/parent_dir_prefix/audio_duration_{min,max}_seconds/image_camera_{make,model}) + `to_lance_sql` push-down + new `fetch_search_results_by_ids_filtered` LocalIndex method.  Size + date filters parsed by hand-rolled helpers (no `chrono` / `time` direct-dep growth).  19 new tests. |
| `5cab459` | P13.7 Step 4 — CrispLens image push: `images_crisplens_image_push(path, visibility?)` Tauri command.  Two-phase by-hash dedup → multipart POST to `/api/ingest/upload-local`.  IndexConfig.crisplens_image_enrichment_enabled (default false; privacy-aware).  reqwest gains the `multipart` feature. |

### Architectural pieces in place after this batch

- **Audio + image symmetry** end-to-end: both have an L1/L2/L3
  Settings dropdown, both have a Tauri promote command, both
  ship a search-result UX button for L1/L2 → L3, both populate
  dedicated L2 LanceDB columns via versioned schema migrations,
  both flow through bg_ingest with the same master-switch /
  ingest-level pattern.
- **CLI search parity** with cloud-backup's `search.py`.  Same
  filter syntax (`--min-size 100MB`, `--after 2024-01-01`,
  `--hash a1b2c3`) so users moving between the two tools don't
  re-learn the surface.  CrispSorter-specific additions
  (`--audio-duration-{min,max}`, `--image-camera-{make,model}`,
  `--translated-to`, `--lang`) extend rather than replace.
- **CrispLens client-side push** unblocked.  `image_push` joins
  the existing `by-hash` / `people` / `faces` / `search` /
  `watchfolders` Tier 2 endpoints to make CrispSorter a
  bidirectional client.
- **Schema migration framework** has 3 real consumers now
  (v100, v101, v102) following the same shape.  Adding a
  follow-up migration is mechanical.

### Net delta

+34 unit tests across `tauri-app`:
  cli   (4 new — `parse_human_size_*`, `parse_iso_date_to_unix_*`,
        `format_size_human_known_thresholds`, plus the existing
        is_multilingual_whisper_backend test).
  schema (7 new `filters_sql_*` tests covering the new ext /
        hash / parent-dir / audio-duration / image-camera SQL
        builders).
  migrations (4 new — v101 + v102 missing-handle guards +
        version+name pins; idempotency test updated).
  audio L2 + image L2 are end-to-end exercised by the schema
  migration tests and the existing OCR / audio extract tests.

Build verification: `cargo check --no-default-features` and
`cargo check --features crispasr` clean throughout each commit;
full lib test suite green at commit boundaries.

### Cloud-sync work — explicitly NOT in this session

- Step 5 (cloud-backup FastAPI cross-repo) deferred to a fresh
  session because (a) it touches the sibling cloud-backup
  repo's deployment surface and (b) it needs a SQLite migration
  on the cloud-backup side.  Full design — endpoint shapes,
  auth, sync watermark semantics — lives in PLAN.md under
  "P13.7 Step 5".  This batch lands every prerequisite
  (multipart reqwest, by-hash dedup helper, SearchFilters
  scalar SQL coverage) so the cross-repo work can focus on the
  protocol.
- Step 7 (live tests for the sync routes) deferred with Step 5;
  the existing P11 mockito patterns + `WEBDAV_TEST_URL`-style
  env-gated live tests are the model.

---

## Session log — 2026-05-13 — P13.5 follow-ups batch 2 (4 more shipped + CrispEmbed routing unification)

Continuation of the same-day P13.5 follow-ups session.  Picks
off the last four user-quoted follow-ups: Audio-LID auto-
resolution, routing `index/reranker.rs` through the
`CrispEmbedBackend` wrapper, surfacing `crispembed::list_models()`
in the Settings panel, and FTS-over-translated body.

| Commit | Headline |
|---|---|
| `2b80345` | Audio-LID auto-resolution for whisper-family backends — `--lid-method whisper` no longer demands `--lid-model PATH`.  `is_multilingual_whisper_backend` heuristic + `resolve_whisper_lid_model_path` async helper: Path 1 reuses the user's explicit `--model PATH` when whisper-family (whisper / -base / -small / -medium / -large-v3); Path 2 falls back to `registry_lookup("whisper")` + `cache_ensure_file` for whisper-base download.  Silero / Ecapa / Firered still need explicit paths (no CrispASR registry entries).  3-way `lid_options` match in `cmd_chat_transcribe`; tokio runtime construction moved up so `block_on` is available before lid_options is built.  +1 test pinning the allow-list (excludes distil-whisper / parakeet / qwen3). |
| `ebd511f` | `index/reranker.rs` routed through `CrispEmbedBackend` — promoted `load`/`is_reranker`/`rerank` on the wrapper from private+`#[allow(dead_code)]` to `pub(crate)`; reranker's struct field `crispembed::CrispEmbed → CrispEmbedBackend`; `crispembed::CrispEmbed::new` direct import gone from outside `index::embedder`.  Removes a duplicated FFI entry point and inherits the wrapper's UTF-8-path check + future `set_prefix` / `set_dim` knobs.  No public-API or behaviour change; existing reranker tests stay green. |
| `b0ebc23` | `crispembed::list_models()` registry helper — new `embedder_registry_list` Tauri command returns 43-entry registry (name / desc / filename / size); Settings has a `<details>` disclosure beneath the engine toggle listing the bundled registry with max-height scroll.  Informational only: existing dropdown still keys off the `EmbedderModel` enum.  +EN/DE i18n keys.  Wiring registry-driven selection (so non-enum entries are pickable) is tracked separately. |
| `be73321` | FTS-over-translated body — Tantivy schema gains `body_translated` field (same tokenizer as `body`, no STORED).  `IndexFields.body_translated: Option<Field>` plus `bind_fields_from_disk` makes legacy on-disk indexes open as `None` (graceful degrade — old indexes keep working without the new field).  `build_term_query` OR's a boost-0.7 disjunct on `body_translated` when present, so English query against a Bosnian doc with English translation now hits BM25 too (closes the cross-language FTS black hole; multilingual embeddings were carrying the whole load).  Ingest plumbs `RawDocument.translated_text → TantivyInputOwned.body_translated → TantivyInput.body_translated`.  `.cidx` mount path picks up `text_translated` from LanceDB when re-deriving the FTS.  +2 tests: cross-language hit + legacy-schema graceful degrade. |

### Architectural pieces in place after this batch

- **CrispEmbed wrapper is the sole FFI entry point** for the
  GGUF backend — both `index::embedder` and `index::reranker`
  go through `CrispEmbedBackend`.  Adding a new knob (cache_dir,
  Matryoshka, prefix) lifts both consumers in one move.
- **Cross-language FTS** works end-to-end on freshly-created
  indexes: a Bosnian doc with English MT-pass output is
  reachable by both an English `"hello"` query (translated-body
  channel) and a Bosnian original query (body channel).
  Multilingual embeddings still cover the vector channel.
- **Registry transparency** in Settings: users see the full
  CrispEmbed registry beneath the engine toggle, so a new
  upstream model becoming available is at least visible without
  a CrispSorter release.

### Net delta

+4 unit tests across `tauri-app`:
  cli (`is_multilingual_whisper_backend_covers_known_variants`),
  index::fts_index (`body_translated_makes_translated_text_searchable`,
  `bind_fields_from_disk_handles_legacy_schema`).
The +1 in `cli` is from `2b80345`; the +2 in `index::fts_index`
are from `be73321`; `ebd511f` and `b0ebc23` are zero-new-test
refactors covered by existing reranker / capabilities tests.

Build verification: `cargo check --no-default-features` and
`cargo check --features crispembed` clean throughout; full lib
test suite (426 tests, 2 ignored = WebDAV-live integration)
green after the FTS slice.

### What's deferred (queued in PLAN.md)

- ColBERT multi-vector retrieval — needs a per-token vector
  column / separate `chunk_multivec` table + a MaxSim scorer.
- Omnimodal BidirLM-Omni cross-modal search — new model class +
  image-patch preprocessing + per-index dim selection.
- Registry-driven embedder selection — the Tauri command +
  Settings panel are now in place; wiring the dropdown to pick
  registry entries (parallel String-keyed config path or
  EmbedderModel → String refactor) is the next step.
- FTS body_translated migration on legacy indexes — graceful
  degrade is in place; a proper "rebuild FTS from LanceDB"
  migration is needed for shipped indexes to opt in without
  re-ingesting from disk.
- Non-whisper audio-LID auto-resolution — whisper-method case is
  resolved; Silero / Ecapa / Firered need upstream registry
  entries.

---

## Session log — 2026-05-13 — P13.5 follow-ups (5-of-8 shipped)

Continuation of the 2026-05-12 P13.5 vertical.  Picks off five
follow-ups that close the user-visible loop on cross-language
search end-to-end, leaving the more involved schema / Tantivy
work for a later slice.

| Commit | Headline |
|---|---|
| `a60ac30` | `--stream` flag for `chat transcribe` — `AsrHandle::transcribe_streaming<F: FnMut(&str) + Send>` drives `crispasr::Session::stream_open` / `Stream::feed` / `get_text` / `flush`.  Whisper-only at the C-ABI level; partials emit to stderr (stdout stays clean for the final result), final transcript routes through the existing `--output` / `-f` path.  Coexists with `--translate-to` (runs MT after the stream finishes) and warns when combined with `--policy != as-configured` (LID routing needs the full PCM). |
| `2f8400b` | LID model auto-resolution — `extractors::text_lid::LidPreset` enum (Cld3 / Glotlid / Fasttext176) + `resolve_lid_model(preset, cache_dir)` async helper wrapping `crispasr::registry_lookup` + `cache_ensure_file`.  `translate_text` Tauri command's `lid_model` field is now optional; when omitted AND `source_lang` is omitted, auto-resolves CLD3.  MT auto-resolution was already in place via the existing `Asr::load` registry path. |
| `c4f7ffb` | Search-side query rewrite — `SearchFilters.prefer_translated_lang: Option<String>` adds `text_translated_lang = X AND text_translated IS NOT NULL` to the LanceDB scalar filter; `SearchEngine::apply_translation_snippet` swaps `snippet` from the original-text-derived preview to a `text_translated`-derived one (same 400-char cap) for matching rows.  Called BEFORE `maybe_rerank` in all three search paths so the reranker scores against the user-facing snippet.  FTS-over-translated-body deferred (needs Tantivy schema migration). |
| `b13745f` | `IndexConfig.translate_to` + persistence + Settings UI — new `IndexConfig` field, `<data_dir>/index_config.json` JSON ledger mirroring `crisplens.settings`, loaded in the Tauri setup hook (`blocking_lock` on the tokio Mutex; runtime isn't driving any tasks at that point), persisted in `index_set_config` after the in-memory state update (best-effort, non-fatal on I/O failure).  `bg_ingest` reads `translate_to` and AUTO-RESOLVES CLD3 LID via task #2's helper when translate is on — because the extractor's MT hook is a no-op without a known source language.  Svelte Settings dropdown with 7 commonly-used targets (en / de / fr / es / it / ja / zh) — same set as the frontend filter. |
| `9be60b8` | Frontend `translate_text` integration — `SearchResult` interface gains `text_translated` / `text_translated_lang` (frontend-side fast path skipping the Tauri round trip when the row already carries the matching translation from the index-time batch path); per-result `Map<key, TranslateState>` lifecycle (idle / loading / finished / error) tracked under `${doc_id}:${chunk_index}` keys; filter-row dropdown for target language (same 7-item set as the IndexConfig UI); inline rendering BELOW the snippet with cached / backend badges.  `clearTranslations()` hook in `runSearch` so stale "Translated en" badges don't outlive a query change.  Hidden when `r.language === translateTargetLang` (no point offering "Translate to en" on an English hit).  i18n keys (`translate_to`, `translate_to_none`, `translate_to_hint`) added to both EN and DE locales for `svelte-check` cleanliness. |

### Architectural pieces in place after this batch

- **One-click on-demand translation** from any search hit:
  the SvelteKit UI calls `invoke('translate_text', ...)` per
  click; the backend's SQLite `translation_cache` table
  (cb3150d) makes repeated clicks on the same chunk free; the
  m2m100 handle is process-cached via `OnceLock<AsrHandle>` so
  only the first click pays the model load.
- **Index-time batch translation** flipped on entirely from
  Settings → Search Index → "Index-time translation: English"
  — `index_set_config` persists the choice to
  `<data_dir>/index_config.json`, the next `bg_ingest` pass
  reads it + auto-resolves a CLD3 LID model, and every freshly
  ingested document lands with a populated `text_translated`
  column.
- **Search-time view of translations**: result rows surface
  `text_translated` as the snippet when the caller sets
  `SearchFilters.prefer_translated_lang`; the frontend's
  per-result `text_translated_lang` field gives the
  "Translate to en" button a no-network-round-trip fast path
  for index-time-translated rows.
- **Streaming transcription** for long files: `--stream`
  produces incremental partials on stderr as Whisper commits
  each rolling window (step=3000ms / length=10000ms /
  keep=200ms — the reference shape).

### Net delta

+16 unit tests across `tauri-app`:
  cli (`chat_transcribe_stream_flag_parses`),
  extractors::text_lid (3 LidPreset round-trip / drift-guard /
  canonical-string tests),
  index::schema (3 SearchFilters SQL builder tests covering
  the new `prefer_translated_lang` predicate + escape +
  combined-with-source-language case),
  index::search (5 `apply_translation_snippet` tests),
  index::config_persist (4 round-trip / corrupt-file /
  missing-file / create-dir tests).

Build verification: `cargo check --no-default-features` and
`cargo check --features crispasr` clean; `npx svelte-check`
0 errors (down from 3 introduced by the new i18n key
references, closed by the locale entries).

### What's deferred (queued in PLAN.md)

- SRT / VTT output formats for `chat transcribe` — needs a
  segments-returning ASR API.  Current wrapper concatenates
  to `String`.
- Per-language reranker selection — `language` column is
  populated; routing the reranker model by it is the next
  slice.
- Per-chunk vs per-doc translation storage optimisation —
  today replicates per chunk (matching `full_text_md`
  convention); a "L0-only" alternative would JOIN at search
  time.  Needs migration on shipped data.
- Audio-LID auto-resolution — text-LID side done; audio
  needs upstream registry entries OR a "reuse the loaded ASR
  ggml for Whisper-method LID" path.
- FTS-over-translated body — Tantivy schema migration to
  add a `body_translated` field, then wire
  `SearchFilters::prefer_translated_lang` into the FTS query.
  Multilingual embeddings already handle the vector channel.

---

## Session log — 2026-05-12 — P13.5 Audio + Translation vertical (Phases 0–8, all shipped)

End-to-end audio + cross-language search vertical across two repos.
Three architectural deliverables in one session:

1. **Audio + video transcription pipeline.**  Pure-Rust symphonia
   decode + ffmpeg shell-out fallback → 16 kHz mono Float32 PCM →
   CrispASR `Session::transcribe_with_language`.  Handles 22
   extensions: WAV / MP3 / M4A / FLAC / OGG / OPUS / AAC for audio;
   MP4 / MOV / MKV / WebM / M4V for video (audio-stream demux only,
   no video decode); AVI / WMV / FLV / TS / AMR / RA / 3GP / ASF via
   ffmpeg on PATH.  Two consumer surfaces: a new CLI (`chat
   transcribe`, `chat tts`) and an index-time `extractors/audio.rs`
   so audio files become first-class searchable documents alongside
   PDFs and OCR'd images.

2. **Language-aware backend routing.**  `asr/lang.rs` carries a
   curated capability table mirroring CrispASR's README feature
   matrix — 24 ASR backends each tagged with their supported language
   set, native-LID flag, translation pathway, and speed tier.
   `BackendFallback` policy enum (`AsConfigured` / `Strict` / `Auto` /
   `Translate`) feeds a pure `route(policy, current, detected)`
   decision function.  `asr/orchestrator.rs` runs LID over the first
   10 s of PCM, routes per policy, applies the decision against the
   primary or a freshly-constructed sibling `AsrHandle`.  PLAN's life
   example ("DE audio with English-only parakeet → switch to whisper")
   is a one-line CLI invocation.

3. **Cross-document translation, both surfaces.**  On-demand:
   `translate_text` Tauri command, SQLite-backed `translation_cache`
   table keyed by `(SHA-256(text), source_lang, target_lang, backend)`
   so repeated UI clicks on the same chunk are free.  Index-time
   batch: new `text_translated` + `text_translated_lang` LanceDB
   columns populated by an extractor-side MT pass when
   `ExtractOptions::translate_to` is set; columns added to existing
   tables by the **first real consumer of the schema-migration
   framework** (`AddTextTranslatedColumns` at v100).

### Commits — CrispSorter (chronological, all on `main`)

| Commit | Phase / Layer | Headline |
|---|---|---|
| `b9c9153` | Phase 0 | `AsrModel` enum → `AsrConfig { backend, model_path }`; PLAN consolidation of axes 1–3 (backends / media / language handling) |
| `a8821f9` | Phase 1 | `src-tauri/src/audio/` — symphonia decoder + ffmpeg fallback + linear resample + hound writer; 22 tests; `audio_decode_demo` smoke example (verified against `/System/Library/Sounds/Glass.aiff` at 83× realtime) |
| `a800606` | chore | `.gitignore .cargo/` for the per-repo `target-dir` redirect to `<external-volume>/code/cargo-target/CrispSorter` — tauri-cli spawns cargo as a child process which bypasses the interactive `~/.zshenv cargo()` function wrapper, but `.cargo/config.toml` survives that boundary |
| `d202f23` | Phase 2 | `asr/lang.rs` — `Language` newtype, `LidMethod`, `BackendCapabilities` table for all 24 ASR backends, `BackendFallback` policy + pure `route()` decision; 26 tests covering routing decisions × policies + parakeet EU-25 / distil-whisper EN-only / `Many` / `PerModel` shapes |
| `52ca2fa` | Phase 3 (slice A) | `chat transcribe` + `chat tts` CLI subcommands; `Asr::synthesize` / `set_voice` / `set_speaker_name` / `speakers` + `AsrHandle::synthesize_with_options` (atomic apply-then-synth); `hound` promoted to non-optional so `audio::writer` always-compiles; 7 clap-parse + helper tests |
| `1d83476` | Phase 4 (slice B) | `extractors/audio.rs` index-time transcription, `AUDIO_EXTS` dispatch arm slotted before the text catch-all, process-level `OnceLock<AsrHandle>` so batch ingest of many audio files loads the model once; 7 tests |
| `87b631e` | Phase 5 | Transcript translation post-processing: `Asr::translate_text` + `AsrHandle::translate_text` consuming the upstream `Session::translate_text` (CrispASR `cfe6770a`); `--translate-to` / `--translate-backend` / `--translate-model` / `--translate-max-tokens` CLI flags |
| `18ff3b2` | Phase 6 | Audio-LID routing applied: `asr/orchestrator.rs` with `transcribe_with_lid_routing(pcm, primary, policy, lid, hint)`; `--policy as-configured\|strict\|auto`, `--fallback BACKEND`, `--lid-model PATH`, `--lid-method whisper\|silero` CLI flags |
| `3adb326` | docs | PLAN.md Phase 8 — cross-document translation entry |
| `70de0df` | docs | PLAN.md — mark Phase 8 upstream FFI as landed |
| `9db27e1` | docs | PLAN.md — mark Phase 7 upstream text-LID FFI as landed |
| `ff2a8cb` | infra | Versioned schema-migration framework (`crate::migrations`): `Migration` async trait, `MigrationContext { lance, sqlite, data_dir }`, `MigrationRunner` with gap/duplicate detection + downgrade guard + idempotent reruns; 8 tests against in-memory SQLite ledger |
| `ceca1eb` | Phase 7 (consumer a) | Text-LID safe wrapper consumer: `extractors::text_lid::detect_language` over the upstream `crispasr::text_detect_language` + ISO 639-3→1 normaliser (37-entry curated table covering parakeet EU-25 + granite-6 + commonly-encountered others); 10 tests |
| `cb3150d` | Phase 8 (on-demand) | `translate_text` Tauri command + SQLite cache (`translation_cache` table in `crisp_jobs.db` via the new `JobQueue::conn_arc()` accessor); 7 cache tests |
| `c4eb28f` | Phase 7 (consumer b) | Extractor language plumbing: `ExtractedDocument.language: Option<String>` + `ExtractOptions.text_lid_model: Option<PathBuf>` + post-dispatch LID hook (2 KB-capped, 20-char floor, non-fatal); 8 extractor initialisers + bg_ingest fallback chain `item.language.or(extracted.language)` |
| `3a25024` | Phase 8 (batch a) | Extractor translation plumbing: `ExtractedDocument.translated_text` + `translated_to_lang` + `ExtractOptions.translate_to` / `translate_backend` / `translate_model` + post-LID translation hook with process-level `OnceLock<AsrHandle>` for the MT backend |
| `2734199` | Phase 8 (batch b) | LanceDB schema + migration + pipeline: `text_translated` + `text_translated_lang` columns in `build_schema`; `RawDocument` + `DocumentChunk` + `SearchResult` field plumbing across 12 initialiser sites; `chunks_to_record_batch` writes 2 new StringArrays; `record_batches_to_search_results` + `search_sparse_in_pool` read with null-tolerance for pre-v100 rows; `index/migrations.rs` with `AddTextTranslatedColumns` (v100), 2 tests including end-to-end against a real LanceDB tempdir; `MigrationRunner` wired into `init_index` between `LocalIndex::open_or_create` and `IngestPipeline::new`, ledger at `<data_dir>/.crispsorter_migrations.db` |

### Commits — CrispASR sibling repo

| Commit | Headline |
|---|---|
| `cfe6770a` | `feat(translate): expose text-to-text translation through Rust` — adds `crispasr_session_translate_text` + `crispasr_session_translate_text_free` C-ABI exports (free-fn mirrors the punc-side pattern so safe-Rust doesn't drag in libc), `extern "C"` in `crispasr-sys`, safe `Session::translate_text(text, src, tgt, max_tokens) -> Result<String, String>` in the high-level crate.  C-ABI bumped to **0.5.1**. |
| `ee5e7cd8` | `feat(text-lid): expose text-language-detection through Rust` — adds module-level `crispasr_text_detect_language(text, model_path, n_threads, out_label_buf, out_label_cap, out_conf*) -> int` wrapping the internal `text_lid_dispatch` façade (CLD3 + GlotLID-V3 fastText + LID-176 fastText, routed by GGUF `general.architecture`).  Return-code contract mirrors the audio-side `crispasr_detect_language_pcm` (0 / -1 / 1 / 2).  Safe wrapper exposes `crispasr::text_detect_language(text, model_path, n_threads) -> Result<TextLidResult, String>`; label format is preserved as-is (CLD3's `zh-Latn`, fastText's `eng_Latn`) — normalisation is the consumer's choice.  C-ABI bumped to **0.5.2**. |

### Architectural deliverables in place

- **24 ASR backends** addressable by string: `whisper` (99 langs, default for GUI back-compat), `parakeet` (25 EU langs, FastConformer-TDT, fast batch), `distil-whisper` (EN-only, 6.3× faster than whisper), `canary` (25 EU explicit `-sl/-tl`), `cohere`, `granite{,-4.1,-4.1-plus,-4.1-nar}`, `voxtral{,4b}` (13 langs, realtime), `qwen3` (30 + 22 Chinese dialects, native LID), `wav2vec2` (per-model), `glm-asr` (17 langs, native LID), `kyutai-stt` (EN/FR), `firered-asr` (Mandarin + 20+ dialects), `moonshine{,-streaming}` (realtime), `gemma4-e2b` (140+ langs, native LID, dual ASR+MT), `omniasr{,-llm,-llm-unlimited}` (1600+ langs), `vibevoice` (50+), `mimo-asr`, `fastconformer-ctc`.
- **5 TTS backends**: `kokoro`, `qwen3-tts`, `vibevoice-tts`, `orpheus` (preset speakers via `set_speaker_name`), `chatterbox`.  All emit 24 kHz mono Float32 PCM; CLI writes via `audio::writer::write_wav_mono`.
- **4 translation backends**: `m2m100` (100 langs any-to-any, default), `m2m100-wmt21` (EN↔{zh,de,fr,ja,ru,is,ha} direction-specific), `madlad` (419 langs via target-language prefix tag), `gemma4-e2b` (dual ASR+MT, 140+ langs).
- **4 LID model options**: Whisper encoder (99 langs, reuses any ggml-*.bin), Silero (95 langs, 16 MB GGUF), Ecapa (107 VoxLingua or 45 CommonLanguage, 40 MB), Firered (120 langs incl. Chinese dialects, 544 MB).  Whisper + Silero work via the module-level `detect_language_pcm`; Ecapa + Firered are session-level.
- **Schema-migration framework**: `Migration` trait + `MigrationRunner` with gap detection (no v3 in ledger if v2 isn't), duplicate-version rejection at register time, downgrade guard (ledger says vN applied but no migration registered → refuse to proceed), failure isolation (mid-run failure leaves ledger consistent for resume).  Ledger DB intentionally separate from `crisp_jobs.db` — admin metadata stays isolated from runtime data.

### Bosnian-PDF demo (the user's motivating example)

Two surfaces, both work end-to-end:

```ts
// On-demand: search hit, user clicks "Translate to en"
invoke('translate_text', {
    text: chunk.full_text,
    source_lang: chunk.language,        // populated by Phase 7 (b) at index time
    target_lang: 'en',
    mt_backend: 'm2m100',               // any-to-any 100 langs
    lid_model: '/models/cld3-f16.gguf', // used when source_lang is null
})
// → { translated_text: 'Hello, how are you?', source_lang: 'bs',
//     cached: false (first call) / true (repeated clicks) }
```

```rust
// Index-time batch: bg_ingest with translate_to = Some("en")
//   1. extractor decodes + transcribes (audio) or reads text (PDF/DOCX)
//   2. text_lid hook detects 'bs' (Bosnian) and writes it to extracted.language
//   3. translation hook runs m2m100 bs→en, writes to extracted.translated_text
//   4. bg_ingest packs into RawDocument and the IngestPipeline writes:
//        - language = 'bs'              (existing column)
//        - text_translated = 'Hello...' (NEW column, added by v100 migration)
//        - text_translated_lang = 'en'  (NEW column, added by v100 migration)
//   5. search hits return the English translation immediately, no per-query
//      MT cost.
```

### Disk-pressure side-fix

Boot disk hit 100% mid-session.  Recovered ~7 GB by `rm -rf`-ing the in-tree
`target/`, then pinned all future CrispSorter builds to `<external-volume>` via
a per-repo `.cargo/config.toml` (gitignored — paths are workstation-specific).
The existing `~/.zshenv` `cargo()` function wrapper only catches interactive
shell invocations; tauri-cli spawns cargo as a child process which bypasses it.
A per-repo `config.toml` survives any spawning context.  Captured in PLAN.md +
[`LEARNINGS.md`](LEARNINGS.md) so the next workstation rebuild doesn't waste
time rediscovering this.

### What's deferred (queued explicitly in commit messages + PLAN.md)

See PLAN.md "P13.5 follow-ups" for the full list.  Highlights:

- IndexConfig.translate_to settings UI so batch translation can be turned on
  without code edits (bg_ingest currently hard-codes `None`).
- Search-side query rewrite: hit `text_translated` column when target_lang
  filter is set (today always hits `full_text`).
- Frontend integration of `translate_text` into the Übersicht search-results
  panel (per-result "Translate to …" affordance).
- SRT / VTT output formats for `chat transcribe` (needs `transcribe_segments`
  on the safe wrapper since the current method concatenates).
- Streaming `--stream` flag for live captions when reading from stdin.

---

## Session log — 2026-05-11 — P13 Bilder Tier 2 completion (B2–B5)

Continuation of the same working session as the entry below.
Closes Tier 2 of P13 against the user's live CrispLens server at
`<crisplens-host>` (FastAPI v2 production instance).

| Commit | Slice | Headline |
|--------|-------|----------|
| `250f137` | **B4** | Health monitor + 4-state degradation banner (hidden / offline / session_expired / warming_up / ok); 30 s poll lifecycle gated on (active tab == images + Tier 2 configured); idle network traffic = zero.  Plus a side fix for `enable-crispembed.sh` after `cargo clean` (script copied libs into `$PROJECT_ROOT/target/` but the `~/.zshenv` cargo wrapper redirects to `<external-volume>/code/cargo-target/<reponame>` — script now mirrors the wrapper's resolution). |
| `8a4a2e0` | **B5** | Open-in-CrispLens deep-link button in the preview pane + watchfolders cross-reference hint.  Live verified: `WatchFolder` permissive `serde_json::Value` shape handles SQLite int booleans (`recursive: 1`) + REAL-typed `scan_interval_hours: 24.0` from the live v2 server. |
| `01e6203` | **B3** | People view + `/api/images/{id}/faces` end-to-end.  Two material deviations from the spec sketch surfaced during the live demo: `bbox` is a NESTED OBJECT `{top, right, bottom, left}` (not flat columns); `image_id` is ABSENT from v2 face rows (caller knows it from the URL).  Type reshaped to match reality; pinned with a `face_v2_live_payload_parses` regression test using verbatim captured JSON. |
| `814efe8` | **B2 reduced** | Scope check: live `/api/search` is filename / person-name substring only on both v2 and v4 — no semantic backend exists in CrispLens today.  Slice shipped as "remote text search" with the UI labelled honestly; true semantic remains an upstream-CrispLens TODO. |

Net delta: +13 unit tests in `crisplens-protocol` (29/29 now; +6 for
B3 Person/Face, +5 for B5 WatchFolder, +2 for B2 SearchHit) and +3
in `tauri-app::images::crisplens` (18/18, all from B4).

### Live verification recipe (for posterity)

Once per CrispSorter rebuild, macOS Keychain prompts for ACL on the
existing entry because the binary signature changed.  Headless
demo workaround:

```
security delete-generic-password -s "CrispSorter.CrispLens" -a <URL>
CRISPLENS_PASSWORD=… crispsorter images crisplens login --user <U>
```

Then the rest of the demo runs without dialog interruptions.
Doesn't affect production users (they don't rebuild the binary).

### Live demo against <crisplens-host>

* B4 status (offline simulation):
  ```
  $ crispsorter images crisplens status -f text     (after bogus URL)
    health: FAILED / authenticated: false
    note: "health probe failed: error sending request for url …"
  ```
* B5 watchfolders:
  ```
  $ crispsorter images crisplens watchfolders -f json
    [{"id":2,"path":"/opt/crisp-lens/uploads",
      "recursive":1,"auto_scan":0,"scan_interval":24.0,"enabled":null}]
  ```
* B3 people + faces:
  ```
  $ crispsorter images crisplens people -f text
    9 person cluster(s):
      [ 33]    0×  Alexander Kenneth-Nagel
      [  1]   12×  Christian Ströbele
      …

  $ crispsorter images crisplens image-faces 201
    3 face(s) in image 201:
      [238] ✓ det=0.88  bbox=t0.43,r0.33,b0.58,l0.26  Hussein Hamdan
      [240] ✓ det=0.85  bbox=t0.46,r0.54,b0.59,l0.48  Karin Schieszl-Rathgeb
      [239] ✓ det=0.88  bbox=t0.35,r0.79,b0.52,l0.71  Christian Ströbele
  ```
* B2 text search:
  ```
  $ crispsorter images crisplens search 'Christian' --limit 5
    5 match(es) for "Christian" (text search, NOT semantic):
      [ 134] 2f      3f2e3cfddbc849e6ac1d257d63f5539d.jpg
      … (4 more)
  ```

### Process side-fix surfaced: cargo clean during build (commit `250f137`)

The user ran `cargo clean` mid-build to recover disk space, which
yanked files cargo was actively reading.  The original build
failed with `error: could not compile tauri-app (lib) due to 1
previous error` — that error being IO/file-not-found rather than
a real code issue.  Confirmed by `cargo check -p tauri-app --lib`
passing cleanly after the clean.  Fresh-from-scratch build took
~38 min (vs ~24 min when starting from a warm incremental cache).

Folded the bonus `enable-crispembed.sh` fix into the B4 commit
because (a) the user requested it during the B4 wait, and (b) the
script's broken path-resolution would have stalled future demos
the same way.

### What's deferred

Two items, neither blocking Tier 2 declared complete:

1. **Image-overlay face boxes** — drawing `Face.bbox` rectangles on
   the previewed image.  Blocked on image_id ↔ doc_id cross-
   reference: CrispLens's `/api/images` doesn't emit sha256 at the
   list level.  Workable interim: filename + filesize probabilistic
   match.  Better fix: CrispLens upstream gains
   `GET /api/images/by-hash/{sha}`.
2. **True semantic search** — Wire `/api/search/semantic` once
   CrispLens grows it.  One-line URL swap on CrispSorter's side
   plus a UI label update.

---

## Session log — 2026-05-10/11 — P13 Bilder Tier 1 (A1–A4) + Tier 2 foundation (B1)

Implemented [`docs/P13_Bilder_integration.md`](docs/P13_Bilder_integration.md)
through slice B1.  Six commits on `main`:

| Commit | Slice | Headline |
|--------|-------|----------|
| `76e8a79` | (pre) | `fix(crispcat)`: tokio dev-dep so `cargo test --workspace` builds the lance-feature tests |
| `b2853d8` | **A1** | Bilder tab + image-row filter on the existing LanceDB index |
| `deb920a` | (rename) | bilder→images: drop Denglish from Rust + CLI + Svelte + i18n keys (DE values stay) |
| `6795548` | **A2** | thumbnail generator + EXIF preview pane (incl. kamadak `continue_on_error` fix for piexif-shaped IFD chains) |
| `abf7266` | **A3** | SHA-256 dup view (image rows grouped by `source_hash`) |
| `ce0bfbd` | **A4** | pHash near-dup view (chose `HashAlg::Gradient` over Mean+DCT after live demo surfaced image_hasher's small-buffer DCT collapse) |
| `0aa3a51` | **B1** | `crisplens-protocol` crate + keyring-backed session storage + Settings UI |

Net delta: +351 unit tests across the new modules + the workspace
fix.  Total now 311 in `tauri-app` lib (was 232 baseline).

### Spec vs reality (B1 live-server cross-check)

`docs/P13_Bilder_integration.md` was written before the CrispLens
HTTP routes were inspected.  When B1 work started against the
real server (`<crisplens-dir>` source +
`<crisplens-host>` live instance) the protocol-types
sketch turned out to be **aspirational across the board**.  The
deviations were uniform between v2 (FastAPI) and v4 (Express):

| Spec said | Reality (v2 + v4) |
|-----------|-------------------|
| `Authorization: Bearer <jwt>` | **httpOnly session cookie** (`session=<value>`) |
| `LoginResponse {access_token, token_type, expires_in}` | `{ok, username, role, token?}`; v2 echoes `token` in body, v4 cookie-only |
| `Image {path, size, sha256, phash, gps_lat/lon, exif}` | `{filepath, file_size, …}` — no sha256/phash/gps/exif at the list endpoint |
| Single `rating: i32` | v4 emits both `rating` + `star_rating`, v2 only `star_rating` (HTTP adapter renames v2→v4 before serde) |
| `ImagesPage {items, total, page, page_size}` | v4: `{images, total}`; v2: bare array `[…]` (adapter wraps) |
| `HealthResponse {status: "ok"\|"degraded", face_engine}` | v4: `{ok, version, backend}`; v2: `{ok, model_ready, …}` |

The protocol crate now models v4-canonical names with permissive
defaults; 16 unit tests pin both v2- and v4-shaped JSON payloads
extracted from the live route source so any future drift surfaces
as a failed deserialise rather than a silent UI bug.  See
`crates/crisplens-protocol/src/lib.rs` top doc-comment for the
full delta.

### Live verification of credential containment (B1)

The spec's risk register required: "Token storage — JSON config
leaks credentials on backup / cloud-sync.  Use Keychain / DPAPI /
secret-service; never write token to `tauri-plugin-store` JSON".

Verified end-to-end against `<crisplens-host>` with the
admin credentials in `<local-env>`:

```
$ crispsorter images crisplens set-url '<crisplens-host>' --enable
$ CRISPLENS_PASSWORD=… crispsorter images crisplens login --user <admin-user>
  → "logged in as <admin-user> (admin)"
$ security find-generic-password -s "CrispSorter.CrispLens" -a "<crisplens-host>"
  → entry exists in macOS Keychain
$ cat <data_dir>/crisplens.settings.json
  → { "backend":"crisplens", "url":"<crisplens-host>", … }
    (no token / no cookie / no password — credential-free)
$ crispsorter images crisplens logout
  → server-side cookie invalidated + Keychain entry wiped
```

### A4 implementation deviation (DCT-pHash → gradient hash)

The spec called for "64-bit DCT-pHash for stability".
`image_hasher`'s `.preproc_dct()` runs the DCT on a `hash_size`-
shaped buffer, not Krawetz's canonical "32×32 DCT → low-frequency
8×8 block".  At our wire-mandated 64-bit hash size that means an
8×8 DCT input where the DC coefficient dominates so heavily the
resulting hash collapses to a single bit.  Surfaced live: gradient,
inverted gradient, AND a coarse checkerboard fixture all hashed to
`0x0…01`.  Switched to `HashAlg::Gradient` (8×8, no DCT
preprocessing) — still 64 bits, still threshold-tunable around 8,
genuinely informative on real images.  Public identifier `phash`
is preserved so the future LanceDB `phash INT64` column lands
without churn.  Full rationale in
`src-tauri/src/images/phash.rs` top doc-comment.

### What's left of P13 (B2–B5)

| Slice | Spec hours | Doable? | Notes |
|-------|-----------|---------|-------|
| **B2** semantic search | 5 | partial | `/api/search` endpoint exists but does **filename / person-name substring** only (v2 + v4 both).  No embedding-based search backend.  Either ship "remote text search" with a labelled scope, or wait for a CrispLens upstream change. |
| **B3** Faces subtab | 8 | yes | `/api/people` + `/api/images/{id}/faces` endpoints verified live; payload shapes captured for future protocol-crate addition. |
| **B4** health monitor + degradation banner | 4 | yes | `/api/health` already verified live; the polling loop + banner are pure UI work. |
| **B5** open-in-CrispLens + watchfolder cross-reference | 4 | yes | `/api/watchfolders` returns `[]` on the live server (no folders configured) but route is reachable; deep-link is just a URL build. |

---

## Session log — 2026-05-09/10 — P11 cloud drives end-to-end + live e2e + upstream bug fixes

PLAN.md P11 had named four pillars (server, runtime modes, IVF-PQ scale,
sync) plus "cloud drives" as a placeholder.  This session closed the
cloud-drive pillar end-to-end across three repos (CrispSorter +
internxt-cli + filen-python), surfaced two real upstream bugs along the
way, and wired the whole chain into the Übersicht UI.

### Drives layer (Rust)

`src-tauri/src/drives/` grew from a one-impl stub (`LocalDrive`) to four
real backends sharing a single `trait CloudDrive`:

  * `LocalDrive`     — `std::fs`-backed (covers OS-mounted SMB/NFS/SFTP).
  * `InternxtDrive`  — subprocess to a patched `internxt-cli/cli.py`
    that gained `--json` flags on `whoami` / `list-path` / `resolve`.
    Rust deserialises typed JSON instead of scraping emoji text.
  * `FilenDrive`     — same pattern with `filen-python/cli.py`, which
    additionally got a missing `handle_trash` method (the dispatch
    referenced it, but the method didn't exist — so `cli.py trash …`
    crashed with `AttributeError` regardless of `--json`).
  * `WebDavDrive`    — generic HTTP-based.  Wire-shape parser handles
    both `D:`-prefixed (Nextcloud/ownCloud) and default-namespace
    (Synology) PROPFIND XML.  Optional `insecure_tls` flag flips
    `reqwest::ClientBuilder::danger_accept_invalid_certs` for the
    self-signed local servers spun up by `internxt-cli webdav-start` /
    `filen-python webdav-start`.

Routing fix in `DriveRegistry::instantiate`: the previous code funnelled
*every* `DriveType` variant through `LocalDrive` (a leftover stub).
Each kind now lands at its real backend.

`DriveConfig` gained `username` / `password` / `insecure_tls` fields,
all `#[serde(default, skip_serializing_if = "Option::is_none")]` so
existing `drives.json` files round-trip unchanged.

### URI scheme + ingest/promote

`FileLocation::Drive { drive_id, remote_path }` is the new URI variant:
`crisp+drive://<drive-id>/<remote-path>`.  Generic — works for any
registered backend.  Coexists with the existing `crisp+local://`,
`crisp+vps://`, `crisp+internxt://`, `crisp+cb-archive://` schemes.

Two new Tauri commands closed the ingest+promote loop:

  * `index_ingest_drive_manifest` — recursive walk via the new
    `crate::drives::walk()` helper (free function, kept off the trait
    so `Box<dyn CloudDrive>` stays object-safe).  Builds `L1FileEntry`
    rows tagged with `crisp+drive://` and batches 64 at a time through
    `pipeline.ingest_l1`.  Manifest-only — no bandwidth cost beyond
    directory listings.  Optional ext filter + max-depth.
  * `index_promote_drive_archive` — fetches a single file via
    `drive.read_file`, stages under `app_data/drive_retrieve/`, and
    routes through the existing cb-archive `promote_path` pipeline so
    extract+embed+L3-replace logic stays in one place.  Mirrors the
    UX users already trained on for cb-archive promote.

### SyncManager — pull-apply loop closed

`pull_pending` previously returned counters; now returns
`Vec<SearchHit>` + `max_indexed_at`.  `sync_pull` Tauri command writes
those rows as L1-metadata `DocumentChunk`s into local LanceDB
(`chunk_index = -1`, `metadata_json = {"level":1, "source":"sync_pull"}`)
and only advances `last_pull_ts` after the LanceDB writes succeed — so
a mid-apply crash re-fetches the same rows next time (idempotent because
LanceDB row PKs are stable).

### UI wiring (Svelte)

Three additions to `IndexIngest.svelte`'s Quellen tab:

  * "Cloud-Ordner" toolbar button next to "Ordner hinzufügen" — opens
    an inline dialog.
  * Inline create/edit/delete drive form — Label / Typ (webdav, filen,
    internxt, local, sftp) / URL or path / WebDAV-only Benutzer +
    Passwort + "Selbstsigniertes Zertifikat akzeptieren".  Auto-shown
    when no drives registered; `+` toggle when at least one exists.
    Edit prefills the form, switches "Anlegen" → "Speichern", calls
    `drive_update` (the new sibling to `drive_create` that preserves
    the id so `crisp+drive://<id>/...` index rows keep resolving).
    Delete confirms with a warning that index rows for that drive
    remain but become unpromotable.
  * Per-row "Promote to L3" CloudDownload icon-button on
    `crisp+drive://` rows — sibling to the existing
    `crisp+cb-archive://` button at `IndexIngest.svelte:1272`.

### Live e2e tests

Two `#[ignore]`'d integration tests in `drives::webdav::tests`
(`webdav_live_list_root`, `webdav_live_write_read_delete_roundtrip`)
gated by `WEBDAV_TEST_URL` / `WEBDAV_TEST_USER` / `WEBDAV_TEST_PASS` /
`WEBDAV_TEST_INSECURE` env vars.  Tolerant of server-quirky DELETE
failures (logs the warning instead of failing the assertion) so the
suite works across partially buggy servers.

These tests immediately surfaced **two real upstream bugs**:

#### Bug #1 — internxt-cli: PROPFIND root crashes with `int(None)`

`Folder.get_etag()` did `modified = int(self.get_last_modified())`,
where `get_last_modified()` falls through to `super().get_last_modified()`
which returns `None` for the root collection (despite the type
annotation lying about `-> float`).  Fixed by making
`get_last_modified()` always return a real float (`0.0` fallback) and
adding defensive `try/except` around the `int()` call.

#### Bug #2 — filen-python: DELETE always returns 500

`drive_service` caches folder/file listings for 10 minutes (TTL).
`trash_item()` and `delete_permanent()` didn't invalidate that cache.
After DELETE, wsgidav's post-check `provider.exists(path, environ)`
saw a stale cache entry and reported the resource as still alive →
`DAVError(HTTP_INTERNAL_ERROR, "Resource could not be deleted.")`.
Even though the underlying API call had succeeded.

Fixed by adding `_invalidate_all_caches()` to both `trash_item` and
`delete_permanent`.  Also helps any other caller (CLI `trash`,
`delete-path`) since just-deleted items previously reappeared in `ls`
for up to 10 minutes.

#### Both fixes pushed upstream

The patches live in their respective repos
(`internxt-python/845ed2d`, `filen-python/dd88a41`); the integration
tests now pass against both servers' full PUT→STAT→GET→DELETE round-
trip.

### CI rescue (internxt-python)

The internxt-python repo's CI lane had been red across many commits
(unrelated to my patches).  Walked the failures one by one:

  1. **mypy** — 5 errors (`st_birthtime` missing on Linux stub, two
     unused `# type: ignore`, one duplicate-name `wsgi`).  Fixed with
     cross-platform `# type: ignore[<code>, unused-ignore]` patterns
     (the `unused-ignore` companion suppresses mypy's own meta-warning
     when the underlying code doesn't fire on the current platform).
  2. **pytest — 7 failures** across 4 test files:
     * `get_content` — auth lookup happened before the pending-shortcut
       check; tests for pending/missing-uuid resources couldn't pass
       without credentials.  Moved shortcut first.
     * `start(server_choice='nonexistent')` — provider construction
       (which needs auth) ran before `server_choice` validation, so
       invalid choices returned `MissingCredentialsError` instead of
       the explicit `ValueError`.  Hoisted validation to the top of
       the `try:` block.
     * `_available_memory` Linux fallback — ran for *any* non-darwin
       non-win32 platform, including the synthetic `'unknown-os'` the
       4 GB-fallback test patched in.  Gated on
       `sys.platform.startswith('linux')`.
     * `cheroot` test — sys.modules-injected stub couldn't help because
       `from cheroot import wsgi` first imports the package itself
       (not installed in CI's `requirements.txt`).  Added `cheroot>=10.0`
       to `requirements-dev.txt`.
     * `test_isolated_session_separate_threads_get_separate_clients`
       (intermittent on 3.10) — each thread independently entered a
       `with patch(...)` block; `unittest.mock.patch` is not thread-
       safe, races between `__enter__` / `__exit__` let the real auth
       code leak through and kill thread 2 silently, leaving the
       `clients[2]` slot unset → `KeyError`.  Hoisted both patches out
       of the per-thread body so they wrap the entire join window.

After all 4 commits the lane is green across Python 3.10/3.11/3.12.

### CrispSorter test coverage

Drives + location: 53/53 unit tests (LocalDrive ×7, Registry ×3,
DriveType + instantiate ×2, InternxtDrive ×8, FilenDrive ×6,
WebDavDrive ×9, FileLocation ×18).  Plus 2/2 ignored live tests
against both Filen and Internxt webdav servers.

---



The Axum VPS backend that PLAN.md P11 names as the server side of the
remote-architecture story used to live in a sibling directory
(`../crisp-index-server`) without a git repo. P11 still described it as
"a documented skeleton with stub handlers", but the local code had
already grown a full LanceDB + Tantivy + RRF implementation. Two
parallel definitions of the wire format (`IngestChunk` / server,
`IngestPayload<'a>` / client) were drifting silently.

This session vendored the server into the CrispSorter repo as a Cargo
workspace member, with a third member crate (`crisp-index-protocol`)
holding the wire types both sides depend on.

### Layout change

```
CrispSorter/
├── Cargo.toml             ← new workspace root (resolver = "2")
├── Cargo.lock             ← unified workspace lockfile
├── crisp-index-protocol/  ← wire types + serde tests (new)
├── crisp-index-server/    ← copied from ../crisp-index-server (no prior git)
└── src-tauri/             ← existing Tauri 2 desktop app, now a member
```

The previous `src-tauri/Cargo.lock` was deleted; the workspace root
owns the lockfile.

### Why a workspace, not a separate GitHub repo

- P11 steps 1, 2, 4, 5 are intentionally paired client + server changes
  (`embedderLocation` flag, `IngestBatch`, `/v1/ingest/batch` 202 +
  task_id). One repo lets one PR touch both sides.
- The protocol crate ends the parallel-types problem: change the
  `IngestChunk` shape and both crates rebuild; change one and serde
  tests in `crisp-index-protocol` catch it.
- The server can still be released and deployed independently — its
  `crisp-index-server/README.md` Docker / systemd / nginx recipes work
  unchanged.
- No prior git history existed for `../crisp-index-server`, so a clean
  import was free.

### Protocol crate (`crisp-index-protocol`)

Single source of truth for: `IngestChunk`, `IngestResponse`,
`SearchRequest`, `SearchFilters`, `SearchHit`, `UpdateLocationBody`,
`UpdateLocationByUriBody`, `UpdateLocationResponse`, `DeleteResponse`,
`StatsResponse`, `HealthResponse`, `ErrorResponse`. Plus
`SearchFilters::to_lance_sql()` (pure-string), used by the server's
LanceDB layer.

`SearchHit` is the strict wire subset. The client-side `SearchResult`
in `src-tauri/src/index/schema.rs` is a superset — its extra optional
fields (`metadata_json`, `catalog_source`, `volume_id`) are populated
locally for catalog-channel hits, ignored when reading server
responses (default `None`). Keeping these split means the server
doesn't depend on LanceDB schema details that are only meaningful
client-side.

Bonus correctness fix: `SearchHit` now includes `ext` (was missing
from the server's old local `SearchResult`); the server reads it from
the LanceDB `ext` column in `batches_to_results`.

`tags` standardised to `Vec<String>` with `#[serde(default)]` on both
sides — was `Option<Vec<String>>` on the server and `&[String]` on the
client; round-trip was already compatible but the new shape removes
one Option unwrap.

### Build system tweaks

- `crisp-index-server/build.rs` now mirrors `src-tauri/build.rs`'s
  protoc fallback via `protoc-bin-vendored`. The transitive
  lance-encoding requirement on `protoc` is now covered by both
  workspace members independently.
- Root `.gitignore` adds `/target` (workspace target dir) and
  `crisp-index-server/{data,target}` so a real deployment's hundreds
  of GB of LanceDB shards never get accidentally `git add`-ed.

### Verification

- `cargo build -p crisp-index-protocol` — green.
- `cargo test  -p crisp-index-protocol` — 4/4 passing
  (round-trip, omit-None, tolerant-deserialize, lance SQL).
- `cargo build -p crisp-index-server` — green (with two pre-existing
  unrelated `unused_mut` / `dead_code` warnings).
- `cargo build -p tauri-app` — desktop app still compiles unchanged
  modulo the protocol-crate dep (verified after the workspace
  conversion; the move from `src-tauri/target` to `/target` triggers
  a full re-link but no source changes).

### What this unblocks

P11 steps 4-7 (server bulk ingest API, server-side embedding, IVF-PQ
with sample_rate, sharding) are now in-tree work. The next concrete
step is P11 refactor (a) — `index_ingest_batch` Tauri command + the
parallel `crisp_index_protocol::IngestBatch { chunks: Vec<IngestChunk> }`
wire type. Both can land in one commit since both halves live here.

### Disk hygiene fallout from the workspace move (commits `4654c18`, `10ecaab`)

The workspace promotion above silently changed where `cargo build`
puts artefacts: from `src-tauri/target/` to `/target/` at the repo
root. None of the developer scripts noticed:

* `enable-crispembed.{ps1,sh}` kept staging DLLs into
  `src-tauri/target/{debug,release}/` while cargo was writing the
  .exe to the new location. STATUS_DLL_NOT_FOUND on every cuda /
  vulkan build, except where a previous run had left the same DLLs
  in `src-tauri\bin\` (Tauri's bundled-resources dir, which
  *is* searched).
* `recompile-exe.ps1` kept reporting "Build successful! Executable
  located at: src-tauri\target\release\CrispSorter.exe" when the
  .exe was actually somewhere else.
* `release.sh`, `scripts/build.sh`, `scripts/bundle_macos_native_libs.sh`
  all looked in the legacy paths. The macOS notarisation /
  bundling pipeline would have failed on first run on any clean
  machine.
* `scripts/build.sh`'s `CRISPSORTER_TARGET_VOLUME` symlink-to-
  external-SSD trick was silently broken — it created a symlink
  at `$SRC_TAURI/target/` while cargo wrote to
  `$REPO_ROOT/target/`. Users who'd set it up to keep build
  artefacts off the boot drive were quietly seeing them back
  on the boot drive.

The workspace orphaned `src-tauri/target/` accumulated 26 GB of
pre-workspace artefacts on the user's notebook (debug 20 GB +
release 5.7 GB) — boot drive at 99% full, 6.4 GB free.

Fix in two commits:

* **`4654c18` — script paths.** All six callers now write to /
  read from `target/` at the workspace root first; legacy
  `src-tauri/target/` paths kept as graceful fallbacks for
  branches that haven't picked up the workspace move.
* **`10ecaab` — `CARGO_TARGET_DIR` honoured.** The DLL-staging
  code in `enable-crispembed.ps1` and the .exe-locator in
  `recompile-exe.ps1` both read `$env:CARGO_TARGET_DIR` if set,
  falling back to `$ProjectRoot\target`. User-facing
  "Staged N DLL(s) to ..." message reads from the same
  variable so the printed path is honest.

After cleanup:

* `rm -rf src-tauri/target` recovered 26 GB instantly (build
  cache only; .gitignored; regenerated on next build).
* Repo size 31 GB → 5.2 GB on disk; free 6.4 GB → 32 GB.
* User's standard incantation for "build with target on D:\":

  ```powershell
  $env:CARGO_TARGET_DIR = "D:\cargo-target\crispsorter"
  .\enable-crispembed.ps1 -Backend cuda
  ```

  Documented in PLAN.md → P4 → "Disk hygiene: redirect Cargo
  target dir to an external drive."

---

## Phase ship index — moved-from-PLAN items as of 2026-05-10

This section consolidates everything that was marked `[x]` in
PLAN.md's "Open TODOs" up through the 2026-05-09/10 session.  They
are preserved here so the active plan stays focused on `[ ]` work
only.  Where a session log above this point goes into deep detail
(e.g., the cloud-drives session log), the entry below is a one-liner
that points at it.

### P3.5 — CrispEmbed / CrispASR bundling (Phase 1)

- **macOS arm64** — `scripts/bundle_macos_native_libs.sh` processes both
  `libcrispasr.dylib` and `libcrispembed.dylib` (+ ggml backend libs +
  recursive homebrew transitives) into `.app/Contents/Frameworks/`
  with `install_name_tool` rewriting absolute LC_RPATH entries to
  `@loader_path/.`.  Each wrapper is independently feature-gated, so
  builds with only `--features crispasr-metal` skip CrispEmbed cleanly.
  Phase 2 (Linux + Windows) and Phase 3 (mobile) remain open.

### P6 — Catalog (Phase 5)

- **`crispcat` workspace crate** — `crates/crispcat/` ships `caf` /
  `dedup` / `index` / `scan` modules; `lance` module is feature-gated
  (default off) so `cargo install crispcat-cli` doesn't pull in
  lancedb.  Tauri app uses `crispcat = { features = ["lance"] }` and
  re-exports as `crate::catalog`.  Standalone
  `crispcat scan|info|browse|find-dupes` binary in
  `crates/crispcat-cli/` — no Tauri, no LanceDB, no embedder.

### P7.7 — Mountable archive index

- **LanceDB export (`export_cidx`)** + Tantivy FTS companion
  (`--include-fts`); Übersicht "Archiv" tab mounts the export and
  auto-loads the FTS companion.
- **Background-ingest on `.cidx` import** — Archiv tab checkboxes,
  selection bar with "Auf L3 hochstufen" calling
  `index_promote_cb_archive` per selected cb-archive row, "archiv"
  badge on L1 cb-archive rows.

### P7.8 — OCR Tier 3

- **PaddleOCR via `usls`** (`--features paddle-ocr`).  DB detection +
  SVTR recognition, CJK/Latin model selection via `OcrRecLang` enum
  (Auto/Latin/Cjk), Auto-tier path heuristic, Settings dropdown,
  `bg_ingest.ocr_rec_lang` field + matching Tauri command.
- **SLANet table extraction** still open.

### P8.2 — CLI (continuation, partial)

- Existing surface: `version / doctor / catalog / index stats|list|
  search|delete|export-cidx|inspect-cidx|list-failed|retry-failed|
  ingest-cb-manifest / batch add|list|apply / completion / manpage`.
- **`index init --model M --device D`** — downloads embedder model to
  `data-dir/models/`; supports bge-m3, multilingual-e5-*, bge-*-en-v1.5,
  nomic, minilm.
- **`index ingest <paths>... [--model M] [--device D]`** — full
  extraction+embedding pipeline headless; walks directories,
  SHA-256 + extract + embed + LanceDB+Tantivy write.
- **`batch process [--job-id J] [--limit N] [--llm-url URL]
  [--llm-model M] [--export-path DIR] [--path-template T]
  [--out-plan FILE] [--dry-run]`** — headless LLM extraction pass,
  emits sort plan JSON.
- **`chat query "<prompt>" [--context-files] [--system]`** — POSTs to
  OpenAI-compatible `/chat/completions`.
- **Polish (partial)** — `cargo install --path crates/crispcat-cli`
  works for the standalone catalog CLI.  Full
  `cargo install crispsorter` story for the Tauri-app binary still
  pending a binstall recipe + signing.

### P10 — Robust ingest remaining

- **DRM help-popover** — clicking `fail-badge.fail-drm` opens an
  inline popover explaining the encryption, with a close button.  No
  third-party tool recommendations.
- **CLI `skip-failed`** —
  `crispsorter index skip-failed [--dry-run]` permanently marks
  timeout/other rows as "unsupported".

### P11 — Remote server (everything shipped)

- **Server queue blob fix** — `embeddings_blob BLOB` + `embed_dims`
  columns; `payload_json` stores compact batch with empty vectors;
  blob repacked on claim.
- **IVF-PQ at 100 M+ vectors** — `num_partitions` auto-scales to
  `sqrt(row_count)`, `sample_rate` exposed on `index_build_ivf_pq`
  Tauri command + `build_vector_index()`.
- **Runtime modes** — `BackendType` gains `Hybrid` variant
  (serialises as `"hybrid"`).  Hybrid init path = Local for now
  (SyncManager placeholder).  Settings dropdown shows
  Standalone/Server/Hybrid with i18n; data-dir + remote fields
  visible in Hybrid.
- **Cloud drives + `crisp+drive://` + UI + live e2e + upstream
  server fixes** — covered in detail in the "2026-05-09/10 — P11
  cloud drives end-to-end" session log above.
- **SyncManager** — SQLite outbox at `src-tauri/src/sync/`,
  `enqueue/claim_batch/mark_done/mark_error/clear_failed`,
  `push_pending` (POST per op type),
  `pull_pending` (GET `/v1/sync/since?ts=…&limit=…`),
  `is_remote_online` (GET /health), `sync_state` kv table.  Server
  side: `routes/sync.rs` + `VectorStore::rows_since(since_ms, limit)`
  + stdlib `iso_from_ms` formatter.  5 Tauri commands;
  nav sync chip (⇅ N) polls every 30 s.

### P12 — cloud-backup (everything shipped)

- **L1 manifest import** via `index_ingest_cb_manifest`.
- **L3 promotion** via `retrieve.py` (`index_promote_cb_archive` +
  CloudDownload button in Übersicht).
- **Reverse lookup UI** — `index_lookup_cb_file` Tauri command
  queries `source_files`+`archives`; preview pane shows
  Lokal / VPS / Cloud (Internxt) availability when a
  `crisp+cb-archive://` row is opened.  Reads
  `archives.upload_verified` + `remote_path` + `local_deleted` so the
  chip distinguishes "VPS verified" from "VPS pruned, cloud-only".
  Manifest DB path persisted as `cbManifestDbPath` setting on first
  import.
- **VPS-trigger indexing** — `vps_worker.py` gains
  `_notify_crisp_index()`: after PROCESSED, POSTs L1 file metadata
  (from manifest `files[]`) to `CRISP_INDEX_URL/v1/ingest/batch`
  (batches of 64) via `urllib.request`.  Opt-in via env vars
  `CRISP_INDEX_URL` / `CRISP_INDEX_API_KEY` / `CRISP_INDEX_OWNER_ID`.
  Fully non-blocking on failure.

---

## Session log — May 2026 — index-test → main reconciliation + P9 step 1+2

### Branch reunification (commits `400df29`, `33479da`)

The `index-test` branch had drifted significantly from `main` — 18
commits ahead carrying the L1/L2/L3 multi-level ingest, hf-hub
Windows-symlink workaround, GGUF model registry expansion,
NC-license gating, embedder benchmark, CAF round-trip restore,
`enable-crispembed.{ps1,sh}` + DLL staging, and protoc bootstrap
in `paths.ps1`. Meanwhile `main` had shipped 65 commits worth of
independent work — full Catalog/Cathy subsystem with `.caf v6`
+ volume-header round-trip, parallel scanner (jwalk-backed),
dedup engine, deletion scripts; LanceDB materialisation with
unified search; live preview pane; pure-Rust OCR via `ocrs`;
background ingest scheduler with mtime-skip + foreground-search
throttling; saved searches; field-prefix FTS syntax;
cross-mount volume awareness (P7.6); single-binary CLI mode;
matryoshka dim selection; cross-encoder reranking; macOS/Linux
native-lib bundling for `libcrispasr` / `libcrispembed`.

17 conflicts resolved with the better-of-both rule. Notable choices:

* **`index/embedder.rs`** — main's expanded model set
  (EmbeddingGemma300M, GTE base/large) wins; HEAD's
  `approx_download_mb` / `gguf_download_mb` /
  `gguf_quant_suffix_str` / `gguf_file_name` helpers + the
  hf-hub Windows workaround `fastembed_native_files()` preserved.
  Octen variants now route through fastembed-rs's auto-download
  (3 of 4 variants), keeping the local-only Int8 fallback.
* **`index/mod.rs`** — `IndexConfig` combines HEAD's `use_vector`
  master switch with main's `reranker_model` / `rerank_top_n`
  / `model_cache_dir` / `matryoshka_dim`.
* **`index/search.rs`** — combined HEAD's `Option<Arc<Mutex<Embedder>>>`
  (so L1/L2 paths work with `use_vector=false`) with main's
  reranker support and `EmbedRole::Query` asymmetric retrieval.
* **`index/tauri_commands.rs`** — pre-compute `models_dir` +
  `effective_dim` up-front so they're available even when
  `load_embedder=false`. Then conditionally construct the
  `Option<Arc<Mutex<Embedder>>>`.
* **`index/local_index.rs`** — kept main's `update_location_by_uri`
  AND HEAD's `update_l2_fields`. `SearchResult` carries both
  HEAD's `metadata_json` and main's `catalog_source` + `volume_id`.
* **`src/lib/log.ts`** — main's `frontendLogs` store + `flog`,
  HEAD's `logInfo` / `logWarn` / `logError` as wrappers that
  push to the local store (no Rust round-trip → no LogPanel
  duplication).
* **Settings.svelte** — kept HEAD's organised-by-size embedder
  dropdown with engine-aware filtering and NC-license gating;
  updated all model values + i18n keys to main's naming. Added
  EmbeddingGemma + GTE base/large to the mid/large optgroups.
* **Catalog subsystem** — main wins entirely. Pulled HEAD's
  pruned `caf.rs` / `index.rs` restore in favour of main's
  full v6-writer + dedup + lance modules.

After the merge, three trivial compile errors fell out
(`SearchResult` missing `metadata_json` in two call sites,
`embed_dense` arity change to take `EmbedRole`); fixed in
commit `33479da`. `cargo check --no-default-features`: green.

Branch hygiene: `index-test` was fast-forwarded into `main` (no
force push, since main was a strict ancestor of index-test
post-merge) and then deleted both locally and on origin. `main`
is once again the canonical branch.

### PowerShell scripts unstuck on PS 5 / German locale (commit `1f1d2a9`)

`paths.ps1` started failing to parse on a German Windows shell
with `Unerwartetes Token "Active"` and `Die Zeichenfolge hat kein
Abschlusszeichen`. Cause: PowerShell 5 reads UTF-8-without-BOM
files using the system code page (CP1252 on a German install),
so the multi-byte em-dash bytes inside string literals get
re-interpreted as a quote-like character and the parser sees
an unterminated string. Cascading "missing closing brace" errors
follow.

Belt-and-braces fix:
1. Replaced every em-dash with `--` (ASCII) in `paths.ps1`,
   `enable-crispembed.ps1`, `recompile.ps1`, `recompile-exe.ps1`.
2. Re-wrote each script with a UTF-8 BOM (EF BB BF) so PS 5
   detects UTF-8 explicitly.

Bonus: `enable-crispembed.ps1`'s "Staged 0 runtime DLL(s)"
message was misleading on the no-op happy path (re-running the
script when target dirs already had the DLLs at the right size).
Branch: print "Staged N DLL(s)" in green only when something was
actually copied; otherwise print a calmer "DLLs already up to
date (N files, no copy needed)".

### Settings + DB persistence on app restart (commit `e41d704`)

Symptom: every app restart, the Search-Index model selection
silently reverted to BgeM3 (the Rust default). Cause: the Rust
`IndexState` always boots with `IndexConfig::default()`. The
JS-side `tauri-plugin-store` carried the user's persisted
choices, but `Settings.svelte`'s onMount loaded them only
into JS state — never pushed them to Rust until the user
opened Settings and clicked Apply.

Fix: added a boot block in `+page.svelte` `onMount` that loads
every `index_*` setting from the store, translates UI keys
(`bge_m3`, `auto`, etc.) to Rust kebab strings (`bge-m3`,
`auto`) via inline maps mirroring `Settings.svelte`'s
`*ToRust` helpers, invokes `index_set_config`, and -- if
`cfg.enabled` is true — auto-invokes `index_init` with
`withEmbedder=false`. The L1 LanceDB rows from a previous
session now appear in Übersicht on app start instead of
showing an empty pane.

### UI cleanup: Kataloge umbrella + Duplikate sub-tab (same commit)

Pre-merge nav had two buttons firing on `activeTab === 'catalog'`
(both HEAD and main wanted that slot) plus a standalone
"Duplicates" button. Dropped both duplicates; single Kataloge
nav entry routes into `IndexIngest`, which now hosts the
.caf-volumes Catalog and Duplikate as sub-tabs alongside
Übersicht / Suche / Hinzufügen / Quellen.

### i18n coverage extension (commits `e41d704`, `2e92ec8`)

* Tab labels (Übersicht / Suche / Hinzufügen / Quellen) moved
  out of inline German literals into `i18n.t.indexIngest.tab_*`.
* New keys for the `tab_caf_catalog` + `tab_duplicates` sub-tabs.
* Full Duplicates pane: title / subtitle / source / destinations
  / match-mode strategy options / find / running / errors / 4
  picker dialog titles / result table column headers / matches +
  selected counts / deletion-script builder (format / target /
  generate / save / space-freed hint) / empty state. EN+DE.
* `Settings.svelte`'s "CrispEmbed was built with the cuda
  backend" hint moved to `i18n.t.crispembed_engine_built` /
  `_cpu` with `{backend}` substitution and minimal
  `**bold**` / `` `code` `` markdown rendering.

Open follow-up: the `.caf` Catalog sub-tab (`Catalog.svelte`)
still has hard-coded English strings. The `caf_catalog.*` i18n
keys exist (EN+DE) but the component hasn't been wired through
them yet.

### P9 step 1 — `index_query_documents` Tauri command (commit `4ecfd7a`)

Paginated, filterable, sortable browse of the documents table,
designed to drop in cleanly today and graduate to keyset + DB-side
ORDER BY without breaking the API contract.

* `DocumentFilter` / `SortSpec` / `PageSpec` / `PageCursor` /
  `DocumentPage` types in `index/schema.rs` (with `#[serde(default)]`
  on every field of `DocumentFilter` so the frontend can omit
  fields it isn't constraining).
* `LocalIndex::query_documents` + helpers (`filter_to_sql`,
  `sort_rows`) in `index/local_index.rs`. `total_estimate` via
  `count_rows` against the same predicate. 50k-row hard cap on
  the in-process sort window because LanceDB 0.26's public Rust
  query API doesn't expose `ORDER BY`.
* Tauri command + `lib.rs` registration. Returns an empty page
  silently when the index isn't yet initialised — the Übersicht
  pane polls during boot before `index_init` finishes, and
  erroring there would surface as red log lines instead of an
  empty state.
* 7 pure-function unit tests pinning filter SQL generation, sort
  ordering, and PageCursor offset round-trip.

### P9 step 2 — columnar Übersicht + multi-select (commit `9cbe0c1`)

* CSS-grid table — single `grid-template-columns` shared
  between thead and every row, sticky header. Columns: select,
  ext, name, author, year, size, modified, folder, level,
  actions.
* Server-side filter + sort + pagination via
  `index_query_documents`; the chip bar (folder prefix, ext
  multi-select, L1/L3 toggle, name substring, sort header)
  serialises into a `DocumentFilter`. Completeness chip stays
  client-side until P9 step 3 promotes those flags to scalar
  columns.
* "Load more" button paginates (fetches next 200 rows, appends
  in-place, preserves selection, total estimate shown alongside).
* Multi-row selection with mouse: bare click = single-row
  select, Shift+click = range from last anchor, Ctrl/Cmd+click
  = toggle. `user-select:none` on rows so dragging selects rows
  instead of highlighting filename text. Mirrors the
  `BatchReview.svelte` handler so the two panes feel identical.
* Bottom-left stats now shows a collapsible "Stapel: 271 · DB:
  321k" summary; click to expand the per-extension breakdown.
  `dbDocCount` polls `index_stats` every 4 s.
* Settings sidebar alignment fix — wrapped icon + label in a
  flex `<span class="prov-label">` so the App-Einstellungen
  buttons hug their icons; status checkmark sits to the right
  via the parent's `space-between`.

Bugs hit on first walkthrough, all fixed:

* "Übersicht is empty when entering Kataloge from another tab"
  — `loadContents` was wired only to the tab-button onclick;
  fixed via a `$effect` that fires on first activation +
  every chip change.
* "Always says Lade…" + ERROR loop — combined cause: an earlier
  auto-load `$effect` re-ran on every `_allContents = []` reset
  (which the catch path did on every error), and
  `DocumentFilter` rejected payloads that omitted any field
  (`missing field 'ext'`). Fixed: dropped the redundant effect,
  added `#[serde(default)]` on `DocumentFilter`, made
  `index_query_documents` return an empty page when local
  isn't initialised yet, deduped error log to one line per
  distinct message.

---

## RAG / Search Extension — Original Plan (March 2026)

> Originally `rag_plan.md`. Phases P1–P13 are all shipped; this section
> remains as the design rationale for the search-index architecture
> (LanceDB + Tantivy + dtSearch query syntax + URI-based location
> tracking + multi-user-from-day-one). When the code references
> "§N rationale" it points at the corresponding section below.
>
> Original status: planning → implementation. Last edited 2026-03-16.

---

## 0. Goals

1. Add **local LanceDB** semantic + full-text search to CrispSorter
2. Support **remote LanceDB** via a self-hosted Rust/Axum VPS server
3. Track **where every file lives** with a typed, forward-compatible URI scheme
4. Handle **hundreds of thousands** of German + English academic documents
5. Provide **advanced** proximity/wildcard/boolean full-text search
6. Keep the setup **versatile from the UI** (mode, embedder, device, backend)
7. Design for **multi-user** from the start without forcing it on single-user installs

---

## 1. File Location URI Model

Every indexed document carries a `location_uri` — a single UTF-8 string, structured as a typed URI.

### Scheme

```
crisp+local://{user-uuid}@{machine-uuid}/{absolute-path}
crisp+vps://{user-uuid}@{host}:{port}/{path}
crisp+internxt://{user-uuid}/{cloud-path}
crisp+internxt-zip://{user-uuid}/{archive-cloud-path}#{internal-path}
```

### Design decisions

- `user-uuid` — UUID v4 assigned at first CrispSorter launch, stored in app config.
  **Not** a username: usernames change, collide across machines, and leak PII into the index.
- `machine-uuid` — UUID v4 generated once per installation.
  For single-user installs both UUIDs are auto-populated and invisible in the UI.
- A **user registry** (small JSON sidecar, not in LanceDB) maps `user-uuid → display-name`
  so the UI shows "stc @ Desktop" rather than raw UUIDs.
- The `internxt-zip` scheme uses `#fragment` for the in-archive path — same convention
  as URL fragments, making the URI round-trippable with standard URL parsers.
- `metadata_json` (see schema) is the escape hatch for future location types — no
  schema migration needed.

### Rust enum

```rust
pub enum FileLocation {
    Local   { user_id: Uuid, machine_id: Uuid, path: PathBuf },
    Vps     { user_id: Uuid, host: String, port: u16, path: String },
    Internxt { user_id: Uuid, cloud_path: String },
    InternxtZip { user_id: Uuid, archive_cloud_path: String, internal_path: String },
}

impl FileLocation {
    pub fn to_uri(&self) -> String { … }
    pub fn from_uri(s: &str) -> Result<Self> { … }
    pub fn retrieval_cost(&self) -> RetrievalCost { … }  // Free / Cheap / Expensive
}
```

`retrieval_cost` lets the UI warn: "This file must be downloaded from Internxt before opening."

---

## 2. Embedder Selection

### Primary: `BAAI/bge-m3` via `fastembed-rs` 5.x

| Property | Value |
|---|---|
| Context window | **8 192 tokens** (decisive for long academic texts) |
| Languages | 100+; German and English both top-tier |
| Output | Dense 1024d + multilingual sparse (SparseModel::BGEM3) |
| Crate | `fastembed` 5.13.x (`EmbeddingModel::BGEM3`, `SparseModel::BGEM3`) |
| Execution providers | CoreML (Metal/Neural Engine, macOS) · CUDA (Windows/Linux) · CPU |

#### Quantisation

| Format | Size | CPU speedup | Quality loss | Status |
|---|---|---|---|---|
| FP32 | ~1.1 GB | 1× | 0% | **available** (`EmbeddingModel::BGEM3`) |
| INT8 | ~280 MB | 2–3× | <1% on BEIR | **not yet in fastembed hub** — load via `try_new_from_user_defined` with custom ONNX |
| Q4 | N/A | — | — | not applicable to ONNX encoder models |

> **Q4 answer**: ONNX Runtime does not support 4-bit quantisation for transformer encoder
> models the way llama.cpp/GGUF does for decoder LLMs. INT8 is the practical limit.
> A custom INT8 bge-m3 ONNX can be produced with `optimum-cli` and loaded via
> `TextEmbedding::try_new_from_user_defined` — planned as a future UI option.

#### Embedder menu (UI dropdown) — `EmbedderModel` enum

| Variant | fastembed model | Dims | Context | Sparse | Best for |
|---|---|---|---|---|---|
| `BgeM3` ★ default | `BGEM3` | 1024 | 8192 | `BGEM3` (multilingual) | de+en, all sizes |
| `MultilingualE5Large` | `MultilingualE5Large` | 1024 | 512 | none | lighter, still multilingual |
| `MultilingualE5Base` | `MultilingualE5Base` | 768 | 512 | none | faster, medium quality |
| `MultilingualMiniLm` | `ParaphraseMLMiniLML12V2` | 384 | 512 | none | VPS CPU, very fast |
| `BgeSmallEn` | `BGESmallENV15` | 384 | 512 | `SPLADEPPV1` | English-only |

VPS server defaults to `MultilingualMiniLm` (CPU-friendly) unless overridden by config.

#### Sparse model pairing rationale

- `BgeM3` → `SparseModel::BGEM3`: same model, multilingual sparse weights. ✓
- `BgeSmallEn` → `SparseModel::SPLADEPPV1`: English-only collection, SPLADE is fine.
- `MultilingualE5*` / `MultilingualMiniLm` → **no sparse**: English SPLADE against German
  text produces poor recall. Better to do dense-only than degrade hybrid results.

---

## 3. Chunking Strategy

Do **not** embed whole documents. Embed overlapping chunks aligned to section boundaries.

```
Document → extract headings (from Markdown structure)
         → split at headings; subdivide long sections into 512-token windows
            with 128-token stride overlap
         → one LanceDB row per chunk
         → whole-document row (chunk_index = -1) for metadata queries
```

At query time: retrieve top-K chunks → deduplicate by `doc_id` → rank by max-chunk-score.
Heading-aligned chunks are semantically coherent; stride overlap prevents boundary misses.

---

## 4. LanceDB Schema

One table per "library" (user-configurable). Rows are **chunks** (one per embedding unit).

```
id                Utf8            SHA-256 of (doc_id + chunk_index)
doc_id            Utf8            SHA-256 of file content (stable across moves)
location_uri      Utf8            crisp+* URI
owner_id          Utf8            user UUID (denormalized for fast filter)
filename          Utf8
title             Utf8
author            Utf8
year              Int32
ext               Utf8
language          Utf8            "de" | "en" | "de+en" | …
page_count        Int32
headings_text     Utf8            all headings joined (for boosted FTS field)
full_text         Utf8            stripped plain text (FTS source + embedding source)
full_text_md      Utf8            Markdown with heading hierarchy (for display/preview)
embedding         FixedSizeList<Float32>[1024]     bge-m3 dense vector
embedding_sparse  Utf8            JSON: {"term": weight, …}  bge-m3 sparse weights
embedding_model   Utf8            model ID that produced this embedding
chunk_index       Int32           0-based; -1 = whole-document metadata row
chunk_total       Int32           total chunks for this doc
chunk_start_char  Int32           byte offset in full_text
chunk_end_char    Int32
indexed_at        Timestamp
source_hash       Utf8            MD5/SHA256 of original file bytes
tags              List<Utf8>
metadata_json     Utf8            forward-compat escape hatch (Internxt zip paths,
                                  batch IDs, session IDs, future location types, …)
```

### Indexes

| Index | Type | Column(s) | Notes |
|---|---|---|---|
| Vector | IVF-PQ | `embedding` | `num_partitions=256`, `num_sub_vectors=128` |
| Full-text | Tantivy (direct) | `full_text`, `headings_text` | separate Tantivy index, see §5 |
| Scalar | B-tree | `owner_id`, `language`, `year` | pre-filter before ANN |

---

## 5. Full-Text Search — Tantivy Direct (not via LanceDB FTS API)

LanceDB has built-in FTS via Tantivy, but its query API is too simplified for advanced
proximity + wildcard queries. We use the `tantivy` crate directly alongside LanceDB.

### Tantivy schema (parallel to LanceDB table)

```
doc_id            TEXT STORED       links back to LanceDB row
headings          TEXT STORED       boosted field (^3 at query time)
body              TEXT              full stripped text, positional index
language          FACET             for per-language filtering
owner_id          TEXT STORED       for multi-user filtering
```

The Tantivy index lives at `{data_dir}/fts/` next to the LanceDB directory at `{data_dir}/lance/`.
Both are written atomically during ingest.

### Query Translator

Parses an advanced query string → Tantivy query tree.

Supported operators:

| Query syntax | Meaning | Tantivy implementation |
|---|---|---|
| `foo AND bar` | both terms | `BooleanQuery::must` |
| `foo OR bar` | either term | `BooleanQuery::should` |
| `NOT foo` | exclude term | `BooleanQuery::must_not` |
| `"foo bar"` | exact phrase | `PhraseQuery(slop=0)` |
| `foo*` | prefix wildcard | prefix scan TermDictionary → `BooleanQuery::should` |
| `fo?` | single-char wildcard | regex on TermDictionary |
| `foo~2` | fuzzy (edit distance) | `FuzzyTermQuery(distance=2)` |
| `foo w/N bar` | within N words, either order | `PhraseQuery([foo,bar], slop=N)` + `PhraseQuery([bar,foo], slop=N)` OR'd |
| `foo pre/N bar` | foo before bar within N words | `PhraseQuery([foo,bar], slop=N)` only |
| `(foo OR bar) w/N baz` | grouped proximity | recursive parse → cross-product slop queries |

**Implementation note on `w/N`**:
Tantivy `PhraseQuery` with slop is *directional*: `["foo","bar"]` with slop N matches
"foo … bar" with up to N intervening tokens. To get bidirectional `w/N` semantics we emit
**two** phrase queries (both orderings) wrapped in `BooleanQuery::should`.
Wildcard expansion happens first via TermDictionary prefix scan, then slop queries are built
for each expanded term pair. The expansion is cached per-query.

```rust
// src-tauri/src/index/fts_query.rs
pub fn translate(query: &str, reader: &IndexReader) -> Result<Box<dyn Query>>;
```

---

## 6. Search Modes

| Mode | What runs | When to use |
|---|---|---|
| **Text only** | Tantivy BM25 | exact terms, author names, theological vocab |
| **Vector only** | LanceDB ANN | semantic / paraphrase / cross-language |
| **Hybrid** | BM25 + ANN → RRF rerank | best recall, default for large corpora |
| **Sparse+Dense** | bge-m3 sparse + dense → rerank | best when embedder is bge-m3 |

Reciprocal Rank Fusion (RRF) for hybrid reranking — simple, parameter-free, robust.

---

## 7. Extraction Pipeline (updated)

```
File drop
  → [existing] text extraction (PDF/DOCX/EPUB/OCR via pdfjs, mammoth, tesseract)
  → [new] produce three outputs:
       full_text_raw    stripped plain text (for embedding)
       full_text_md     Markdown with heading structure preserved
       headings[]       ordered list of section titles
  → chunk(full_text_raw, headings) → chunks[]
  → for each chunk:
       embed(chunk.text) → embedding (dense 1024d) + embedding_sparse
       write to LanceDB (lance/) and Tantivy (fts/)
       location_uri = FileLocation::from_current_context()
  → on Sort step: update_location(doc_id, new_uri)
```

**Why `.md` extraction?**
- Heading boundaries → semantically coherent chunks
- Heading text → boosted FTS field (headings rank higher in BM25)
- Markdown stored as `full_text_md` → rich preview rendering in UI
- Strip markdown syntax before embedding → cleaner vectors

---

## 8. Remote VPS Server

### Technology: Rust + Axum

| Criterion | Rust+Axum | Python FastAPI | Node+LanceDB |
|---|---|---|---|
| Same LanceDB crate | ✓ | ✗ | ✗ (N-API bindings) |
| Same fastembed-rs | ✓ | ✗ | ✗ |
| Static binary | ✓ (musl) | ✗ | ✗ |
| No runtime deps on VPS | ✓ | ✗ (Python env) | ✗ (Node) |
| CPU perf | excellent | good | good |

Compile target: `x86_64-unknown-linux-musl` — fully static, no glibc version concerns.

### REST API

```
POST   /v1/ingest              body: { text, metadata, location_uri } or { embedding[], metadata, location_uri }
POST   /v1/search/text         body: { query, filters, limit }
POST   /v1/search/vector       body: { embedding[], filters, limit }
POST   /v1/search/hybrid       body: { query, embedding[], filters, limit }
DELETE /v1/docs/{doc_id}
PATCH  /v1/docs/{doc_id}/location   body: { location_uri }
GET    /health
GET    /v1/stats               index size, doc count, model info
```

### Authentication

`Authorization: Bearer <api-key>` — HMAC-SHA256 signed token.
Key stored in `.env` on VPS, in Tauri secure store (OS keychain) on client.

### VPS vs local embedding

The server can embed on ingest (from raw text) **or** accept pre-computed vectors
(client embedded locally). Config flag: `server_side_embedding: bool`.
This lets a GPU-equipped client send vectors and the CPU VPS just stores+indexes.

### IndexBackend trait (shared abstraction)

```rust
#[async_trait]
pub trait IndexBackend: Send + Sync {
    async fn ingest(&self, doc: DocumentChunk) -> Result<()>;
    async fn search_text(&self, query: &str, filters: &Filters, limit: usize) -> Result<Vec<SearchResult>>;
    async fn search_vector(&self, emb: &[f32], filters: &Filters, limit: usize) -> Result<Vec<SearchResult>>;
    async fn search_hybrid(&self, query: &str, emb: &[f32], filters: &Filters, limit: usize) -> Result<Vec<SearchResult>>;
    async fn delete_doc(&self, doc_id: &str) -> Result<()>;
    async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()>;
}
```

`LocalIndex` and `RemoteClient` both implement this. The active backend is chosen at
runtime from settings, wrapped in `Arc<dyn IndexBackend>` in `AppState`.

---

## 9. Settings UI

New section "Search Index" in Settings:

```
┌─ Search Index ───────────────────────────────────────────────┐
│  [ ] Enable search index                                      │
│                                                               │
│  Backend       ○ Local    ○ Remote (VPS)                      │
│  Remote URL    [_________________________________]            │
│  API key       [_________________________________] [Test]     │
│                                                               │
│  Search mode   ○ Text only  ○ Vector only  ○ Hybrid           │
│  Embedder      [bge-m3 INT8 ▼]                                │
│  Device        ○ Auto  ○ CPU  ○ Metal (macOS)  ○ CUDA         │
│                                                               │
│  [ Re-index current session ]  [ Rebuild IVF-PQ index ]       │
│  [ Export index stats ]                                       │
└───────────────────────────────────────────────────────────────┘
```

---

## 10. Multi-user Design

- Every ingest call carries `owner_id` (user UUID from app config)
- Every LanceDB row and Tantivy document stores `owner_id`
- Search pre-filters by `owner_id` unless "all users" mode is explicitly enabled
  (admin setting on the VPS server)
- Single-user installs: `owner_id` is auto-populated, never shown in UI
- User registry (`users.json` alongside the index dir) maps `uuid → { display_name, email }`

---

## 11. Cargo Dependencies (additions to `src-tauri/Cargo.toml`)

```toml
# Search / RAG
# lancedb 0.26.2 = latest stable on crates.io. 0.27.0-beta.5 is git-only,
# adds only JS native array inference (irrelevant to us).
lancedb          = { version = "0.26.2", default-features = false }
tantivy          = "0.22"
# fastembed 5.x has real EmbeddingModel::BGEM3 + SparseModel::BGEM3 (multilingual sparse).
# lancedb has NO ort dep → no version conflict.
fastembed        = { version = "5.13.0", features = ["ort-download-binaries-native-tls", "hf-hub-native-tls"] }
arrow            = { version = "57", default-features = false }
arrow-array      = { version = "57", default-features = false }
arrow-schema     = { version = "57", default-features = false }
arrow-select     = { version = "57", default-features = false }
# ort pinned to match fastembed 5.13.0; used directly for CoreML/CUDA EP types.
ort              = "=2.0.0-rc.11"

# Utilities
uuid             = { version = "1", features = ["v4", "serde"] }
async-trait      = "0.1"
serde_json       = "1"    # already present
```

For the VPS server (separate crate `crisp-index-server`):
```toml
axum             = "0.8"
tower            = "0.5"
tower-http       = { version = "0.6", features = ["cors", "auth"] }
tokio            = { version = "1", features = ["full"] }
lancedb          = "0.14"
tantivy          = "0.22"
fastembed        = "4"
hmac             = "0.12"
sha2             = "0.10"
dotenvy          = "0.15"
```

---

## 12. Rust Module Layout (in `src-tauri/src/`)

```
index/
  mod.rs              pub re-exports, IndexBackend trait, AppState integration
  location.rs         FileLocation enum, URI parse/serialize, RetrievalCost
  schema.rs           Arrow schema builder, chunk helper types
  embedder.rs         fastembed-rs wrapper: model enum, device selection, batch embed
  fts_query.rs        advanced query → Tantivy query translator
  fts_index.rs        Tantivy index open/create/write/search
  local_index.rs      LanceDB local: open/create/ingest/search, IVF-PQ build
  remote_client.rs    HTTP client to VPS server (reqwest)
  ingest.rs           orchestration: text → chunks → embed → write both indexes
  search.rs           unified search: dispatch to text/vector/hybrid, RRF merge
```

VPS server (separate workspace member or repo):
```
crisp-index-server/
  src/
    main.rs
    state.rs          SharedState: LanceDB conn + Tantivy index + embedder
    auth.rs           Bearer token HMAC validation
    routes/
      ingest.rs
      search.rs
      delete.rs
      health.rs
      stats.rs
```

---

## 13. Phased Implementation

| Phase | Deliverable | Est. | Status |
|---|---|---|---|
| **P1** | `location.rs` — full URI model with tests | ½ day | ✅ Done |
| **P2** | `fts_query.rs` — advanced query translator with tests | 1 day | ✅ Done |
| **P3** | `embedder.rs` — fastembed-rs wrapper, model enum, device picker | 1 day | ✅ Done |
| **P4** | `fts_index.rs` — Tantivy index CRUD + search | 1 day | ✅ Done |
| **P5** | `local_index.rs` — LanceDB CRUD, IVF-PQ, vector search | 2 days | ✅ Done |
| **P6** | `ingest.rs` — full pipeline: chunk → embed → write | 1 day | ✅ Done |
| **P7** | `search.rs` — unified FTS+vector with RRF reranking | 1 day | ✅ Done |
| **P8** | `tauri_commands.rs` + `crisp-index-server` skeleton + `index_init` command | 2 days | ✅ Done |
| **P9** | Svelte Settings UI — Search Index panel in Settings.svelte | 2 days | ✅ Done |
| **P10** | `remote_client.rs` + remote mode switching in `init_index` | 1 day | ✅ Done |
| **P11** | Sort-step `update_location` hooks in `execute_batch` | ½ day | ✅ Done |
| **P12** | `.md` extraction + heading detection in extractors | 1 day | ✅ Done |
| **P13** | Internxt-zip URI parsing (stub, no retrieval) | ½ day | ✅ Done (P1) |

---

## 14. Session Continuity

### What is fully built (P1–P9 complete, cargo check ✅)

- `src-tauri/src/index/mod.rs` — `IndexBackend` trait, `IndexState` (+`local` field), `IndexConfig`, `SearchMode`, `BackendType`
- `src-tauri/src/index/location.rs` — `FileLocation` URI model (Local/Vps/Internxt/InternxtZip), tests
- `src-tauri/src/index/schema.rs` — Arrow schema, `DocumentChunk`, `SearchResult`, `SearchFilters`
- `src-tauri/src/index/embedder.rs` — `Embedder` (fastembed 5.x), correct model mappings, `chunk_text`
- `src-tauri/src/index/fts_query.rs` — advanced query → Tantivy (AND/OR/NOT/phrase/wildcard/fuzzy/w/N/pre/N)
- `src-tauri/src/index/fts_index.rs` — Tantivy index CRUD + search with owner-filter
- `src-tauri/src/index/local_index.rs` — LanceDB CRUD, IVF-PQ build, `batches_to_search_results_with_scores`
- `src-tauri/src/index/search.rs` — `SearchEngine`: FTS+ANN+RRF(k=60), parallel tokio::spawn
- `src-tauri/src/index/ingest.rs` — `IngestPipeline`: chunk→embed→write, `RawDocument`, `IngestStats`
- `src-tauri/src/index/tauri_commands.rs` — `index_search`, `index_ingest_document`, `index_update_location`,
  `index_build_ivf_pq` (uses `IndexState.local`), `index_get_config`, `index_set_config`, `index_init`
- `src-tauri/src/lib.rs` — `AppState.index`, `get_app_data_dir` command, all 7 index commands registered
- `src/lib/components/Settings.svelte` — Search Index panel: enable toggle, mode/backend/embedder/device selectors,
  remote URL+key, data-dir picker, Apply & Init button, IVF-PQ button, status indicator
- `src/lib/i18n.svelte.ts` — `settings.index.*` keys (en + de)
- `crisp-index-server/` — Axum VPS server skeleton (stub handlers)

### All phases complete — what is built (P1–P12)

All Rust backend and TypeScript frontend code compiles cleanly.

**Remaining work (non-critical):**
1. **crisp-index-server real handlers** — stub Axum routes in `crisp-index-server/` need
   real LanceDB+Tantivy implementations (same logic as `local_index.rs` and `fts_index.rs`).
2. **Frontend: pass `doc_id` + `new_location_uri` in batch execute** — `BatchExecutionItem`
   now accepts optional `doc_id` / `new_location_uri`; the Svelte batch store needs to
   populate these fields when the document was previously indexed (requires a lookup by
   `source_hash` → `doc_id` mapping stored locally).
3. **Frontend: call `index_ingest_document` after extraction** — in `store.svelte.ts` after
   `item.extractedText` is populated, call `invoke('index_ingest_document', {...})` if
   `indexEnabled` is true. Pass `markdownText` and `headings` from the extraction result.
4. **IVF-PQ direct access** — `LocalIndex` already implements `build_vector_index`; it is
   now exposed via `IndexState.local` and the `index_build_ivf_pq` command.
5. **User/machine UUID persistence** — app startup should generate and store UUIDs in
   `settings.json` (`userUuid`, `machineUuid`) and use them to build `crisp+local://` URIs.

### Cargo.toml state (src-tauri)

```toml
lancedb      = { version = "0.26.2", default-features = false }
tantivy      = "0.22"
fastembed    = { version = "5.13.0", features = ["ort-download-binaries-native-tls", "hf-hub-native-tls"] }
ort          = "=2.0.0-rc.11"
arrow        = { version = "57", default-features = false }
arrow-array  = { version = "57", default-features = false }
arrow-schema = { version = "57", default-features = false }
arrow-select = { version = "57", default-features = false }
uuid         = { version = "1", features = ["v4", "serde"] }
async-trait  = "0.1"
```

futures = "0.3" needs to be added (for TryStreamExt when reading LanceDB result streams).

---

## 15. Open Questions / Future Work

- **Chunking for scanned PDFs**: OCR produces flat text; heading detection needs heuristics
  (line length, font-size metadata from pdfjs) rather than Markdown parsing.
- **Cross-language search**: bge-m3 handles de+en in the same vector space, so a German
  query naturally retrieves English documents. Confirm with benchmark on actual corpus.
- **Internxt retrieval**: when a file at `crisp+internxt-zip://` is requested, the retrieval
  pipeline must: authenticate to Internxt → stream zip → extract single member.
  This mirrors `retrieve.py` from the cloud-backup system. Can reuse the same VPS
  as a retrieval gateway (VPS has Internxt credentials, client does not).
- **Index versioning**: when the embedding model changes, re-indexing is required.
  Track `embedding_model` per row → allow mixed-model indexes with per-model ANN subindexes.
- **Sync between local and remote**: local LanceDB can sync a subset (recent / tagged)
  to the VPS index for shared search. Use LanceDB's delta/versioning (Lance format
  is versioned by design) for efficient sync.

---

## Shipped Phases — Archived from PLAN.md

This section preserves the original specs of phases that have shipped.
Kept for context (commit history / review only tells the *what*; these
entries explain the *why* and the design choices that didn't end up in
code comments). For active work, see [PLAN.md](PLAN.md).

### P2 — Search index / RAG (full plan)

The detailed P2 plan — LanceDB schema, dtSearch query translator,
embedder selection, dense + sparse + BM25 + RRF + cross-encoder
reranking — is the §1-§13 archive at the top of this file (originally
`rag_plan.md`). All P1-P12 phases shipped; the §14 "Session Continuity"
notes record the implementation order.

### P3 — Voice chat (CrispASR integration, in-scope items)

ASR via the CrispASR sibling repo (whisper-cpp wrapper exposed through
a C library), TTS via the platform's native synth (`say` on macOS,
SAPI on Windows, `espeak` on Linux), Settings UI for voice picker /
rate / "auto-speak replies" toggle. Hotword/wake-word gating remains
an explicit non-goal for v1 and stays in PLAN.md as a pending item.

### P3.5 Phase 1 — macOS arm64 native-lib bundling

CrispEmbed + CrispASR are shipped as Cargo path-dep wrappers around
`libcrispembed.dylib` / `libcrispasr.dylib`. The post-build script
(`scripts/bundle_macos_native_libs.sh`) copies the cmake-built dylibs
+ ggml backends + transitive libs into `Contents/Frameworks/`,
patches install names with `install_name_tool`, and re-codesigns.
Pattern proven on v0.1.36's .dmg; recipe documented in LEARNINGS.md.
Phases 2 (Linux/Windows) and 3 (mobile) remain pending.

### P6 Phases 1-4 — Catalog / Cathy integration (Catfish port-and-merge)

Brought Catfish's drive-cataloging + duplicate-finding + offline
file-search into CrispSorter as a Tauri-native feature, with byte-exact
read/write of any `.caf` file produced over the past 20+ years
(Cathy 1.x → Catfish v8). The `.caf` binary format spec
(little-endian, magic = `version × 1_000_000_000 + 500_410_407`,
NUL-terminated latin-1 strings, dirs encoded as `size < 0`) was
reverse-engineered from `core/file_index.py` in Catfish.

Phases shipped:

- **Phase 1 — `.caf` I/O + parallel scanner**: `src-tauri/src/catalog/`
  — `caf.rs` (versions 1-8 reader/writer, including v ≤ 6 size quirks),
  `index.rs` (in-memory `FileIndex` with size-bucket HashMap), `scan.rs`
  (rayon-parallel walker via `jwalk`), Tauri commands
  `catalog_load_caf` / `catalog_save_caf` / `catalog_scan_dir` /
  `catalog_metadata`.
- **Phase 2 — Duplicate engine + CLI parity**: `dedup.rs` size-bucket
  fast-path with parallel hash verify (mirroring Catfish's
  `find_all_duplicates_bulk`), generate-deletion-script for bash/batch/
  powershell, JSON output mode matching Catfish's `--output json`.
- **Phase 3 — UI tabs**: `Catalog.svelte` (registry + browse/refresh/
  delete + Active toggle) and `Duplicates.svelte` (source + N
  destinations, hash dropdown, results table, deletion-script export).
  `BatchReview.svelte` gained `exportCaf()` for round-tripping.
- **Phase 4 — Hybrid storage (option C)**: `.caf` is the canonical
  on-disk form; LanceDB has a derived `catalog_entries` table (thin
  schema `(catalog_path, entry_path, size, mtime, hash?)`) populated
  on `set_active(true)`. Cross-links to the existing `documents` table
  via `entry_path`. `catalog_export_sorted` dumps batch slices to a
  fresh `.caf` for archival/sharing.

Phase 5 (extract a `crispcat` workspace crate + standalone CLI) remains
deferred-optional in PLAN.md.

### P7 Phases 7.1-7.6 + 7.8 Tiers 1-2 — Full-volume desktop search

Closed the gap between "smart sort assistant" and "general-purpose
desktop search" by extending each P6 catalog row with extracted text
content + an embedding on a background schedule, plus operator-grade
query syntax, instant preview, saved searches, and cross-mount
awareness.

- **Phase 7.1 — Unified query covering catalogs**: `index_search`
  queries both `documents` and `catalog_entries` in one pass; catalog-
  only hits surface with `catalog_source` set, score=0.4, chunk_index=-1.
- **Phase 7.2 — Operator-grade query syntax**: custom `translate()` in
  `index/fts_query.rs` parses AND/OR/NOT, phrases, w/N + pre/N
  proximity, wildcards, fuzzy, parentheses, plus field-prefix
  (`title:foo`, `body:foo`, `headings:foo` / `h:`, `text:` aliases).
- **Phase 7.3 — Live preview pane**: right-side pane in
  `IndexSearch.svelte` rendering PDF/image/text via `convertFileSrc` +
  `readTextFile`.
- **Phase 7.4 — Background full-content ingest**: per-filetype
  extractor registry (`extractors/{pdf,text,html,ocr,ocr_ocrs}.rs`),
  background ingest scheduler (`bg_ingest/mod.rs` with `tokio::Mutex`-
  guarded queue, `ForegroundGuard` RAII for QoS yielding), mtime-skip
  via `LocalIndex::indexed_mtime_for_uri` parsing `metadata_json`'s
  `{"mtime_unix": v}` shape.
- **Phase 7.5 — Saved searches**: persisted `(query, filters)` tuples
  in `tauri-plugin-store`, surfaced as a dropdown in
  `IndexSearch.svelte`.
- **Phase 7.6 — Cross-mount UUID tagging**: `volume::volume_id_for_path`
  shells out to `diskutil info` (macOS) / `findmnt -no UUID` (Linux) /
  `wmic VolumeSerialNumber` (Windows); id is packed into the existing
  `metadata_json` column alongside `mtime_unix`. New
  `volume_list_mounted` Tauri command.
- **Phase 7.6 follow-up — Search-time availability filter**:
  `index_search` now drops hits whose recorded `volume_id` isn't in
  the currently-mounted set (single shell-out per query). New
  `include_unmounted: Option<bool>` parameter overrides the filter
  for browse / inventory cases. `SearchResult` carries `volume_id`
  through the pipeline (parsed out of `metadata_json` by a new
  hand-parser mirroring `indexed_mtime_for_uri`'s shape — 5 unit
  tests pin its behaviour). UI: a "Inkl. nicht eingehängter
  Laufwerke" checkbox in `IndexSearch.svelte`'s filter row.
- **Phase 7.8 Tier 1 — Tesseract via shell-out** (`bbbca1b`): zero
  binary bloat; user installs Tesseract on demand. Hardcoded
  `eng+deu`. PDFs with empty text layer fall through when `try_ocr`
  is on; image extensions dispatch directly.
- **Phase 7.8 Tier 2 — `ocrs` (pure-Rust RTen engine)**: Apache-2.0,
  CRAFT-shaped models in PyTorch → ONNX, executed via the project's
  RTen runtime (zero system-onnxruntime dep). Adds ~10-20 MB to the
  binary. Latin-script only; German users get a hint to install
  Tesseract for better results.

Tiers 3 (usls PaddleOCR) and 4 (deepseek-ocr.rs VLM, opt-in cargo
feature) remain pending in PLAN.md, along with Phase 7.7 (mountable
`.cidx` archive index files).

### P8.1 — Configurable per-file conversion timeout

New Settings UI knob *Per-file conversion timeout* (default 120 s,
0 = no timeout = pre-P8.1 behaviour). Wraps the whole `extractDocument`
promise with `Promise.race(extract, timer)` in
`src/lib/batch/store.svelte.ts`. Distinct from the page watchdog
(`PAGE_WATCHDOG_MS = 30 s`) — they coexist: page watchdog catches
"extractor froze", total-time timeout catches "extractor making slow
but real progress on a too-big file".

### P8.2 — CLI mode (first cut)

clap-based subcommand router (`src-tauri/src/cli/mod.rs`) with argv
sniff in `main.rs` to route between CLI and GUI modes on a single
binary. Subcommands wired:

* **version** — print app version
* **doctor** — env / model / lib check
* **catalog scan / info / browse / find-dupes / gen-script /
  set-active / search** — all matching the corresponding Tauri commands

JSON Lines is the default output format; `--format text` switches to a
human-readable column view. Stateless subcommands (catalog) work today;
the stateful families (`index` / `batch` / `chat`) need a Tauri-runtime
spinup for `AppState` / `Mutex` / `AtomicUsize` and stay pending.

### Per-version changelog (was PLAN.md scratchpad)

Versioned feature entries that previously lived at the bottom of
PLAN.md. Kept here for the *what-when-why* (commit messages have the
same span but lack the rationale lines).

- **XMP metadata extraction (May 2026, v0.1.35)** — `extract_pdf_metadata`
  now reads the catalog's `/Metadata` stream (XMP RDF/XML) in addition to
  the `/Info` dict. XMP fields win when present (better-curated by
  publisher tooling); `/Info` fills any gaps via the new `merge_in`
  helper. quick-xml-based event walker tracks `dc:title`, `dc:creator`,
  `dc:subject`, `dc:description`, and `xmp:CreateDate`/`ModifyDate`/
  `MetadataDate` — handles the typical RDF wrapping (`Alt`/`Seq`/`Bag` >
  `li`). Multiple creators get joined with `" and "` (BibTeX-friendly
  format). Uses quick-xml's `xml_content()` to decode + unescape XML
  entities in one step. 5 new unit tests cover the dc:Alt/Seq pattern,
  Bag keywords, XMP-with-only-Producer (returns None — no merge needed),
  truncated input resilience, and the merge precedence.
- **Multi-folder watcher (May 2026, v0.1.34)** — extends v0.1.32
  from single-folder to a list. `WatcherState` now holds
  `HashMap<PathBuf, RecommendedWatcher>` keyed by canonical path; one
  shared per-path debounce map across all watchers. Tauri commands:
  `watch_start` (idempotent), `watch_stop_one`, `watch_stop_all`,
  `watch_list`. Settings UI shifts to a list with `+ Add folder` /
  `×` per-row remove. `watchFolders: string[]` setting; on read,
  migrates the v0.1.32 single-folder shape (`watchEnabled` +
  `watchFolder`) to the list, so existing users don't lose their
  setup. `+page.svelte` resume loop calls `watch_start` for each;
  cleanup uses `watch_stop_all`. EN+DE.
- **BibTeX export (May 2026, v0.1.33)** — pure-TS `buildBibFile` in
  `src/lib/export/bibtex.ts`. Citation key = sanitized `{LastName}{Year}`,
  numeric suffix on collisions. Author lastname extracted from "Smith,
  John" or "John Q. Smith"; falls back to "anon". Year regex-matched to
  the first 4-digit substring (handles "2023-03", "approx. 2019"). LaTeX
  special chars (`\ & % $ # _ { } ^ ~`) escaped per the BibTeX spec;
  capitalized words in titles wrapped in `{…}` so case-folding styles
  preserve them. All entries emit as `@misc` (universally accepted; we
  don't have enough metadata to differentiate article/book/report yet).
  Placeholder values (`Unknown Title`, `n/a`, `?`, `-`) are skipped
  rather than emitted as data. Export button in BatchReview header
  next to the source-update button; saves via Tauri dialog with
  `crispsorter-YYYY-MM-DD.bib` default name. Skips items the user
  marked Ignored. EN+DE.
- **Folder watcher v1 (May 2026, v0.1.32)** — drop a file into the
  watched folder and it lands in the batch. New `watcher/` module wraps
  `notify` (FSEvents on macOS, inotify on Linux, `ReadDirectoryChangesW`
  on Windows). `watch_start` / `watch_stop` / `watch_status` Tauri
  commands; single-folder invariant for v1 (multi-folder is future
  work). Per-path 2-second debounce kills the duplicate events common
  to atomic-save patterns. Extension allowlist matches the rest of
  the app (pdf, epub, djvu, txt, md, rtf, doc, docx, odt); editor
  swap files (`.tmp`, `.crdownload`, dotfiles, `~`-suffixed) get
  dropped. Settings UI: folder picker + enable toggle + Apply button.
  `+page.svelte` owns the global `folder-watch:added` listener — calls
  `batchManager.addItem` with path/name/size; `addItem` already dedupes
  on path so retried events stay benign. **No auto-process** in v1:
  files queue up, user still presses Start. The architecture supports
  auto-process as a future toggle — flagged as risky in PLAN P5.
- **PDF metadata pre-fill (May 2026, v0.1.31)** — new
  `extract_pdf_metadata` Tauri command reads the PDF /Info dictionary
  via `lopdf` (already a transitive dep of pdf-extract). Returns title /
  author / subject / keywords / year / producer; year parsed best-effort
  from the `D:YYYYMMDD…` PDF-date format. UTF-16BE-with-BOM and UTF-8
  string decoders handle the most common producer encodings; PDFDocEncoding
  falls back to lossy UTF-8 (covers Title/Author for most European PDFs).
  Frontend extraction phase invokes it on `.pdf` files when the new
  `pdfMetadataPrefill` Settings toggle is on (default true) and pre-fills
  empty `suggestedTitle/Author/Year` slots. The LLM (when enabled) still
  overwrites these in phase 2 — this is purely a fallback for runs where
  the LLM is off or fails. (XMP metadata streams added in v0.1.35.)
  6 new unit tests pin the date parser + string decoder shape.
- **TTS auto-speak for chat replies (May 2026, v0.1.30)** — closes the
  P3 voice loop with zero-dep platform synth. New `tts/mod.rs` shells
  out to macOS `say` / Windows PowerShell SAPI / Linux `spd-say` or
  `espeak` (whichever is on PATH), piping text via stdin so arbitrary
  chat content needs no argv quoting. `tts_speak` and `tts_stop` Tauri
  commands. AppState holds the running child so `tts_stop` (and a fresh
  `tts_speak`) can kill it mid-utterance — no overlapping voices.
  Settings adds an "Auto-speak chat replies" toggle (default off).
  Chat.svelte detects new bot messages via the deep-chat `onMessage`
  delta, strips Markdown/HTML, and pipes plaintext to the synth. Mute
  button appears in the chat header while speaking. The contract is
  identical for a future GGUF Piper/Kokoro sidecar — only the spawn
  function would change.
- **CrispASR voice input — sidecar + push-to-talk (May 2026, v0.1.29)** —
  optional `crispasr` path dep at `../../CrispASR/crispasr` with cargo features
  `crispasr`, `crispasr-metal`, `crispasr-cuda`, `crispasr-vulkan` mirroring
  the CrispEmbed pattern. New `src-tauri/src/asr/mod.rs` wraps `crispasr::Session`
  with auto-download via `cache_ensure_file`. `AsrHandle` is a cheap-clonable
  lazy-load handle on `AppState`. New `asr_transcribe` Tauri command takes
  Float32 PCM 16kHz mono and returns concatenated transcription text.
  `Chat.svelte` has a mic button next to Clear: WebAudio capture →
  OfflineAudioContext resample to 16 kHz → `invoke('asr_transcribe')` →
  `chatElement.submitUserMessage`. Stub-on-feature-off path so users without
  the `crispasr*` feature flag get a clean error toast. CI: release.yml now
  also checks out `CrispStrobe/CrispASR` as a sibling and rewrites the path
  dep, parallel to the existing CrispEmbed handling.
- **Matryoshka dimension selection (May 2026, v0.1.28)** — new
  `IndexConfig.matryoshka_dim: Option<u32>` threads through
  `EmbedderConfig.with_matryoshka_dim` to `CrispEmbedBackend::set_dim` at
  load. `EmbedderConfig::effective_dim()` clamps to the model's nominal
  dim and treats `Some(0)` as `None` (model default). The LanceDB column
  width now uses the effective dim so the schema matches what the embedder
  emits — changing `matryoshka_dim` on an existing index requires
  re-ingestion (warned in the UI hint). UI: number-select (128/256/384/512/768)
  appears under "Inference Backend" only when GGUF is selected and the
  model has a GGUF spec — fastembed has no per-call truncation hook so
  ONNX paths ignore the field. Quality only holds for MRL-trained models
  (BGE-M3, Snowflake Arctic L v2, PIXIE-Rune); the hint flags this.
- **Sparse retrieval + Octen auto-download (May 2026, v0.1.27)** — BGE-M3
  / SPLADE sparse vectors are now used at query time as a 3rd RRF channel
  alongside FTS + dense ANN. `LocalIndex::search_sparse_in_pool` scores the
  union of FTS+ANN candidates by sparse dot product (two-pointer merge for
  sorted indices, hash-join fallback otherwise) and `SearchEngine::maybe_sparse_search`
  fuses the result via the new generalized `rrf_merge_n`. Auto-on when the
  embedder has a sparse head (BGE-M3, BGE-small en-v1.5 with SPLADE++);
  silently skipped otherwise. Octen 0.6B variants (FP32, INT4, INT8-Full)
  switched from local-only `with_local_subdir` to fastembed-native
  auto-download via `cstr/Octen-Embedding-0.6B-ONNX*` HF repos. The
  matMul-only INT8 variant stays local-only (no fastembed equivalent —
  dropped in fastembed-rs 77cc2e45 due to platform-dependent checksums).
- **Configurable model cache dir (May 2026, v0.1.25)** — new
  `IndexConfig.model_cache_dir: Option<String>` + `resolve_model_cache_dir`
  helper picks: `CRISPSORTER_MODEL_CACHE_DIR` env > UI override >
  `{data_dir}/models/`. Single dir is shared by fastembed (ONNX), hf-hub
  (external-data ONNX + GGUF embedder + GGUF reranker), so one setting
  controls every weight on disk. Settings.svelte adds a "Model cache
  directory" picker; an external volume like
  `<external-volume>/ai/crispsorter-models` lets the cache survive app
  re-installs and (partially) share with CrispEmbed CLI. Three unit tests
  pin the resolve precedence.
- **Cross-encoder reranking pipeline (May 2026, v0.1.25)** — new
  `RerankerModel` enum (`BgeRerankerV2M3`, `BgeRerankerBase`,
  `JinaRerankerV2BaseMultilingual`) + `Reranker` wrapper around
  `crispembed::CrispEmbed::rerank` (cross-encoder only; bi-encoder skipped).
  `RerankerHandle` is a cheap-clonable lazy-load handle: GGUF download +
  model open happens on first `score_batch` call. `SearchEngine` now fetches
  `rerank_top_n` candidates (default 50) from FTS / ANN / RRF when a
  reranker is configured, scores each via `score_batch(query, snippets)`,
  and re-sorts; NaN scores fall back to RRF order. `IndexConfig` gains
  `reranker_model: Option<RerankerModel>` + `rerank_top_n: usize`. UI:
  Settings.svelte adds a "Reranker" section between Compute Device and Data
  Directory. GGUF-only — without the `crispembed` cargo feature, `Reranker::load`
  returns a clear error.
- **Pre-existing FTS regression fixed (May 2026)** —
  `index::fts_index::tests::scenario_accent_folding` was failing on `main`
  before any of this branch's edits: query-side `fold_accents` was applied
  but the index used Tantivy's `default` tokenizer (lowercase only), so
  `München` was indexed as `münchen` and never matched the folded query
  `munchen`. Fixed by registering a custom `ascii_folding` tokenizer
  (SimpleTokenizer + RemoveLong + LowerCaser + AsciiFoldingFilter) on the
  index and using it for the title/headings/body fields. Existing FTS dirs
  need re-ingestion — see LEARNINGS.md for the migration note. Also cleaned
  up clippy: `wrong_self_convention` on `to_gguf_spec`/`to_model_spec`
  (`&self` → `self` since `EmbedderModel: Copy`), and explicit
  `#[allow(dead_code)]` on `CrispEmbedBackend` placeholders that future P2
  work will use.
- **Query/passage prefix selection (May 2026)** — auto-apply model-specific
  prefixes via `EmbedderModel::prefix(EmbedRole)`. E5 (`query:` / `passage:`),
  Nomic v1.5 (`search_query:` / `search_document:`), BGE en-v1.5 + Mxbai
  (BGE-style query-only), Jina v5 (`Query:` / `Document:`), EmbeddingGemma
  (task templates). All other models pass through unprefixed. CrispEmbed path
  uses native `set_prefix`; fastembed/OrtPath paths prepend in Rust. Sparse
  encoders (BGE-M3, SPLADE++) untouched — trained without prefixes.
- **CrispEmbed/fastembed-rs registry sync (May 2026)** — added 12 new
  `EmbedderModel` variants (`MultilingualE5{Small,Base,Large}`, `Bge{Small,Base,Large}EnV15`,
  `NomicEmbedTextV15`, `MxbaiEmbedLargeV1`, `AllMiniLmL6V2`, `EmbeddingGemma300M`,
  `Gte{Base,Large}EnV15`). Each wired through both ONNX (native fastembed-rs
  via `CrispStrobe/fastembed-rs@feat/new-model-entries`) and GGUF (CrispEmbed
  `cstr/*-GGUF` registry). `BgeSmallEnV15` paired with `SparseModel::SPLADEPPV1`
  per `HISTORY.md` §2 rationale. Serde kebab-case test pins frontend mapper.
- Stop button — wires `AbortController` through extraction and LLM queries (v0.1.22)
- Per-request LLM timeout — 3 min local / 60 s remote via `Promise.race` (v0.1.22)
- Extraction hang timeout — 5 min auto-abort on `extractionAbort` controller (v0.1.22)
- Frontend log panel — `flog()` store, merged with Rust `app-log` events in LogPanel (v0.1.22)
- Live processing stats in footer — N/total done · extracting X · analyzing Y (v0.1.22)
- Release workflow — auto-publish draft after matrix even if one platform runner is slow (v0.1.22)
- macOS 13 / `crispembed` stub — created minimal stub so CI/dev builds resolve the optional dep
- Stuck items on resume — `resumeLastSession()` resets extracting/analyzing → unfinished (v0.1.23)
- Per-page extraction watchdog — 30 s no-progress timeout replaces flat 5-min timeout (v0.1.23)
- Two-phase batch processing — extract-all then analyze-all; LLM stall never blocks extraction (v0.1.23)
- `unfinished` status — amber badge, filter option, footer counter, resetStuckItems handles it (v0.1.23)
- i18n status strings — all BatchStatus values translated EN + DE; Chat/BatchReview use them (v0.1.23)
- Chat context title/author — shows suggestedTitle + suggestedAuthor for analyzed docs (v0.1.23)
- Stop button during rate-limit wait — `abortableSleep()` makes 429 backoff honour AbortSignal (v0.1.23)
- Rate-limit Retry-After cap — capped at 90 s to prevent 10-min dead waits (v0.1.23)
- Provider round-robin fallback — processAll phase 2 cycles through fallback providers on failure (v0.1.23)
- Round-robin Settings UI — ordered checklist in LLM Options with up/down reorder (v0.1.23)
- Index location update on move — `index_update_location_by_path` Rust command + TS call (v0.1.23)
- i18n audit: Chat.svelte — "Docs:", "Chat:", "Clear Messages" use i18n keys (v0.1.23)

---

## Archived phase specs — 2026-05-09

The following are full design documents for phases that have shipped.
Kept here for "why does this code look this way" context.
See PLAN.md for the current active plan and open items.

### P3 — Voice chat / CrispASR (shipped except hotword/wake)

Full-spec in earlier HISTORY entries. Core shipped: Whisper + CrispASR
backend, ASR UI in Chat panel, TTS, push-to-talk, Rust audio bridge.
Remaining: hotword/wake word (out of scope for v1).

### P4 — Code quality / maintenance (shipped)

Model-cache boot-drive hint, CARGO_TARGET_DIR redirect, i18n audit
(Settings.svelte + LogPanel.svelte ~80 strings). All shipped.

### P5 — Future / planned

Auto-process toggle on watch detection (needs UX design), PWA demo
(File System Access API). Deferred.

### P6 — Catalog / Cathy integration (shipped, Phase 5 deferred)

.caf I/O, parallel scanner, duplicate engine, deletion-script generator,
Catalog/Duplicates UI tabs, hybrid-storage catalog_entries Lance table.
Phase 5 (crispcat workspace crate extraction) optional/deferred.

### P7.1–P7.6 — Full-volume desktop search (shipped)

Unified catalog/documents search, operator-grade query syntax, live
preview pane, background full-content ingest, saved searches, cross-mount
UUID tagging with availability filtering, Tesseract + ocrs OCR (Tiers 1-2).

### P8.1 — Per-file conversion timeout (shipped)

Settings knob conversionTimeoutSeconds, page watchdog in JS extractor.

### P9 — Übersicht at million-file scale (fully shipped)

8 steps: index_query_documents + columnar Übersicht, parent_dir column +
scalar index, folder-tree breadcrumb + index_folder_children, DB-side
ORDER BY via lance::Scanner, column registry + persistence, volume_id
column + scalar index, preview pane, mtime/size/parent_dir metadata.

### P10 — Robust ingest at scale (shipped, minor items remain)

TaskFailureReason enum, extraction timeouts (300s), L2 fallback via
ingest_l2_row, EPUB DRM detection, N-worker bg_ingest, Übersicht failure
badges + retry button, skip non-retryable failures on re-run.
Remaining: DRM help-popover (clickable), CLI --skip-failed (deferred).

### P11 — Remote-server architecture (partially shipped)

Shipped: IndexBackend trait, RemoteClient, crisp-index-server (Axum,
real LanceDB+Tantivy+RRF+IVF-PQ), crisp-index-protocol wire types,
async SQLite job queue (both tiers), batched ingest, embedderLocation
config, local single-writer queue, UI wired to durable job queue,
server-side embedding (SERVER_EMBED=1).
Remaining: server queue blob-size fix (store refs not full embeddings),
IVF-PQ at 100M+, runtime modes enum, cloud drives, SyncManager.

### P12 — cloud-backup integration (L1 shipped, L3 shipped 2026-05-09)

Shipped: index_ingest_cb_manifest (source_files → L1 LanceDB rows,
crisp+cb-archive:// URI scheme), index_promote_cb_archive (retrieve.py
bridge for L3 promotion), CloudDownload button in Übersicht.
Remaining: reverse lookup UI, VPS-trigger indexing hook, global_catalog sync.

### P13 — Image-vertical convergence with CrispLens (future)

CLIP image embedder, face recognition (SCRFD+ArcFace), Images tab.
Deferred until P11 server + sync layer is stable.

### P15 — Batch pre-processing (shipped 2026-05-09)

P15a: content-dedup (size→SHA-256, duplicateGroupId/isDuplicatePrimary,
orange row tint, "Duplikate überspringen" checkbox).
P15b: book-chapter detection (ISBN-13 prefix, fm/001/bm suffix priority,
representative LLM pass only, metadata propagation, edited-volume toggle).


---

## Test sweep — 2026-05-09

Coverage push across recently-shipped surfaces that had no tests:

- **`task_failure.rs`** — 11 tests: classify recognises DRM keywords (encryption.xml,
  ADEPT, FairPlay, AES, drm), distinguishes password from drm, falls back to
  Corrupt, is case-insensitive; `is_retryable()` matches the documented matrix;
  `as_tag()` agrees with serde's `rename_all = "snake_case"` output for every
  variant; `epub_is_drm_protected` returns `false` for missing files / non-zip
  files / clean EPUBs and `true` when `META-INF/encryption.xml` is present
  (built with the `zip::ZipWriter` API).

- **`drives/mod.rs`** — 12 tests: LocalDrive label/type, write→read round-trip,
  parent-dir creation on write, list_dir is sorted alphabetically with size
  metadata, stat for files/dirs, delete for files/dirs, missing-file error,
  DriveRegistry persistence across `open` calls, dedup by id on `add`,
  remove returns found-flag, DriveType serialises snake_case, `instantiate`
  returns LocalDrive for all kinds (Filen/Internxt placeholders).

- **`sync/mod.rs`** — 10 tests: open is idempotent, enqueue returns increasing
  rowids, pending_count excludes max-retried entries, claim_batch respects
  limit + FIFO order, mark_done removes, mark_error increments + records
  message, clear_failed only removes max-retried, sync_state KV round-trip,
  status snapshot is consistent, payload is preserved verbatim.

- **`bg_ingest/mod.rs`** — +6 tests: default OCR settings off, cancel is no-op
  when idle, resume only works when paused, snapshot consistency,
  PendingIngest serde round-trip, `EXTRACTION_TIMEOUT_SECS = 300` sanity guard.

- **`extractors/mod.rs`** — +6 tests: `ExtractOptions::default()` is safe (OCR
  off), `OcrTier::default() == Auto`, `OcrRecLang::default() == Auto`,
  case-insensitive extension dispatch, image extensions excluded from
  `supported()`, no-OCR + image errors, dispatch lowercases extensions.

- **`index/location.rs`** — +5 tests for the new `CbArchive` URI variant: format
  starts with `crisp+cb-archive://`, filename extracted from path,
  retrieval_cost == Expensive, user_id falls back to `Uuid::nil()`,
  spaces in path get %20-encoded.

- **`index/mod.rs`** — +5 tests: `BackendType` serialises to "local"/"remote"/
  "hybrid" (the persisted strings), defaults to Local, `SearchMode` round-trips
  + defaults to Hybrid, `EmbedderLocation` defaults to Client, `IndexConfig`
  defaults are safe (disabled, vector on, no remote URL).

Result: **195 tauri-app + 20 crispcat = 215 unit tests passing**, 0 failed.
