# CrispSorter v0.7.0

## Highlights

**Search UX overhaul.** Sixteen new features bring CrispSorter's search and
document management to parity with professional desktop-search tools and
enterprise DMS suites. The search bar gains saved searches, search history,
fuzzy matching, natural-language filter parsing, progressive result refinement,
and a "find similar" button. A new Corpus Dashboard shows at-a-glance corpus
statistics with an interactive document timeline.

**Enterprise DMS features.** Document status tracking (pending/approved/rejected),
batch field extraction to CSV, table-to-CSV export, and barcode/ISBN/ISSN
detection at ingest time close the gap with enterprise document management
systems.

**Email indexing.** Native `.eml` and `.mbox` extraction parses RFC 822 headers
into structured metadata (From -> author, Subject -> title, Date -> year,
List-Id -> tag) and handles multipart MIME (text/plain preferred, HTML
fallback with tag stripping).

**Performance.** A 32-entry LRU result cache with generation-based invalidation
eliminates redundant embedding + search for repeated queries. Cache hits skip
the entire FTS + ANN + RRF pipeline and return instantly.

---

## New features

### P22 — Search UX & Discovery

#### P22.1 — Saved searches
Persist named query+filter combos via the Tauri plugin-store. Bookmark button
in the search bar saves the current query, mode, and all filters. Saved
searches appear in a dropdown — one click re-runs.

#### P22.2 — "More Like This" (similar-document discovery)
`index_find_similar(doc_id, chunk_index, limit)` — looks up a document's
dense embedding from LanceDB, runs ANN excluding the source document, returns
semantically similar documents. The "≈" button on every search result card
replaces results with "Similar to: <title>" view.

#### P22.3 — Auto-summary at ingest
Schema migration **v110** adds a `summary` column. During ingest, the first
2-3 sentences are extracted as a clean summary (sentence-boundary splitting,
word-boundary truncation). Summaries display as an italic, blue-bordered block
above the BM25 snippet on each search result card.

#### P22.4 — Natural-language → Filters
Deterministic heuristic parser (`index/nl_query.rs`) extracts structured
intent from plain-text queries:
- Year ranges: `from 2023`, `2020-2024`, `since/before/ab/bis/vor`
- Languages: `in German`, `auf Deutsch` (19 languages, EN+DE)
- File types: `pdf files`, `.docx`, `PDFs`
- Folder prefixes: `in /path/to/...`
- Tags: `tagged X`, `tag:X`
- URL domains: `from spiegel.de`

"Smart" checkbox toggle in the search bar enables auto-parsing before dispatch.

#### P22.5 — Corpus Dashboard
`index_corpus_stats` Tauri command returns aggregate statistics: total docs,
total chunks, extension distribution, language distribution, year histogram,
top tags, and total indexed size. New `CorpusDashboard.svelte` component
accessible from the Catalog tab's "Dashboard" sub-tab. Summary cards +
bar/pie charts rendered in pure CSS (no chart library).

### P23 — Search Power Features

#### P23.1 — Fuzzy / typo-tolerant search
`fuzzify_query()` in `fts_query.rs` auto-rewrites bare word terms with `~1`
(1 edit distance). Skips phrases, operators, wildcards, existing fuzzy markers,
pure numbers, and words shorter than 4 characters. New `SearchFilters.fuzzy`
boolean. "Fuzzy (typo-tolerant)" checkbox in the advanced filters panel.
Especially valuable for OCR-heavy corpora where recognition errors are common.

#### P23.2 — Search-within-results (progressive refinement)
New `SearchFilters.doc_id_scope: Vec<String>` restricts search to a specified
set of doc_ids. "Refine" button appears when 2+ results are displayed — clicking
it captures current result doc_ids as a scope, clears the query for the user to
type a narrowing query, and runs the next search scoped to those documents.
Scope badge with clear button shows when active.

#### P23.3 — Email (.eml) extraction
New `extractors/eml.rs` module. Parses RFC 822 headers (From -> author,
Subject -> title, Date -> year, List-Id -> tag) and body (text/plain preferred,
text/html with tag-stripping fallback, multipart MIME boundary splitting).
Registered in `supported()` and the extension dispatch.

#### P23.4 — Interactive document timeline
The Corpus Dashboard's year histogram is now a full-width interactive timeline
with clickable bars. Selecting a year highlights the bar and shows a filter
hint. Pure CSS, no chart library.

