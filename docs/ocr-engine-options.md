# OCR engines and the measured path

This inventory describes the application after integration of the OCR/PDF update
from main `de802fed5d8f7d40525877ad826fd627835394b9` on 2026-11-11. It distinguishes
workbench choices, advanced pipeline stages and extraction fallbacks. A listed
model still needs its matching files, supported runtime and any required model
licence consent.

## Engine used for the six-page measurement

The [committed panel receipt](../tests/quality/measurements/system-ocr-explicit-output.json)
used **System Tesseract 5.5.0**, engine ID `system_tesseract`, through the actual
production [word-evidence adapter](../src-tauri/src/ocr_word_evidence.rs).
It used automatic page segmentation (`--psm 3`), pinned `tessdata_fast` packs,
and these explicit language selections:

| Page | Language selection |
| --- | --- |
| English prose and English table | `eng` |
| German Antiqua | `deu` |
| German Fraktur and degraded German historical print | `deu_latf` |
| Receipt | `eng+ind` |

The trained-data revision is `87416418657359cb625c412a48b6e1d6d41c29bd`.
Text and TSV words, boxes and scores come from the same recognition invocation.
The adapter requests `tessedit_create_tsv=1` and `tessedit_create_txt=1`
explicitly, so standalone language-pack directories do not depend on optional
named configuration files. That fix remains intact after the merge.

No internal image cleanup was applied in this panel. Confidence is uncalibrated.
The receipt's 0.2632 word recall is against a partial reference for that page;
it is not a full-transcription score or an evaluation of every OCR engine.
This continuation has not run a comparative accuracy panel across the other
engines below.

## Workbench: Recognize choices

A CrispEmbed-enabled build advertises **18 explicit embedded engine choices**,
plus **Automatic pipeline**. Installed System Tesseract is added separately when
its capability probe succeeds. The
[native capability list](../src-tauri/src/ocr_workspace.rs) governs availability;
[the workbench](../src/lib/components/OcrWorkbench.svelte) supplies the labels.

| Choice or group | What the application provides |
| --- | --- |
| System Tesseract · word evidence | The installed desktop executable and installed `.traineddata` packs. Returns native word offsets, boxes and scores with source receipts. Requires a build that can run sidecars; unavailable on mobile and MAS/sandboxed builds. |
| Tesseract LSTM | Embedded CrispEmbed/GGUF recognition, normally paired with DBNet detection and a registered language model. This is separate from the executable used by the measured panel. |
| Tesseract LSTM · Fraktur | The dedicated embedded Fraktur stage. |
| DBNet + TrOCR | Embedded detection and recognition stages. |
| PARSeq scene text | Embedded detector/recognizer option. |
| PP-OCRv6 (CJK lines) | Embedded PP-OCRv6 path with its own model requirements. |
| EasyOCR | Embedded DBNet/CRNN path. |
| Document and vision models | GOT OCR, GLM OCR, Qwen2 VL, InternVL2, DeepSeek OCR 2, Pix2Struct, Granite Vision, LightOnOCR, Qwen3 VL, Unlimited OCR, olmOCR 2 (7B), and LFM2.5 VL. These are local model choices. The UI labels GOT as non-commercial; model-specific licence gates remain in force. |
| Automatic pipeline | A routing policy. In a CrispEmbed build, an empty-stage configuration now selects embedded Tesseract. An explicitly configured pipeline keeps its stages. |

The pinned embedded runtime is CrispEmbed 0.17.18; the
[runtime lock](../scripts/crispembed-runtime-lock.json) records the source and
platform assets. A menu choice is not proof that its weights are already
installed or that its accuracy has been measured on the user's documents.

### Language defaults and custom models

For a fresh explicitly supplied Automatic/embedded-Tesseract configuration,
Model default uses the interface-language hint: `deu` for German and `eng` for
English. An explicit document language overrides that interface-language hint.
A configured Automatic pipeline retains its trained models and warns when an
explicit language hint cannot change them. A matching custom Tesseract
recognizer retains its model when Model default is selected. If the frontend
has no configuration snapshot, it preserves the
Automatic request so native app configuration remains authoritative.

System Tesseract has a separate installed-language selector. Its default is
English (`eng`), as stated in the UI; it does not detect the page's language
automatically. Pipeline cleanup, layout and the Qwen CPU profile are not applied
inside this System Tesseract recognition step. Apply page preparation first.

### Confidence and source evidence

The embedded review adapter retains the provenance of region/sequence scores.
Those scores do not automatically establish native word boxes or verified
character alignment. The new Confidence view uses available verified word or
character evidence and leaves unsupported words unscored. System Tesseract's
same-run native word evidence remains a distinct path.

Line grouping preserves the recognizer's resulting line and column order and
untouched native-word/non-text regions. It still sorts words left-to-right
inside each geometric segment; it does not establish general right-to-left
reading-order support. A merged line score requires complete,
uniform score provenance; mixed or missing scores cannot become a whole-line
recognition score. Character evidence requires verified codepoint alignment
for every contributing region.

## Advanced pipeline settings

The [stage editor](../src/lib/components/Settings.svelte) and
[native engine mapping](../src-tauri/src/extractors/mod.rs) also expose these
configurations, which are not all direct Recognize-menu choices:

| Advanced stage | Requirement |
| --- | --- |
| Surya | Explicit suitable model configuration. |
| Unified | An explicitly selected GGUF whose metadata chooses the engine; no registry default. |
| Unlimited-OCR-2 | Experimental; requires `CRISPEMBED_UNLIMITED_OCR2=1`. |
| MiniCPM-OCR-1.0 | Experimental; requires `CRISPEMBED_MINICPM_OCR=1` and `CRISPEMBED_QWEN35_OCR=1`. |

The Low-memory CPU profile applies to a single Qwen2 VL stage. On supported
desktop builds it uses an isolated CPU worker with a 262,144-pixel vision budget
and releases the parent's cached pipeline first. The UI warns that it still
uses several GB of RAM, reloads the model each run and may lose fine print.
This is a pixel budget, not a general process-memory cap or a profile for every
OCR engine. See [the worker](../src-tauri/src/extractors/ocr_worker.rs).

## Other extraction routes

The native/background ingestion path has the CrispEmbed pipeline and a legacy
fallback ladder: optional PaddleOCR v4 through `usls`/ONNX when built with
`paddle-ocr`, `ocrs`/RTen, and installed system Tesseract. These settings are
separate from the workbench menu.

The frontend [image extractor](../src/lib/extractors/imageExtractor.ts) and
[PDF OCR fallback](../src/lib/extractors/pdfExtractor.ts) use Tesseract.js,
with English/German selections. These plain-text extraction routes were not the
source of the native six-page word-evidence measurement.

## Recent integration

The feature branch preserved `1f5d470` (Windows member-copy protection) and
`b6bc7fa` (provider model-list test), then merged main in `c9b3cfd`. The incoming
OCR/PDF commit `73c6070` adds the Automatic default, line-level review,
confidence-coloured text, less crowded controls and badges, app styling, the
PDF editor reopen-loop fix and progressive thumbnails.

The older `feat/ocr-workspace-engines-cpu-budget` branch was already an ancestor
of main. Its engine catalog and worker were retained without replaying those
commits. Focused follow-up fixes preserve configured model selection, native
line order and score provenance through the new behavior. PDF thumbnail runs
capture their session and refresh identity before asynchronous work and reject
stale document loads and tile publication after a newer refresh or close.
Concurrent stage and thumbnail consumers share one preview load for the same
session snapshot, so a cancelled caller cannot destroy another current
consumer's preview document.
