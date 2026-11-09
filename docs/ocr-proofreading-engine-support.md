# Proofreading, recognition hints, and model training

This is the OCR-02d engine feasibility decision for the
[OCR roadmap](ocr-roadmap.md), inspected on **2026-11-09**. The native baseline
is OCR-01 commit `349725a`, with CrispEmbed **0.17.16** pinned to
`e4ee71937a68f1ff9d28f4d7cdbd66bda68ba05a` in
[`crispembed-runtime-lock.json`](../scripts/crispembed-runtime-lock.json).
The application proofreading layer is being delivered in OCR-02; its pushed
status and validation belong in the roadmap delivery log.

**The selected OCR-02 implementation stores vocabulary and reusable correction
rules in the application. Accepting a correction does not train a recognizer or
silently add an engine decoding hint.** Full model training remains unavailable
until the data, adapter, and evaluation gates below are implemented. This
document closes the feasibility decision, not those gates.

## Current support at each boundary

| Path | What the application can use | What is not connected |
| --- | --- | --- |
| Application proofreading | Explicit-language vocabulary, protected terms, occurrence-specific ignore decisions, suggested corrections, and literal correction rules. Text changes use a captured preview and explicit acceptance. | Vocabulary membership is not OCR confidence. These records are not model weights or native decoding dictionaries. |
| System Tesseract (`system_tesseract`) | An explicitly selected installed executable and installed language packs; one invocation supplies aligned TXT/TSV with real word boxes and scores. Its language argument can select an already installed custom pack when the installed probe reports it. | The current adapter does not pass `--user-words` or `--user-patterns`, invoke training tools, install language packs, or register a newly trained model. |
| Embedded CrispEmbed Tesseract (`tesseract`) | The pinned Rust/C pipeline loads the selected converted GGUF model and configured preparation/recognition stages. | No application or public pipeline parameter carries a mutable user lexicon, user patterns, or a training request. It is not the installed Tesseract CLI. |
| Other embedded OCR/VLM engines | Their existing model selection and documented inference settings. Some VLM paths accept a language prompt. | A prompt or corrected output is not a weight update. No supported correction-to-training adapter is exposed by the application. |
| Browser/webview spellcheck | Platform-provided editing assistance where the webview supplies it. | It is not an application-managed OCR dictionary or a portable training mechanism. |

The implementation boundaries can be checked without inferring capabilities
from UI labels:

- [`proofreadingData.ts`](../src/lib/ocr/proofreadingData.ts):
  `ProofreadingData`, `readProofreadingData`, `mergeProofreadingData`, and
  `exportProofreadingData` define application-owned vocabulary/rules and their
  persistence. Occurrence ignores are deliberately not portable vocabulary.
- [`proofreading.ts`](../src/lib/ocr/proofreading.ts): `suggestCorrections`,
  `captureCorrectionPreview`, and `applyCorrectionPreview` implement suggestion
  provenance, scoped previews, and stale-target checks. Literal replacements
  are not Tesseract pattern expressions.
- [`ocr_word_evidence.rs`](../src-tauri/src/ocr_word_evidence.rs):
  `capabilities`, `requested_language`, and `system::recognize` implement the
  installed provider. The invocation currently supplies `-l`, `--psm 3`, `tsv`,
  and `txt`. The process route requires `sidecars` and excludes MAS/mobile.
- [`extractors/mod.rs`](../src-tauri/src/extractors/mod.rs): `OcrPipelineConfig`
  and `OcrStageSpec` have no user dictionary, pattern, or training fields;
  `tess_model_dir` is a directory of Tesseract **GGUFs** for language selection.
- [`ocr_crispembed.rs`](../src-tauri/src/extractors/ocr_crispembed.rs):
  `build_pipeline` constructs `CrispOcrPipeline::new` or `from_stages` using
  explicit parameters. It does not forward arbitrary Tesseract CLI options.
- [`ocr_workspace.rs`](../src-tauri/src/ocr_workspace.rs):
  `recognition_config` maps the embedded language choice to a registered model
  or a supported VLM prompt. The System Tesseract route is separate.

## What Tesseract supports, and what that means here

