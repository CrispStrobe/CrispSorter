# OCR workflow

CrispSorter's OCR is a configurable **pipeline**, not a single model call. This
doc explains how the stages fit together, how to drive it (GUI, CLI, ingest),
and where the work happens (CrispSorter = thin Rust caller + settings surface;
**CrispEmbed** = the C++/ggml engines + renderers).

> Everything OCR-related is gated behind `--features crispembed` (the ML lives
> in CrispEmbed). Scanned-PDF rasterization additionally needs
> `--features pdf-render` (PDFium, bound at runtime). Without those, OCR falls
> back to the legacy Rust tier ladder (ocrs / PaddleOCR / Tesseract shell-out).

## The pipeline, end to end

```
file (image / PDF / TIFF)
  │
  ├─ page sourcing  ............ multi-frame TIFF → N PNGs;  scanned PDF → N rasterized pages (PDFium)
  │                              single image → itself                      [extractors/page_source.rs]
  ▼  (per page)
  ├─ source-type router ........ classify screenshot / scanned-doc / photo → pick the recipe (chain)
  ├─ cleanup (pre-process) ..... deskew · crop borders · whiten bg · binarize (Otsu/Sauvola) · NAFNet denoise
  ├─ engine (detect+recognize) . one of 7 engines (see below)
  ├─ accept-gate ............... enough chars + mean confidence? → accept, else escalate to the next stage
  │                              (a "chain" is an ordered list of stages per source type)
  ├─ layout pass (optional) .... RT-DETRv2 regions → column-aware reading order → OCR each region
  │                              text→engine, formula→math-OCR, figure/table skipped, header/footer optional
  └─ post-process (optional) ... punctuation / spacing / truecasing restore (FireRedPunc / PCS)
  ▼
pages joined (form-feed separator) → text  ·  or rendered to hOCR / ALTO / searchable PDF
```

The orchestration (router, cleanup-per-stage, accept-gate, escalation) is a C++
module in CrispEmbed (`ocr_orchestrator`); CrispSorter calls it via the
`crispembed` Rust binding and adds the page-sourcing + output glue.

## Engines

Selectable per stage (`--engine`, or per-stage in the GUI builder):

| Engine | Kind | Notes |
|--------|------|-------|
| `dbnet_trocr` | DBNet detect + TrOCR recognize | default; lightweight |
| `surya` | Surya-OCR-2 | 91-language detection |
| `tesseract` | DBNet detect + Tesseract-LSTM recognize | per-language GGUF (`tesseract-eng`, …) |
| `got` | GOT-OCR2 | VLM, single-shot |
| `glm` | GLM-OCR | VLM |
| `qwen2vl` | Qwen2.5-VL | VLM, strong on German |
| `internvl2` | InternVL2 | VLM |

`dbnet_trocr` runs in "simple mode"; any other engine is built as an explicit
single stage so the choice takes effect.

## Cleanup (pre-processors)

Per-stage classical cleanup + a learned denoise tier:
`deskew`, `crop_borders`, `whiten_background`, `binarize` (Otsu or Sauvola with
`sauvola_k`/`sauvola_window`), `morph_kernel`, `border_threshold`,
`deskew_max_angle`, and **NAFNet** denoise (`--denoise`, downloads ~30 MB on
first use). The router's defaults: scanned-doc binarizes, photo denoises.

## Post-processors

`--punct-model` runs the joined text through a punctuation/spacing/truecasing
model (FireRedPunc / PCS). Useful for engines (e.g. Tesseract-LSTM) whose raw
output needs cleanup.

## Multi-page

`extractors/page_source.rs` turns a document into one image per page:
- **TIFF** — multi-frame split via the pure-Rust `tiff` crate (Gray8/RGB8/RGBA8).
- **PDF** — `--features pdf-render`; PDFium renders each page at ~200 DPI. The
  libpdfium is bundled in releases (`resources/bin/`) and located at runtime
  relative to the executable; if absent, OCR soft-falls-back to the legacy path.
- single images pass through unchanged.

Pages are OCR'd in order and joined with a form-feed (`\f`) page separator.

