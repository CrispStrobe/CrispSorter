# CrispSorter v0.9.1 — Release Notes

## Highlights

- **Full feature wiring** — all backend features now accessible from CLI + GUI
- **Document-type classification** — automatic at ingest (18 types)
- **CrispEmbed scan cleanup** — despeckle, blackfilter, page splitting, auto-crop
- **164 new unit tests** (954 total, up from 790)
- **Android build fix** — resolved TOML section ordering bug

---

## New Features

### Document-Type Classification at Ingest (P26.1)

Every ingested document is now automatically classified into one of 18
document types: letter, invoice, receipt, form, email, report,
specification, presentation, spreadsheet, image, audio, video, ebook,
code, article, contract, memo, or unknown.

Classification uses a heuristic approach based on file extension and
text content pattern matching (invoice/receipt keywords, contract
signals, letter/memo markers, form fields, report structure).  The
result is stored as a `doctype:<class>` tag, visible in the tag cloud,
searchable via `--tag doctype:invoice`, and filterable in the advanced
search panel.

### Scan Cleanup Improvements (CrispEmbed Integration)

Integrated 107 new CrispEmbed commits including four new scan cleanup
operations:

| Feature | Description |
|---------|-------------|
| **Despeckle** | Remove salt-and-pepper noise from scanned documents |
| **Blackfilter** | Remove black borders and edges from scans |
| **Page split** | Detect two-up book spreads, return gutter position |
| **Content bbox** | Detect printed content area, trim blank margins |

Available as `OcrCleanupSpec` toggles (Settings → Smart OCR Pipeline)
and standalone Tauri commands (`ocr_detect_page_split`,
`ocr_content_bbox`).

Also picked up automatically via the CrispEmbed model registry:
- IQ4_XS/IQ4_NL quantization for all embedders (better quality at smaller size)
- imatrix quantization for rerankers, NER, ColBERT, SPLADE
- 6 engine fixes (LFM2-ColBERT CUDA, DeBERTa reranker, BERT-NER,
  SPLADE sparse, DAT/TBSRN super-resolution)

### Complete Feature Wiring

All backend features from v0.9.0 are now fully accessible:

**New CLI subcommands** (`crispsorter index`):
- `versions` — show document version history
- `audit-log` — query the audit trail
- `retention-rules` / `retention-add` — manage retention policies
- `compare` — word-level diff between two documents
- `entity-graph` — build NER entity co-occurrence graph
- `feed` — fetch and parse RSS/Atom feeds
- `export` — export document text to DOCX or HTML

**New GUI panels:**
- **Settings → Audit Log** — query and browse the audit trail
- **Settings → Retention** — create, enable/disable, delete rules
- **Settings → RSS Feeds** — fetch and preview feed entries
- **Dashboard → Entity Graph** — build and view NER co-occurrence
- **Search results** — Export (⤓) and Highlight (★) buttons on every result
- **PDF Tools** — Detect Signatures and PDF/A conversion buttons

**Backend wiring:**
- Audit trail auto-logs search queries, document deletions, and ingests
- Document-type classification runs automatically at ingest time

## Improvements

### Test Suite Expansion

164 new unit tests across all modules (954 total):

| Module | Tests | Coverage |
|--------|-------|----------|
| pdf_ops | 59 | All 18 operations + edge cases |
| CLI parse | 14 | parse_page_spec, parse_split_ranges |
| doctype | 17 | All 18 types + German + edge cases |
| clustering | 12 | K-means++ edge cases |
| synonyms | 15 | Operators, wildcards, bidirectional |
| comparison | 11 | Ratios, long text, whitespace |
| annotations | 8 | CRUD, multi-page, pagination |
| versioning | 7 | Lifecycle, determinism, edge cases |
| retention | 7 | Rules, conflicts, timestamps |
| audit | 10 | Concurrent writes, persistence |
| feed | 17 | RSS2, Atom, HTML strip, edge cases |
| export | 5 | DOCX, HTML, escaping |

### Android Build Fix

Fixed a critical Android build failure caused by a misplaced TOML
`[target.'cfg(not(android/ios))'.dependencies]` section that
accidentally excluded all subsequent dependencies (rusqlite, sha2,
image, etc.) from the Android build.  Also gated `arboard` (clipboard),
`feed-rs`, and `docx-rs` behind the `desktop` feature since they don't
compile on Android NDK.

### CrispASR Sync

Pulled 23 new CrispASR commits: imatrix quantization, Mimi codec
causal default (WER improvement), CPU weight-read hardening,
VoiceVibe/MOSS fixes, cross-platform build fixes.

## Breaking Changes

None.

## Dependency Changes

| Crate | Version | Purpose | Gate |
|-------|---------|---------|------|
| `feed-rs` | 2 | RSS/Atom parsing | `desktop` |
| `arboard` | 3 | Clipboard access | `not(android/ios)` |
| `similar` | 2 | Text diffing | unconditional |
| `docx-rs` | 0.4 | DOCX generation | `desktop` |

## Platform Support

| Platform | Status |
|----------|--------|
| macOS (arm64) | ✓ |
| Linux (x86_64) | ✓ |
| Windows (x86_64) | ✓ |
| Android (aarch64) | ✓ |
| iOS (arm64) | ✓ |

## Statistics

- **107 features shipped** (19 remaining — blocked or complex)
- **954 unit tests** (0 failures)
- **0 TypeScript errors**
- All sibling repos synced (CrispEmbed, CrispASR, crisp-docx)
