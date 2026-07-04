# CrispSorter v0.10.0 — Performance, Zoned OCR, OMR, Cloud Connectors

**Release date:** 2026-07-04

## Highlights

- **~30 performance optimizations** across search, ingest, LanceDB I/O,
  dependencies, and frontend bundle — measurably faster searches,
  ingestion, and app startup
- **Zoned OCR templates** (P26.4) — define named extraction zones on a
  reference page, apply them to batches of same-format documents
- **Checkmark / OMR detection** (P27.8) — detect filled checkboxes in
  scanned forms via Otsu-thresholded fill-ratio analysis
- **OneDrive + Google Drive connectors** (P27.11) — cloud storage via
  Microsoft Graph API and Google Drive API v3
- **1034 unit tests** (up from 961 in v0.9.1 — 73 new)
- **0 compiler warnings**

## Performance (P28)

### Search pipeline
- VecDeque LRU cache with O(1) eviction + direct field hashing
  (eliminates JSON serialization per query)
- Zero-copy RRF merge across all 4 channels (FTS + dense + sparse + omni)
- Allocation-free operator detection in tokenizer, fuzzify, and synonyms
- `truncate_str` helper replaces `chars().take(N).collect()` at 5 hot paths
- Column projection on all 6 LanceDB query paths (browse, search,
  similarity, sparse, chunk hydration) — excludes 3 embedding vectors +
  large blobs from every query

### Ingest pipeline
- Parallel ViT + Omni image embeddings (~2x wall time for dual-model)
- O(N) chunk_text word scanner (was O(N²))
- Cached Arrow schema in LocalIndex (no rebuild per document)
- Conditional texts.clone() (skip when ColBERT inactive)
- Single embedder lock, single fs::metadata call per file
- LanceDB write batch size raised from 128 to 512 rows
- Deferred to_lowercase() in doctype classifier
- Zero-alloc LID text sampling

### Dependencies & build
- tokio: `"full"` → 7 specific features (drops net, signal)
- symphonia: `"all"` → used codecs only (drops adpcm, mp1, mp2)
- Removed unused `similar "unicode"` feature + duplicate `futures-util`
- Cargo profiles: `opt-level=1` for deps in dev, `lto="thin"` in release

### Frontend
- Vite vendor chunk splitting (7 heavy deps: pdfjs, mammoth, tesseract,
  katex, deep-chat, web-llm, HF transformers)
- Dynamic `import()` for all 5 extractors + WebLLM
- NL query parser: 5x fewer to_lowercase() calls, single-pass whitespace

## New features

### P26.4 — Zoned OCR / Template matching
- **Template store** (`templates.db`): create named templates with zones
  (normalised 0.0–1.0 coordinates for DPI independence)
- **Zone extraction engine**: crop each zone, OCR the crop, return
  structured `{label, text}` pairs
- **Zone types**: `"text"` (OCR) or `"checkbox"` (OMR detection)
- **CLI**: `crispsorter zone --template NAME FILE`
- **Tauri commands**: template_create, template_add_zone, template_list,
  template_get, template_delete, template_apply
- 12 unit tests

### P27.8 — Checkmark / OMR (Optical Mark Recognition)
- **Otsu-thresholded fill-ratio detection** for checkboxes in scanned
  forms — no external CV dependency, uses the `image` crate
- Crops the candidate region, converts to grayscale, runs Otsu
  binarisation, counts dark pixels → `fill_ratio`
- Integrated with P26.4 templates: checkbox-type zones return
  `"true"/"false"` instead of OCR text
- **Tauri command**: `omr_detect`
- 8 unit tests

### P27.11 — Cloud storage connectors
- **OneDrive**: Microsoft Graph API v1.0 — list_dir, read_file,
  write_file, delete, stat. OAuth2 access token auth.
- **Google Drive**: Drive API v3 — path-to-ID resolution by folder
  hierarchy walk, list/read/write/delete/stat. Multipart upload.
- Both implement the `CloudDrive` trait and register in `DriveRegistry`
- `DriveConfig` gains `access_token`, `refresh_token`, `client_id`,
  `client_secret` fields (backwards-compatible via serde defaults)
- 8 unit tests
- OAuth webview flow + Settings UI deferred to follow-up

## Quality

- **0 compiler warnings** (down from 9 in v0.9.1)
- **73 new unit tests** covering:
  - All performance optimizations (cache, RRF, operators, snippets,
    batch size, truncation)
  - Edge cases in comparison, doctype, auto_file, nl_query, annotations,
    retention, versioning, eml, export
  - All new features (templates, zone OCR, OMR, OneDrive, Google Drive)
- Performance patterns documented in LEARNINGS.md

## Breaking changes

None — all changes are additive. Existing indexes, settings, and
`drives.json` files are fully backwards-compatible.

## Test plan

- [ ] Full `cargo test --lib --features desktop` — 1034 pass
- [ ] `npx svelte-check` — 0 errors
- [ ] CI green on all commits
- [ ] Manual: browse a 1000+ document index (column projection perf)
- [ ] Manual: create a template, add zones, apply to a scanned form
- [ ] Manual: OMR detect on a checkbox image