## Structured / searchable output

Region boxes + text + confidence flow into CrispEmbed's `ocr_render` renderers
(`crispembed::ocr_render_pages`, binary-safe + multi-page):

- `text` — plain text (Rust).
- `hocr` — hOCR XHTML (`ocr_page`/`ocr_line`/`ocrx_word` + bounding boxes).
- `alto` — ALTO 3.1 XML.
- `pdf` — **searchable PDF**: the page image with an invisible, positioned text
  layer (select/copy/search works). Add `--pdfa` for **PDF/A-2b** archival
  output (XMP conformance metadata + sRGB OutputIntent).

Rendering stays in C++ (the "keep it in cpp" rule) — CrispSorter does not
reimplement hOCR/ALTO/PDF in Rust.

## How to drive it

### CLI — ad-hoc (`crispsorter ocr`)

```bash
crispsorter ocr scan.pdf                               # plain text → stdout
crispsorter ocr scan.png --engine tesseract --source-type scanned_doc \
    --denoise --punct-model fireredpunc
crispsorter ocr paper.pdf --render hocr                # → stdout
crispsorter ocr paper.pdf --render alto --out paper.xml
crispsorter ocr paper.pdf --render pdf  --out paper-searchable.pdf   # binary → --out required
crispsorter ocr paper.pdf --render pdf --pdfa --out paper-archival.pdf   # PDF/A-2b archival
crispsorter ocr photo.jpg --layout --drop-headers-footers
crispsorter ocr --help                                 # full flag list
```

Flags map onto the pipeline: primary (`--engine`, `--det-model`, `--rec-model`),
pre-processors (`--cleanup`, `--denoise`/`--nafnet-model`, `--layout`/
`--layout-threshold`/`--drop-headers-footers`), post-processor
(`--punct-model`), accept-gate (`--min-chars`, `--min-confidence`),
routing (`--source-type`), and output (`--render`, `--out`). `crispsorter ocr`
forces OCR even on PDFs that already have a text layer (explicit request).

> The output format flag is `--render` (not `--format`) — `-f/--format` is the
> reserved global JSON/text envelope selector.

### GUI — Settings → Smart OCR Pipeline

A master toggle plus: source-type router, cleanup preset + the classical knobs,
NAFNet denoise, accept-gate thresholds, a post-OCR punct model, the layout pass
(threshold + drop-headers/footers), and an **advanced per-stage builder**
(add/remove/reorder stages; pick engine + models + cleanup + params + gate per
stage). Persisted via the `bg_ingest_set_ocr_pipeline` command and applied to
the background ingest worker.

### Ingest — automatic during indexing

`crispsorter index ingest` (and the GUI drop zones) run OCR on image extensions
and on PDFs whose text layer is empty, when image extraction is at level **L3**
(`Settings → Multimodal processing`). The persisted Smart OCR Pipeline config is
honored; otherwise the legacy tier ladder runs.

## Feature flags

| Flag | Enables |
|------|---------|
| `crispembed` (`-metal`/`-cuda`/`-vulkan`) | the smart pipeline, all 7 engines, layout, renderers |
| `pdf-render` | scanned-PDF rasterization (PDFium) |
| `paddle-ocr` | legacy Tier-3 PaddleOCR fallback |

## Where the code lives

| Concern | Location |
|---------|----------|
| Page sourcing (TIFF/PDF → pages) | `src-tauri/src/extractors/page_source.rs` |
| Pipeline call + region extraction | `src-tauri/src/extractors/ocr_crispembed.rs` |
| Output formats / renderers | `src-tauri/src/extractors/ocr_render.rs` |
| Layout pass | `src-tauri/src/extractors/layout.rs` |
| Dispatch + config | `src-tauri/src/extractors/mod.rs` (`OcrPipelineConfig`) |
| CLI `ocr` command | `src-tauri/src/cli/mod.rs` (`cmd_ocr`) |
| GUI | `src/lib/components/Settings.svelte` (Smart OCR Pipeline panel) |
| C++ orchestrator + engines + renderers | CrispEmbed (`ocr_orchestrator`, `ocr_render`, …) |
