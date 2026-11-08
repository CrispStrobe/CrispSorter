# Library model evaluation — 28 October 2026

The frozen panel contains 24 fictional, author-written English and German
documents, 12 retrieval questions with expected documents, and 12 document-type
labels. It covers correspondence, invoices, books, articles and unrelated items.
The text and references were committed before inference. This is a bounded
integration/adoption panel, not a claim about arbitrary personal libraries.

Inputs and exact model/source identities are in
[`tests/quality`](../tests/quality). Raw retrieval results, provenance and initial
failures are retained in [`results/2026-10-28`](../tests/quality/results/2026-10-28).
Retrieval and decision model work used two inference threads on a separate CPU
worker; OCR hardware/configurations are retained with their own measurements.
No model weights were downloaded to the shared development VPS.

## Retrieval

The baseline uses the original BGE-M3 Transformers weights, normalized CLS
pooling, float32 CPU inference and a 512-token limit. MoEdern reranks the
baseline's top 20 candidates using separate query/document roles, original
scoring masks and MaxSim. All three arms passed the role/mask checks. Consumer
MaxSim differed from the server's native score by less than 0.000006.

| Model | Recall@1 | Recall@5 | nDCG@10 | MRR | Stored document vectors |
| --- | ---: | ---: | ---: | ---: | ---: |
| BGE-M3 dense | 1.000 | 1.000 | 1.000 | 1.000 | 98,304 bytes |
| MoEdern 1.5M | 0.917 | 1.000 | 0.969 | 0.958 | 674,304 bytes |
| MoEdern 27M | 1.000 | 1.000 | 1.000 | 1.000 | 674,304 bytes |
| MoEdern 115M | 0.917 | 1.000 | 0.969 | 0.958 | 674,304 bytes |

Median measured operations were approximately 0.516 s for the dense model and
0.024 / 0.094 / 0.281 s for the three MoEdern arms. These samples mix document
encoding and query/rerank operations and exclude model loading. They are not
comparable end-user search latency claims. The app currently encodes candidate
passages afresh; it does not persist a MoEdern index or replace the dense index.

The predeclared gate allowed at most a 0.01 drop in Recall@5 or nDCG@10. Only
27M passed. Since the baseline already solved every question, this panel shows
no measured quality advantage for a default change. MoEdern remains an explicit
optional service; 1.5M and 115M are not promoted by this evaluation.

The overall retrieval run ended on the first SystemOne request because the
consumer omitted a required `model` schema field. All retrieval arms completed
before that failure. The adapter and evaluator now include the publisher's
required model identifier; decision inference is evaluated separately.

## Structured decisions

The corrected SystemOne-only run completed on the same frozen input and labels:
9 of 12 document types were correct (75%), below the predeclared 90% threshold.
Median inference time was 25.52 seconds on the CPU worker. Incorrect answers
included a conference letter classified as Other, a retrieval article classified
as Other, and a storage article classified as Book. Their model scores were
0.8535, 0.6502 and 0.8689 respectively, demonstrating why these scores must not
be treated as calibrated correctness probabilities.

**Adoption decision:** retain the optional, explicitly experimental suggestion
panel. Do not enable SystemOne by default, replace open-ended metadata
extraction, approve suggestions automatically or apply file moves from its
decisions. Keep the submitted excerpt visible and require human review.
The full responses and source/model hashes are in `decisions-run.json` and its
provenance receipt. This panel evaluates document-type choices; it does not
establish accuracy for arbitrary collection choices or review judgments.

## Newer OCR engines

