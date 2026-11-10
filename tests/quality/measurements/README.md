# Public measurement receipts

These receipts retain the denominator, failed cases, source and model hashes, and scope of actual native runs. Raw source documents, OCR predictions and per-query retrieval traces remain outside Git. The JSON receipts include hashes of those full results.

## Retrieval baseline

`retrieval-baseline-529c9885.json` measures the frozen `529c9885` production server index and retrieval modules with 5,183 scientific abstracts and all 300 judged test queries. Real 384-dimensional model outputs were prepared with the pinned model and tokenizer in the receipt. The production native retrieval harness ran all four modes against the same stored corpus. Failures and empty results count as zero in every mean.

| Mode | nDCG@10 | MRR@10 | Recall@100 | Empty results | Failed queries |
| --- | ---: | ---: | ---: | ---: | ---: |
| Strict lexical | 0.0275 | 0.0283 | 0.0275 | 289 | 13 |
| Literal any-term comparison | 0.5493 | 0.5254 | 0.8342 | 0 | 0 |
| Dense | 0.6451 | 0.6047 | 0.9250 | 0 | 0 |
| Existing hybrid | 0.4667 | 0.4385 | 0.6583 | 85 | 13 |

The baseline exposes a query-interpretation defect: natural questions can be parsed as strict constraints, including punctuation or prose conjunctions. The any-term row is an explicit diagnostic comparison, not evidence that the existing search box already applies this interpretation. Advanced syntax must retain its strict semantics; a natural-language choice needs a separate, typed query plan. No ranking weights or thresholds were fitted to these judgments.

This run uses a development build. The receipt's native timings are diagnostic and exclude query-model inference, HTTP, the desktop UI, extraction and chunking. Public scientific abstracts do not qualify a private document collection, multilingual material, OCR or a complete desktop release. See [the reproducible native retrieval procedure](../../../tools/quality/native-retrieval.md).

## System OCR output contract

`system-ocr-explicit-output.json` records all six frozen public pages from `system-ocr-panel.json`, using the actual production System Tesseract adapter, runtime 5.5.0 and pinned installed language packs. Every image, reference and language pack was checked before inference. The source receipt identifies the tested adapter hash even though the fix was still uncommitted during the run.

| Page | Character error, excluding spaces | Word error | Word recall | Native wall seconds |
| --- | ---: | ---: | ---: | ---: |
| English prose | 0.0092 | 0.0000 | 1.0000 | 2.634 |
| German Antiqua | 0.0115 | 0.0328 | 0.9781 | 2.652 |
| German Fraktur | 0.0362 | 0.1074 | 0.9195 | 1.784 |
| English table | 0.0337 | 0.0447 | 0.9592 | 7.311 |
| Degraded German print | 0.0515 | 0.1519 | 0.8987 | 1.786 |
| Receipt, partial reference | — | — | 0.2632 | 0.732 |

All six cases completed and supplied validated word offsets, boxes and native confidence from the same recognition run. These confidence values are uncalibrated. Word recall is the primary receipt metric because its reference is partial; the other receipt scores in the machine-readable result must not be presented as complete-transcription accuracy.

The first run failed on all six pages: the isolated language-pack directory had no optional named `tsv`/`txt` config files. The adapter now explicitly requests those output formats. The successful rerun preserves the exact page selection and language choices. Automatic page layout still misses much of the receipt; this is a measured remaining quality issue, not a passing quality claim. The wrapper's launcher memory includes inherited process memory and must not be used as an isolated OCR memory measurement.

The six pages are a small diagnostic panel. They do not justify changing the default engine or claiming broad OCR quality. Source and reference licenses and attribution are retained per page in the receipt.