#### P23.5 — Search result LRU cache
`index/result_cache.rs`: 32-entry LRU cache keyed on `(query, mode,
filters_hash)` with generation-based invalidation (global `AtomicU64` bumped
on every `ingest_batch`). Wired into `SearchEngine.search_hybrid` — cache-hit
skips embedding + FTS + ANN entirely, returning cloned results.

### P24 — Search history

#### P24.2 — Search history panel
Persists the last 50 queries in the Tauri plugin-store. Clock icon in the
search bar opens a dropdown with recent queries showing mode, result count,
and timestamp. One-click re-run, per-entry delete, and "Clear all" button.
Deduplicates on (query, mode) — re-running bumps to the top.

### P25 — DMS & compliance features

#### P25.5 — Barcode / QR code detection at ingest
`index/barcode.rs` scans document text for patterns matching common barcode
formats: EAN-13 (with check digit validation), ISBN-13, UPC-A, and ISSN.
Detected values are stored as `barcode:ean13:...` / `barcode:isbn13:...` /
`barcode:upca:...` / `barcode:issn:...` tags — visible in the tag cloud,
filterable via `--tag barcode:isbn13:978...`.

#### P25.6 — Batch KIE -> CSV export
`tool_batch_kie(folder, labels, threshold)` Tauri command runs key-information
extraction (GLiNER NER or LiLT layout-aware) across all supported files in a
folder. Returns one row per file with extracted field values, plus a ready-to-save
CSV string with the label schema as column headers.

#### P25.10 — .mbox email extraction
`eml::extract_mbox(path)` splits an mbox file on `From ` envelope lines,
parses each message through the RFC 822 extractor, and merges all bodies into
a single document with `── Message N ──` separators. Tags from all messages
are deduplicated.

### P26 — Enterprise DMS parity

#### P26.3 — Table -> CSV export
`tool_table_to_csv(html)` Tauri command converts the HTML table output from
SLANet table detection into RFC 4180 CSV. Properly handles comma-containing
cells (quoted), embedded quotes (doubled), and multi-row tables.

#### P26.8 — Document status / review workflow
Schema migration **v111** adds a `doc_status` column. Values: `pending_review`,
`approved`, `rejected`, `archived`, or NULL. `index_set_doc_status(doc_id,
status)` Tauri command updates via LanceDB `UPDATE ... SET doc_status = ...`.
New `SearchFilters.doc_status` filter. Foundation for solo/small-team review
workflows without the complexity of multi-user approval chains.

---

## Schema migrations

| Version | Column | Type | Purpose |
|---------|--------|------|---------|
| v110 | `summary` | Utf8 | Extractive summary (first 2-3 sentences) |
| v111 | `doc_status` | Utf8 | Document review status |

Both migrations are idempotent and backfill existing rows with NULL.

---

## New Rust modules

| Module | Lines | Purpose |
|--------|-------|---------|
| `index/summary.rs` | ~120 | Extractive summary generation |
| `index/nl_query.rs` | ~200 | Natural-language query parser |
| `index/result_cache.rs` | ~100 | LRU result cache with generation invalidation |
| `index/barcode.rs` | ~180 | Barcode/ISBN/ISSN pattern detection |
| `extractors/eml.rs` | ~450 | RFC 822 .eml + .mbox email extraction |

## New Svelte components

| Component | Purpose |
|-----------|---------|
| `CorpusDashboard.svelte` | Corpus stats grid + distribution charts + timeline |

---

## Test coverage

34 new unit tests across 8 modules (767 total, 0 failures):

| Module | New tests | Coverage |
|--------|-----------|----------|
| summary.rs | 6 | Extraction, truncation, sentence boundaries |
| nl_query.rs | 11 | All filter types, German, mixed, passthrough |
| result_cache.rs | 5 | Hit/miss, invalidation, eviction, mode/filter isolation |
| barcode.rs | 7 | EAN-13, ISBN-13, UPC-A, ISSN, rejection, cap, dedup |
| fts_query.rs | 4 | Fuzzify: bare words, field prefix, parentheses, empty |
| eml.rs | 6 | Plain, HTML strip, mbox split, date, list-id, multipart |
| local_index.rs | 4 | find_similar, corpus_stats, doc_status, doc_id_scope |
| tauri_commands.rs | 2 | HTML table -> CSV basic + escaping |

---

## Breaking changes

None. All new columns are nullable, all new Tauri command parameters are
`Option<>`, and all new frontend features are opt-in (toggles, buttons).
Existing indexes are migrated transparently on first open.