The bundled app runtime is the immutable
[CrispEmbed 0.17.12 release](https://github.com/CrispStrobe/CrispEmbed/releases/tag/v0.17.12).
New development-branch engines and published weights are evaluated separately.
The app does not expose an unsupported engine through its released bindings.

Upstream real-scan receipts at source
[`2acee0b1c920`](https://github.com/CrispStrobe/CrispEmbed/tree/2acee0b1c920fa3d143f05c8d4022994d8b8c763)
include English prose, tables, forms and columns; German Antiqua, Fraktur,
degraded pages and tables; and receipt photos. The retained derived metrics
include each input's manifest-hash status. The form image with a mismatched
hash is excluded from adoption evidence. Outputs that hit their generation cap
are flagged, including two badly failing UnlimitedOCR2 receipts.

UnlimitedOCR2 F16 native inference took about 200–1,492 seconds per page and
roughly 12.5 GiB peak RSS on that worker. Several book pages had low character
error, but receipt failures and difficult table structure preclude treating it
as a generally reliable interactive replacement. Original MiniCPM reference
results show promising receipt/prose quality but also substantial CPU time and
memory use; reference quality alone does not qualify the native app integration.
The full page-by-page metrics remain available for inspection.

**Adoption decision:** retain released engines in task presets. Keep OvisOCR2,
UnlimitedOCR2 and MiniCPM-OCR out of the app selector until a compatible released
runtime, native quality checks and acceptable task latency are established.
The workbench's selective retries and undo are useful independently of adopting
another heavyweight model.

## Released OCR task wiring

A separate CPU probe executed all four task presets using the exact production
pipeline builder, configuration/cache code and Rust → C++ bindings, verified
against the immutable 0.17.12 archive. A source-equivalence gate and fragment
hashes tie this probe to binary source `0eb1a37`. All four retained the known
fixture words and year. This is one synthetic 1500 × 200 image, not a corpus
accuracy result, a table-structure test or an estimate for full-page scans.

| Preset | CPU seconds, including context setup | Known evidence retained |
| --- | ---: | --- |
| Quick scan | 7.54 | Yes |
| Multilingual book | 88.10 | Yes |
| Tables and receipts | 89.52 | Yes |
| Difficult page | 87.53 | Yes |

Timings exclude initial downloads. The verified DBNet and TrOCR files total
about 80 MB; the Qwen2.5-VL file is 2.74 GB. These CPU measurements support
keeping Quick scan as the inexpensive option. Actual performance depends on
image size, content and hardware. The recognized text, configurations, model
hashes, source fragments and probe lockfile are retained in the result folder.
The full native suite and real packaged-app PDF OCR checks are separate gates.

## Preregistered released-engine real-scan comparison

`tests/quality/ocr-real-panel.json` fixes all six priority-one pages whose hashes
matched the existing upstream manifest: English prose, German Antiqua, Fraktur,
a table, degraded print and a CORD receipt photo. The previously mismatched form
is excluded before this comparison. Image/reference/scorer hashes, case budgets
and the decision rule are frozen before inference. The original upstream scorer
is reused unchanged; word recall is primary for the receipt's partial reference.

Quick and multilingual run on all six pages; the table/receipt prompt is tested
on the table and receipt, and difficult-page cleanup on degraded print. Only the
per-page VLM token cap differs from the UI preset, using the shared manifest's
budget for comparability. Each case uses a fresh process/context, excludes initial
model downloads, and records failures instead of dropping them from the panel.
The original comparison completed all 15 planned cases: 11 returned text and
four reached the 1,200-second limit. Every failure remains in
[`ocr-real-original.json`](../tests/quality/results/2026-10-28/ocr-real-original.json),
with source/model/input hashes and attribution. No accuracy score is assigned to
a timeout. Full predictions/logs remain in the private worker output; this
repository retains derived measurements and their hashes.

| Page | Quick CER without spaces | Quick CPU seconds | Qwen multilingual CER without spaces | Qwen CPU seconds |
| --- | ---: | ---: | ---: | ---: |
| English prose | 0.9581 | 177 | Timeout | >1,200 |
| German Antiqua | 0.9248 | 218 | Timeout | >1,200 |
| German Fraktur | 0.9548 | 158 | 0.0302 | 1,106 |
| English table | 0.9266 | 505 | Timeout | >1,200 |
| Degraded German print | 0.9657 | 91 | 0.0466 | 518 |

Quick produced almost entirely uppercase text on the English page, which is a
real case-preservation failure and explains much of its high case-sensitive CER.
Its case-insensitive word error on that page was still 0.1977. Its Fraktur and
degraded-print word errors were 0.9128 and 0.9367 respectively; it is unsuitable
for those pages. On the receipt's partial reference, the primary word recall was
0.7193 for Quick, 0.5088 for multilingual Qwen and 0.6316 for the table prompt.
The table prompt also timed out on the full table. Difficult-page cleanup gave
the same 0.0466 CER without spaces as ordinary Qwen on the degraded page; this
case shows no measured cleanup advantage.

Quick took 52–505 seconds and 1,310–5,039 MiB peak RSS. Qwen used up to 15,525 MiB;
the two clean prose timeouts used 15,525 and 14,542 MiB. Model file size is not
runtime RAM use. These measurements support explicit task choices, region retries
and review, rather than promoting a heavyweight full-page default. Character
and word metrics do not establish correct table-cell structure. Comparisons to
the retained newer-engine results share the frozen images/references/scorer;
different workers and harnesses prevent a controlled latency comparison.

## Separate native CPU pixel-budget profile

The released runtime already supports `CRISPEMBED_MAX_PIXELS` before process
startup. After the original English Qwen timeout, a separately frozen follow-up
set it to 262,144 pixels while retaining the original image/reference/model
hashes, prompt, scorer and 816-token output cap. It completed in 94.03 seconds
at 5,266 MiB peak RSS, with CER 0.0075, CER without spaces 0.0092 and word recall
0.9942. It uses the same production OCR code and released native archive; no
application runtime or default changed.

The first English result remains explicitly exploratory. The preregistered
remaining five-page confirmation completed all eight cases without timeouts.
Together the two phases cover all nine Qwen assignments on the six original
pages. They used the same native library, model files, input/reference hashes,
scorer, prompts and per-page generation limits; only the native pixel budget
changed. Source equivalence binds both phases to binary source `0eb1a37`.

| Page / task | CER | Word recall | CPU seconds | Peak RSS, MiB |
| --- | ---: | ---: | ---: | ---: |
| English prose (exploratory) / multilingual | 0.0075 | 0.9942 | 94 | 5,266 |
| German Antiqua / multilingual | 0.0228 | 0.9235 | 125 | 5,265 |
| German Fraktur / multilingual | 0.0603 | 0.8523 | 107 | 5,265 |
| English table / multilingual | 0.0862 | 0.9049 | 185 | 5,266 |
| Degraded German / multilingual | 0.1959 | 0.6203 | 98 | 5,261 |
| Receipt photo / multilingual | 0.3217 | 0.5088 | 82 | 5,260 |
| English table / tables | 0.1744 | 0.8291 | 180 | 5,266 |
| Receipt photo / tables | 0.4922 | 0.5088 | 91 | 5,260 |
| Degraded German / difficult | 0.1959 | 0.6203 | 98 | 5,261 |

Observed times were 82–185 seconds and peaks were 5,260–5,266 MiB, excluding
initial downloads and including fresh context setup per case. These are sample
measurements, not bounds for other pages, hardware or advanced configurations.
The UI preset's 4,096-token limit also differs from the frozen per-page limits
used here. The binding does not expose decoded token counts, so generation-cap
status is unknown; output length ratios and timeouts are retained.

The resource saving has an accuracy cost. Fraktur CER rose from 0.0283 to 0.0603,
and degraded-print CER from 0.0412 to 0.1959. The budgeted table prompt had CER
0.1744 versus 0.0862 for the ordinary multilingual prompt. Receipt word recall
was 0.5088 for both budgeted prompts, below Quick's 0.7193 on the partial
reference. Difficult cleanup again matched the ordinary prompt's degraded-page
score. Do not promote any of these observations to a universal task winner.

**Adoption decision:** retain the existing defaults. Offer the pixel limit as
an explicit desktop CPU resource profile, with its measured task limits. Use
full-resolution or selected-region retries when fine print matters and review
recognized text against the source. Keep the original failed/slow runs alongside
the follow-up. Plans, complete derived results, input/model hashes, license
attribution and separate source receipts are in `tests/quality`; the combined
`ocr-cpu-profile-summary.json` preserves each case's exploratory/confirmation
phase. No model weights or inference ran on the development VPS.

A separate hosted smoke of the isolated worker at source `43782fb99d21c7c1c12f72249602744497d39b39`
returned `Cobalt warrant expires November 2031.` for the synthetic reference
`Cobalt warranty expires November 2031.` The missing final letter is a real
recognition error under this budget, even though the worker returned successfully.
The original strict fixture assertion failed; the process smoke uses the stable
identifiers and word stem while reporting the full synthetic output.