The upstream Tesseract 5.5.0 [command-line manual][tess-manual] exposes
`--user-words FILE` and `--user-patterns FILE`. These are inference-time inputs,
not a neural-network training command. User words are a word list; patterns
describe permitted character classes using Tesseract's pattern grammar, not
JavaScript regular expressions. Options must precede output config files such
as `tsv` and `txt`.

The official [user-pattern example][tess-patterns] uses `OEM_LSTM_ONLY`, so
patterns must not be described as inherently legacy-engine-only. Upstream
explicitly describes both user dictionaries and patterns as nonexclusive
decoding hints. They cannot guarantee valid identifiers or correct text, and
very broad patterns can slow recognition. Field validation must continue to
report a failed constraint even when a recognizer used the same expected format.

**Decision:** the installed System Tesseract path is the concrete candidate
for a later recognition-hint adapter; it does not need an invented CrispEmbed
ABI. That adapter is **not implemented by the current OCR-02 application
dictionary**. Its acceptance contract is:

1. A separate, explicit recognition-hint setting selects language-scoped word
   lists or Tesseract patterns. Protected terms, ignore decisions, and literal
   correction rules are not automatically converted into engine hints.
2. Validate bounded UTF-8 data and the supported pattern grammar, write files
   into the recognition run's private directory, and pass argument values
   directly to the bounded subprocess. Do not alter global environment state,
   installed traineddata, or a shared user configuration.
3. Record effective language, executable version, decoder mode when selected,
   hint format/version, file hashes, and entry counts in run provenance. A
   successful process exit alone must not imply that a hint file was loaded.
4. Keep the same-run TXT/TSV alignment and original-evidence contract. Show
   changed recognition as a candidate for acceptance, with an unhinted baseline
   available for evaluation. Do not merge scores from different runs.
5. Test supported Tesseract versions with and without hints, malformed files,
   Unicode terms, language combinations, irrelevant terms, and negative
   identifier examples. Measure regression and latency before making a quality
   claim. Keep the capability unavailable where subprocesses are prohibited.

These requirements define an implementable inference adapter. They are not
prerequisites for using the application dictionary and explicit correction
previews already selected for OCR-02.

## Pinned CrispEmbed boundary

At the inspected pin, the public C structures
[`crispembed_ocr_pipeline_params` and `crispembed_ocr_stage`][crisp-c] and Rust
[`OcrStageSpec` / `CrispOcrPipeline`][crisp-rust] have no user-word,
user-pattern, or training entry point. A frontend setting cannot add one.

There is dictionary-related machinery inside the native implementation:
[`tesseract_lstm.h`][crisp-tess] describes loading converted GGUFs, optional
embedded DAWG graphs, and introspection methods, with graph loading gated by
`CRISPEMBED_TESSERACT_DAWG_LOAD`. That is not a public per-user dictionary
mutation API. The existence of these internals does not establish support for
injecting the application's vocabulary into the pinned pipeline.

An embedded lexicon adapter therefore needs a deliberate upstream interface,
matching C and Rust bindings, a runtime pin, capability reporting, and paired
tests using the released library/model combination. An application-only field
or a guessed foreign-function declaration would be unsupported. Likewise, the
app's converted GGUF path is not a tesstrain checkpoint input; no reverse
conversion or fine-tuning path is provided by this integration.

## Model training: blocked until the following gates are satisfied

Tesseract has a real, separate training toolchain. The current official
[`tesstrain` workflow][tesstrain] requires Tesseract **5.3 or newer built with
training tools**, matching Leptonica, GNU make 4.2 or newer, Python and its
dependencies, and supporting utilities. It expects line images paired with
single-line `.gt.txt` transcriptions, builds checkpoints/traineddata, and has
evaluation targets. Its current documentation includes Windows setup; platform
support must be tested rather than inferred from older Linux-only guidance.

The official [model-data documentation][tess-data] identifies floating-point
`tessdata_best` models as fine-tuning bases; integer `tessdata_fast` models are
not suitable bases. An installed working recognition pack is not evidence that
the pack can be trained. The [Tesseract 5 training guide][tess-training] directs
users to the current tesstrain workflow; the old `tesstrain.sh` workflow is
unsupported.

