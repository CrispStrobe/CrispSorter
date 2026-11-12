# OCR engines and the measured path

This inventory describes the shared OCR routing update on 2026-11-11, after
integration of main `2a9a74d509c6803e50c15e5284d5c95401f8646d`. Workbench, file
extraction, ingestion and CLI use one native engine-selection policy. See the
[routing audit](ocr-routing-audit-20261111.md) for entry points and limits. A listed
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

## Model downloads

OCR never downloads a model while recognizing. Every task first asks the
model manager what it needs (`ocr_models_required`): name, purpose, size,
licence and install state. Missing models are shown in one dialog (or, on
the CLI, as a plan with a `[y/N]` prompt, `--yes` / `--download-models`, or
`--offline`). Installation verifies pinned SHA-256 hashes, resumes an
interrupted `.part` with HTTP Range, can be cancelled, checks free disk
space, and holds a per-model lock file so the app and the CLI never write the
same download. Everything lands in one cache (`CRISPEMBED_CACHE_DIR`, default
`~/.cache/crispembed`); folders earlier releases downloaded to stay readable
and are passed to the isolated OCR worker. Settings > OCR models lists each
engine's models with Download and Remove; `crispsorter models plan|pull`
does the same from a shell.

## Workbench: Recognize choices

A CrispEmbed-enabled build advertises **20 non-experimental embedded choices**,
plus **Automatic pipeline**. Two additional experimental choices appear when
their runtime gates are enabled. Installed System Tesseract, ocrs and PaddleOCR
are added when their respective runtime capabilities are available. The
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
| EasyOCR | Embedded DBNet/CRNN path. No published GGUF exists for its recognizer (`easyocr-english-g2`), so it is not downloadable: place the file in the model folder. Automatic never selects it. |
| Document and vision models | GOT OCR, GLM OCR, Qwen2 VL, InternVL2, DeepSeek OCR 2, Pix2Struct, Granite Vision, LightOnOCR, Qwen3 VL, Unlimited OCR, olmOCR 2 (7B), and LFM2.5 VL. These are local model choices. The UI labels GOT as non-commercial; model-specific licence gates remain in force. |
| Custom-model choices | Surya and Unified are available in the shared catalog; supply their required model files in a matching advanced stage. |
| Experimental choices | Unlimited-OCR-2 and MiniCPM-OCR-1.0 appear when their runtime enablement flags are set. |
| Legacy local recognizers | ocrs and PaddleOCR v4 can be selected explicitly when compiled. Their plain-text review adapter supplies an unscored page block. Their pinned weights (ocrs ONNX, PaddleOCR v4 det/rec/SLANet + vocabularies) are installed by the OCR model manager like every other model; usls never downloads on its own. |
| Automatic pipeline | A routing policy. Unconfigured embedded OCR selects Tesseract; an enabled configured pipeline retains its stages. Disabled advanced stages do not reactivate implicitly. Explicit legacy tiers remain meaningful. |

The pinned embedded runtime is CrispEmbed 0.17.19; the
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
automatically. Enabled preparation is applied before recognition, with evidence
bound to the resulting image. A build needs the relevant preparation capability
to perform those steps. The isolated Qwen CPU profile applies only to Qwen2 VL.

Saved application engine/language/model settings feed the general application
OCR routes and CLI OCR, KIE, table and zone commands. CLI parsing defaults do not
overwrite the saved recipe. All four CLI commands accept `--ocr-config FILE`
for a complete pipeline JSON snapshot and `--data-dir DIR` for a saved app
profile. Explicit OCR flags override the selected snapshot; boolean processing
flags also accept `--flag=false`. Workbench project choices and explicit
request options take precedence. A job
captures its policy once, so later Settings edits cannot change engines between
PDF pages. Explicit engine or model failures propagate; they do not silently
switch to another recognizer.

