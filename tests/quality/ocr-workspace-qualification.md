# OCR workspace qualification receipts

Use [ocr-workspace-receipt-schema.json](ocr-workspace-receipt-schema.json) to record
what ran, against which inputs and versions, and what remains unverified. The
receipt has no overall `passed` field. A synthetic browser result cannot qualify
native OCR, and a successful Tesseract command cannot qualify the Rust parser or
an installed CrispSorter application.

## Separate evidence boundaries

Every receipt includes all six categories. Use `not_run`, `blocked`, or
`not_applicable` with an explanation when a category has no executed evidence.

| Category | What a passing result demonstrates |
| --- | --- |
| `synthetic_ui` | The production UI behaved correctly with explicitly synthetic `WorkspaceClient` responses. No OCR accuracy claim. |
| `native_parser` | The production parser or normalization code passed its recorded-input checks. A Python inspection of TSV does not qualify the Rust parser. |
| `native_provider` | An actual recognizer ran and produced checked output. Record `external_cli`, `production_module`, or `installed_app`; these are different scopes. |
| `live_corpus` | The stated frozen real-scan cases ran, with every case outcome and its declared metric retained. A single synthetic page is not a corpus. |
| `device` | The identified packaged application ran on the stated device/platform. A build, package inspection, or browser harness is insufficient. |
| `standards` | The identified independent validator checked the exact exported artifact against the named standard/profile. Export success is insufficient. |

`passed` requires an executed result, elapsed time, explicit assertions, and
hashed evidence. `failed` records an observed failure. `blocked` records a missing
runtime, model, device, permission, or runner. `not_run` means no attempt occurred.
`not_applicable` requires a reason tied to the receipt's scope. A category may be
`passed` only when every result it includes passed; preserve failures and retries
as separate evidence rather than overwriting the original receipt.

## Record a run

Create a new dated subdirectory under `tests/quality/results/`. Keep raw outputs
beside the receipt, with repository-relative paths and SHA-256 checksums. Do not
change an input corpus or earlier result to make a new run pass.

Record the actual app commit/version and dirty-tree state, OS/architecture/runtime,
native executable or library identity, installed language availability, and the
model or language-data file checksums. `engine: null` and an empty model list are
appropriate for a synthetic UI-only run. Do not substitute a requested package
version for the runtime that actually executed.

Include both requested and effective settings: engine, languages, segmentation
mode, preprocessing, thresholds, crop, memory/token budgets, and any fallback.
Record each input's checksum, image dimensions, and ground-truth checksum when
available. Each check records its exact command argument array, elapsed
milliseconds, input IDs, assertions, numerical metrics when relevant, and output
or log checksums. `settings` on a result records case-specific overrides.

For System Tesseract word evidence, retain the **TXT and TSV produced by the same
invocation**, stderr, and the version output. Record the selected language data,
page-segmentation mode, raw word count and confidence range, text alignment
outcome, and box-bound validation. A live external CLI capture may prove these
properties without proving that the application parser or UI consumed them.
Label that boundary `external_cli`; leave the other categories unqualified until
they run. Missing model checksums or inaccessible artifacts belong in limitations.

For word review, preserve the original raw evidence and any exported JSON used by
the check. Validate exact UTF-16 spans, provider token boundaries, full-page box
coordinates, direct score provenance, null-score behavior, and correction
invalidation. A large recognition confidence is not a measured probability that
the transcription is correct.

## Reuse the real-scan panel

[ocr-real-panel.json](ocr-real-panel.json) freezes six licensed source pages and
their ground truth: clean English prose, German Antiqua, Fraktur, a table,
degraded print, and a receipt photo. Keep its manifest/scorer hashes, case IDs,
input hashes, declared metrics, and original selection rule. Use `cer_nospace`
for its complete transcriptions and `word_recall` for its partial receipt ground
truth. Retain every case failure and timeout; do not average only successful cases.

A provider smoke run on [ocr-task.png](ocr-task.png) is useful native evidence,
but remains a synthetic input. It cannot mark this real panel as qualified.
Record actual model hashes and effective configuration for each new panel run;
earlier receipts from another app or native revision are historical evidence.
This guide does not alter the panel's decision rule or claim coverage for all
languages, handwriting, or multi-column documents.

## Validate the receipt

With Python's `jsonschema` package available:

```sh
python -c 'import json, sys; from jsonschema import Draft202012Validator, FormatChecker; schema=json.load(open("tests/quality/ocr-workspace-receipt-schema.json")); Draft202012Validator(schema, format_checker=FormatChecker()).validate(json.load(open(sys.argv[1])))' path/to/receipt.json
```

Schema validity checks the record's structure. Also recompute artifact hashes,
confirm referenced input IDs exist, and inspect the actual output and logs.
Report a concise boundary-specific result, such as “external Tesseract capture
passed; application parser and installed-device checks have not run.”