The following are **CrispSorter acceptance requirements**, not claims that they
exist today:

| Gate | Required implementation and evidence |
| --- | --- |
| Deliberate data collection | An explicit collection choice, a visible dataset, export and deletion/reset. Each example records immutable source-image/crop hashes, page and region revision, language, reviewed transcription, and data-use rights. No correction is silently enrolled. |
| Valid training pairs | A verified line crop and its exact human-reviewed transcription. A paragraph rectangle, a word box, or arbitrary edited passage text alone cannot certify a line-level training pair. Preserve original OCR separately and reject unresolved image/text mismatches. |
| Versioned training environment | Pin tesstrain, Tesseract, Leptonica, training tools, language resources, and the floating-point base model by version and hash. Probe them independently of recognition availability. Bound jobs, report progress/failure, support cancellation, and retain reproducible settings/logs. |
| Data partition | Split by source document or independent acquisition before tuning, so neighboring or duplicate crops cannot leak into the held-out set. Freeze membership and checksums. Keep training, development, and final evaluation distinct; repeated tuning against the final test set invalidates its status. |
| Measured acceptance | Compare the original and candidate model on the same frozen pages/crops, with declared text normalization. Record character/word error rates, critical identifiers/numbers, introduced errors, language/font strata, latency, and memory. Define corpus-specific improvement and regression limits before training; training loss alone cannot pass this gate. |
| Runtime integration | Install a candidate under a distinct identity with explicit model selection and rollback. Verify the actual application adapter with the candidate, not just `lstmtraining` output. Recheck word geometry, exact text alignment, score provenance, export, and the declared packaged platforms. Never replace the default model silently. |
| Evidence and lifecycle | Store a receipt linking dataset partition, toolchain, base/candidate hashes, configuration, evaluation, and application commit. Support removal/export of collected data and generated artifacts without altering source documents. Mark incomplete checks as blocked or not run. |

Until these gates are implemented, the correct product state is **model
training unavailable**. There is no train button, automatic weight update, or
claim that repeated corrections improve the underlying recognizer. A future
training proposal should first choose a bounded language/document target and a
qualified toolchain; it should not generalize a small demonstration to every
engine or document type.

## Evidence and claims

The [System Tesseract smoke receipt][smoke] records a real external CLI run and
raw word output. It does not test hints, training, the accuracy of a trained
model, or general document quality. The focused production-module and paired
application workflows are tracked separately in the roadmap. Use the
[qualification receipt guide](../tests/quality/ocr-workspace-qualification.md)
for any future hint/training evaluation so an external-tool success cannot be
reported as a packaged-application or held-out-corpus pass.

Proofreading suggestions retain their source label. Applying a dictionary or
rule suggestion does not raise recognition confidence; changed text remains a
human correction and the original evidence remains inspectable under the
[native word evidence contract](ocr-word-evidence.md).

[tess-manual]: https://github.com/tesseract-ocr/tesseract/blob/5.5.0/doc/tesseract.1.asc
[tess-patterns]: https://tesseract-ocr.github.io/tessdoc/APIExample-user_patterns.html
[tesstrain]: https://github.com/tesseract-ocr/tesstrain
[tess-data]: https://tesseract-ocr.github.io/tessdoc/Data-Files.html
[tess-training]: https://tesseract-ocr.github.io/tessdoc/tess5/TrainingTesseract-5.html
[crisp-c]: https://github.com/CrispStrobe/CrispEmbed/blob/e4ee71937a68f1ff9d28f4d7cdbd66bda68ba05a/src/crispembed.h
[crisp-rust]: https://github.com/CrispStrobe/CrispEmbed/blob/e4ee71937a68f1ff9d28f4d7cdbd66bda68ba05a/crispembed/src/lib.rs
[crisp-tess]: https://github.com/CrispStrobe/CrispEmbed/blob/e4ee71937a68f1ff9d28f4d7cdbd66bda68ba05a/src/tesseract_lstm.h
[smoke]: ../tests/quality/results/2026-11-09/system-tesseract-word-smoke/receipt.json