For example, `crispsorter ocr scan.png` uses the saved application policy.
`crispsorter ocr scan.png --ocr-config pipeline.json --engine tesseract
--language deu --cpu-pixel-budget=false` explicitly selects embedded Tesseract
and German while retaining compatible settings from the supplied JSON. A
single-model vision engine accepts `--model` as an alias of `--det-model`;
incompatible model flags fail instead of being silently ignored. Supplying one
preparation override to a disabled recipe activates that request without
reactivating hidden restoration or postprocessing steps.

The snapshot is the serialized `OcrPipelineConfig` object, not an entire app
settings export. It may contain advanced `stages` with detector thresholds,
model paths, prompts and cleanup settings. Unknown top-level, stage or cleanup keys, invalid
engine/processing selections and snapshots larger than 64 KiB are rejected.
Reading a supplied snapshot does not create or modify an app settings database.
Source-routed recipes use an enabled Automatic `stages` snapshot. A fixed
`--source-type` pin that the shared resolver cannot preserve is rejected.
Flat escalation flags cannot silently override an advanced stage recipe;
edit its stage JSON or use `--engine` with `--model` for a single vision model.

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

Paragraph grouping applies the same score checks after line-level layout
classification. It preserves line breaks inside an enclosing region box;
multiline PDF placement uses the pinned renderer's estimates within that box.
This does not establish verified source line or word coordinates. Regions with
native word evidence remain separate and retain their evidence.

## Advanced pipeline settings

The [stage editor](../src/lib/components/Settings.svelte) and
[native engine mapping](../src-tauri/src/extractors/mod.rs) also expose these
configurations. They share the Workbench capability catalog; model and
experimental prerequisites still apply:

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

Native image/PDF extraction, archive and mail members, background ingestion,
manual promotion, template text zones and text-based KIE use the shared native
policy and dispatcher. An explicit legacy tier or engine is a selection, not a
request to try another recognizer silently if it fails.

The optional preferred document converter can supply a PDF's text before the
native extractor is needed; forced PDF OCR bypasses that preference. Formula
recognition is a separate specialist operation that produces LaTeX and uses its
selected formula model.

The frontend [image extractor](../src/lib/extractors/imageExtractor.ts) and
[PDF OCR fallback](../src/lib/extractors/pdfExtractor.ts) now call the same native
service in an application runtime. They send bounded raw image bytes and one
captured engine/language/configuration snapshot. PDF extraction preserves usable
digital text and sends only pages needing OCR (or all pages for forced OCR).

A browser-only runtime has no CrispEmbed/native execution backend.
Its Automatic choice uses a reusable Tesseract.js worker. Selecting a native
engine, native tier, custom native pipeline or CPU profile there produces an
explicit capability error. The browser worker is serialized, retired on failure
or active cancellation, and disposed after an idle interval.

Some specialist outputs impose additional contracts. Plain-text ocrs/Paddle
adapters do not provide verified boxes for structured export. LiLT's pinned
binding owns a DBNet+TrOCR recognizer, and native table structure extraction owns
a Tesseract recognizer. Those adapters validate compatibility with an explicit
selection instead of substituting a model. Use ordinary OCR/text-based KIE for
other engine combinations. See the [audit boundaries](ocr-routing-audit-20261111.md#capability-boundaries).

## Recent integration

The [remote integration audit](remote-integration-audit-20261111.md) accounts
for all 76 live branch heads in the 10:37 UTC snapshot and open PRs #44–46,
including PDF consolidation, OCR document view and the advanced SMB head. The app
shares SMB transport settings between GUI and CLI and connects streaming reads
to transfer, sync and index-promotion consumers. The consolidated PDF editor and
CLI share native text exports and edit operations. Paragraph editing retains
the same engine policy and confidence-evidence constraints. See the
[surface and operation-log audit](surface-observability-audit-20261111.md) for
critical-step events, CLI/GUI consistency and verification boundaries.

Main `2a9a74d` merged PR #42 and has exactly the same source tree as its
feature head `48bab31`. This follow-up starts from that merged main revision;
it does not replay those changes.

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
