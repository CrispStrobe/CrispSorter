# OCR workflow

For document review, word evidence, full-passage corrections, and partial OCR
with a before/after preview, see [OCR document workspace](ocr-workspace.md).
For library task presets and recovery, see
[Library workflows](library-workflows.md#ocr-recovery). The bundled runtime is
pinned to CrispEmbed **0.17.16** in
[`scripts/crispembed-runtime-lock.json`](../scripts/crispembed-runtime-lock.json).
Available operations depend on the installed build and models; an engine name
in configuration is not evidence that it has been qualified on a document type.
The remaining work and its acceptance gates are in the
[OCR roadmap](ocr-roadmap.md).

CrispSorter's OCR is a configurable **pipeline**, not a single model call. This
doc explains how the stages fit together, how to drive it (GUI, CLI, ingest),
and where the work happens (CrispSorter = thin Rust caller + settings surface;
**CrispEmbed** = the C++/ggml engines + renderers).

> The Smart OCR Pipeline and native document models require
> `--features crispembed`. Scanned-PDF rasterization additionally needs
> `--features pdf-render` (PDFium, bound at runtime). Legacy extraction paths can
> use the Rust tier ladder (ocrs / PaddleOCR / Tesseract shell-out). The review
> workspace reports its actual capabilities: project editing and text exports
> remain available in builds without native recognition. Checked searchable PDF
> output has an additional renderer gate described below.

## Interactive document review

The **OCR** tab now provides a durable multi-document review workspace. See
[OCR document workspace](ocr-workspace.md) for page preparation, region editing,
confidence review, batch controls, structured extraction, and checked export.

## The pipeline, end to end

```
file (image / PDF / TIFF)
  │
  ├─ page sourcing  ............ multi-frame TIFF → N PNGs;  scanned PDF → N rasterized pages (PDFium)
  │                              single image → itself                      [extractors/page_source.rs]
  ▼  (per page)
  ├─ source-type router ........ classify screenshot / scanned-doc / photo → pick the recipe (chain)
  ├─ cleanup (pre-process) ..... deskew · crop borders · whiten bg · binarize (Otsu/Sauvola) · NAFNet denoise
  ├─ engine (detect+recognize) . an available engine (see below)
  ├─ accept-gate ............... enough chars + mean confidence? → accept, else escalate to the next stage
  │                              (a "chain" is an ordered list of stages per source type)
  ├─ layout pass (optional) .... regions (RT-DETRv2 semantic, or `cc` model-free CC detector) →
  │                              column-aware reading order → OCR each; figure/table skipped, header/footer optional
  └─ post-process (optional) ... punctuation / spacing / truecasing restore (FireRedPunc / PCS)
  ▼
pages joined (form-feed separator) → text  ·  or rendered to hOCR / ALTO / searchable PDF
```

The orchestration (router, cleanup-per-stage, accept-gate, escalation) is a C++
module in CrispEmbed (`ocr_orchestrator`); CrispSorter calls it via the
`crispembed` Rust binding and adds the page-sourcing + output glue.

## Engines

Engine selection is available through CLI `--engine` and the GUI stage builder.
Common engine IDs include:

| Engine | Kind | Notes |
|--------|------|-------|
| `dbnet_trocr` | DBNet detect + TrOCR recognize | default; lightweight |
| `surya` | Surya-OCR-2 | 91-language detection |
| `tesseract` | DBNet detect + Tesseract-LSTM recognize | per-language GGUF (`tesseract-eng`, …) |
| `got` | GOT-OCR2 | VLM, single-shot |
| `glm` | GLM-OCR | VLM |
| `qwen2vl` | Qwen2.5-VL | VLM |
| `internvl2` | InternVL2 | VLM |

The CLI also declares `parseq`, `deepseek_ocr2`, `pix2struct`, `granite_vision`,
`lightonocr`, `qwen3vl`, `unlimited_ocr`, `unlimited_ocr2`, and `minicpm_ocr`.
The review workspace has its own capability list and also offers engines such
as `tesseract_fraktur`, `ppocrv6`, `easyocr`, `olmocr`, and `lfm2_vl` when native
recognition is built in. These lists are intentionally not interchangeable:
check `crispsorter ocr --help` for the CLI and the workspace's available engine
choices for interactive review. Model installation and measured suitability are
separate from accepting an engine ID. See
[`ocr_workspace_capabilities`](../src-tauri/src/ocr_workspace.rs) and
[model evaluation](library-model-evaluation.md).

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

## Scan restoration (dewarp → deblur → super-resolve)

For degraded scans, an optional pre-OCR chain runs before recognition (all
compute in C++; CrispSorter decides *when*):

1. **Dewarp** (`--dewarp`, `crispembed::dewarp`) — straighten curved/warped text
   lines (photos of pages, book spines).
2. **Restore** (`--restore`, Restormer) — **denoise + deblur**. Fixes the
   motion/defocus blur that classical cleanup + NAFNet *denoise* can't.
3. **Super-resolve** (`--sr`, below) — upscale low-res pages.

```bash
crispsorter ocr blurry.jpg --restore                       # deblur
crispsorter ocr book.jpg --dewarp --restore --sr --sr-engine esrgan
```

## Super-resolution (low-res pages)

`--sr` upscales a page whose short side is ≤ `--sr-max-px` (default 1200) **4×**
*before* OCR. `--sr-engine` selects the model: **PAN** (`pan-x4`, 0.5 MB, fast,
default), **Real-ESRGAN** (`esrgan-x4`, better on real-world blur/noise/
compression), or **SAFMN** (`safmn-x4`, lightweight) — a real recognition
win on small scans, screenshots, and faxes. It pairs with the PDF DPI profiling
(a low-DPI page is detected, then super-resolved); the SR compute is in C++,
CrispSorter only decides when to invoke it. Larger pages skip SR (OCR is fine and
4× would waste memory). `ocr_crispembed::super_resolve_page` (cached engine).

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
  layer. This requires `ocr-reviewed-pdf` and the matching native renderer.
  The checked export path rejects unsupported text or geometry instead of
  silently emitting a damaged text layer. Consult the workspace guide for
  current language and rendering limits.

**OCR PDF/A output is unavailable.** The CLI still parses `--pdfa`, but the OCR
renderer rejects it because archival conformance has not been validated. Do not
use it as an archival conversion recipe. The separate PDF Tools operation adds
PDF/A identification metadata and an OutputIntent; it does not establish that
fonts, images, or the complete file conform to PDF/A. Independent validation and
content remediation are scoped in [OCR-13](ocr-roadmap.md#ocr-13-validated-pdfa-output).

Native hOCR/ALTO/PDF rendering stays in CrispEmbed. CrispSorter validates the
reviewed project and export inputs, and implements its text, JSON, HTML,
Markdown, and desktop DOCX adapters separately. DOCX currently carries corrected
text as paragraphs; it does not reconstruct the source page's full typography,
tables, or columns.

## How to drive it

### CLI — ad-hoc (`crispsorter ocr`)

```bash
crispsorter ocr scan.pdf                               # plain text → stdout
crispsorter ocr scan.png --engine tesseract --source-type scanned_doc \
    --denoise --punct-model fireredpunc
crispsorter ocr paper.pdf --render hocr                # → stdout
crispsorter ocr paper.pdf --render alto --out paper.xml
crispsorter ocr paper.pdf --render pdf  --out paper-searchable.pdf   # binary → --out required
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

## Key-information extraction (KIE)

Pull **structured fields** (total, date, vendor, invoice number, …) out of
documents — built on the OCR pipeline + zero-shot NER (GLiNER), so no new model:

- **At ingest (searchable metadata)** — `Settings → NER` label list is zero-shot.
  Add document-field labels and each becomes a searchable **`field:value` tag**
  on the document (e.g. `total:€42.50`, `vendor:Acme`). Same machinery as the
  P19 entity tagging — just point it at the fields you care about.
- **Ad-hoc CLI** — `crispsorter kie <file> --labels "total,date,vendor"
  [--threshold 0.5]` OCRs the file and prints the extracted fields
  (`label · value · score`) as JSON or text. Reuses the persisted NER model.

```bash
crispsorter kie receipt.png --labels "total,date,vendor,tax"
crispsorter kie invoice.pdf --labels "invoice number,due date,amount" -f json
crispsorter kie form.png --labels "question,answer" --lilt
```

The default CLI path is **flat-text** KIE (GLiNER over the OCR text).
`--lilt` selects **layout-aware** extraction; `--lilt-model` selects its model and
also enables that path. The review workspace offers layout-aware fields through
its Extract panel. These operations require the corresponding native build and
models. Extracted values still need review; enabling LiLT does not establish
accuracy on a particular form or vocabulary. Graphical reusable form templates
and field validation are planned in [OCR-07](ocr-roadmap.md#ocr-07-reusable-form-and-zone-templates).

## Feature flags

| Flag | Enables |
|------|---------|
| `crispembed` (`-metal`/`-cuda`/`-vulkan`) | Smart OCR Pipeline, native engine integration, layout and extraction |
| `pdf-render` | scanned-PDF rasterization (PDFium) |
| `ocr-review-diagnostics` | additive native review/provenance snapshots with the paired runtime |
| `ocr-reviewed-pdf` | checked searchable PDF output with the matching updated native renderer |
| `ocr-workspace` | paired `crispembed`, `pdf-render`, review diagnostics, and reviewed PDF capabilities |
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
| Interactive review and passage editing | `src/lib/components/OcrWorkbench.svelte`, `src/lib/components/ocr/` |
| Project, confidence, extraction, and partial-OCR validation | `src/lib/ocr/` |
| Durable project storage and workspace native commands | `src-tauri/src/ocr_workspace.rs` |
| C++ orchestrator + engines + renderers | CrispEmbed (`ocr_orchestrator`, `ocr_render`, …) |
