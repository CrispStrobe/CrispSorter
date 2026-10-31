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

Selectable per stage (`--engine`, or per-stage in the GUI builder):

| Engine | Kind | Default model (license) | Notes |
|--------|------|-------------------------|-------|
| `dbnet_trocr` | DBNet detect + TrOCR recognize | `dbnet-det` (Apache-2.0) + `trocr-printed` (MIT) | default; lightweight |
| `surya` | Surya-OCR-2 | `dbnet-det` + `trocr-printed` unless set | 91-language detection |
| `tesseract` | DBNet detect + Tesseract-LSTM recognize | `tesseract-eng` | per-language GGUF (`tesseract-eng`, …) |
| `got` | GOT-OCR2 | `got-ocr2` (**Tongyi Qianwen RESEARCH, non-commercial**) | VLM, single-shot; needs license acknowledgement |
| `glm` | GLM-OCR | `glm-ocr` (MIT) | VLM |
| `qwen2vl` | Qwen2-VL family | `german-ocr-3.1` (Apache-2.0) | VLM, strong on German |
| `qwen3vl` | Qwen3-VL family | `qwen3vl-2b` (Apache-2.0) | VLM; also `firered-ocr`, `uni-mumer-qwen3-vl-2b` |
| `internvl2` | InternVL2 | `internvl2-ocr` | VLM |

`dbnet_trocr` runs in "simple mode"; any other engine is built as an explicit
single stage so the choice takes effect.

### Model licenses (non-commercial weights)

A CrispEmbed license audit
([CrispStrobe/CrispEmbed#92](https://github.com/CrispStrobe/CrispEmbed/pull/92),
evidence `tests/results/licenses/registry-qwen-audit.json` on that branch) found
registry models labelled permissive whose weights inherit non-commercial terms:

| Registry alias | Effective license | Why |
|---|---|---|
| `qwen2vl-3b` | [Qwen RESEARCH LICENSE AGREEMENT](https://huggingface.co/Qwen/Qwen2.5-VL-3B-Instruct/blob/main/LICENSE) (non-commercial) | is Qwen2.5-VL-3B-Instruct |
| `nanonets-ocr-s` | Qwen RESEARCH (non-commercial) | fine-tune of Qwen2.5-VL-3B |
| `uni-mumer-qwen2.5-vl-3b` | Qwen RESEARCH (non-commercial) | fine-tune of Qwen2.5-VL-3B |
| `teleocr-f16` / `teleocr-q8_0` / `teleocr-q4_k-m` | Qwen RESEARCH (non-commercial) | Qwen2.5-VL-3B vision tower |
| `got-ocr2` (engine `got`) | [Tongyi Qianwen RESEARCH LICENSE AGREEMENT](https://huggingface.co/Qwen/Qwen1.5-0.5B/blob/main/LICENSE) — non-commercial, and neither the model nor its outputs may be used to improve any other LLM | decoder is Qwen1.5-0.5B |
| `transcoda` | [CC-BY-NC-4.0](https://creativecommons.org/licenses/by-nc/4.0/) | encoder fine-tuned from ConvNeXt V2 ImageNet weights |

None of these is a default. Configuring one (GUI field, GOT engine, registry
download, CLI flag or a local GGUF path whose filename matches) requires an
explicit acknowledgement that names the license, states the non-commercial
restriction and links the text: the GUI asks once and persists it
(`acceptedModelLicenses`); the CLI needs `--accept-license` or
`CRISPSORTER_ACCEPT_MODEL_LICENSE=<alias>`. Without it the model is neither
downloaded nor loaded (`index::license_consent` + `ocr_crispembed::resolve_checked`).
After acceptance CrispSorter also sets CrispEmbed's own runtime gates
(`CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE=1` / `CRISPEMBED_ACCEPT_TONGYI_QIANWEN_LICENSE=1`),
which newer runtimes require. Apache-2.0 alternatives for document / formula
OCR: `qwen3vl-2b`, `firered-ocr`, `uni-mumer-qwen3-vl-2b`, `paddleocr-vl`,
`olmocr-2-7b`.

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
```

This is **flat-text** KIE (GLiNER over the OCR text). Genuinely **layout-aware**
extraction (CrispEmbed's LiLT — uses word positions, better on dense forms) is a
future upgrade; the lower-level `crispembed_lilt_*` API exists for it.

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
