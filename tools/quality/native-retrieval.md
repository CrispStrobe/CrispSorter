# Judged native retrieval measurements

This repeatable evaluation runs the production server `SearchIndex`, LanceDB,
Tantivy, hydration, metadata constraints, and rank fusion against a complete
external judged corpus. It supplies real, hash-checked model vectors directly;
the contract harness's model stub is not called. Every test query contributes to
the reported means, including errors and zero-result queries.

The frozen public input is the [BEIR SciFact test set](https://github.com/beir-cellar/beir/wiki/Datasets).
Its archive SHA-256 is
`536e14446a0ba56ed1398ab1055f39fe852686ecad24a6306c80c490fa8e0165`.
The [dataset card](https://huggingface.co/datasets/BeIR/scifact) describes its
source and CC BY-SA 4.0 license. The model is the existing application option
[all-MiniLM-L6-v2](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2),
revision `1110a243fdf4706b3f48f1d95db1a4f5529b4d41`, with its Apache 2.0 license.
The downloader checks frozen model and tokenizer bytes. Retain downloaded
corpus text, weights, full query traces, and model output outside the checkout.

## Run

Use Python 3.11 or later and the repository's native Rust/protobuf build tools.
The input/output directories below are examples outside the Git checkout.

```sh
python3 -m venv ../retrieval-python
../retrieval-python/bin/pip install -r tools/quality/native-retrieval-requirements.txt
../retrieval-python/bin/python tools/quality/fetch_native_retrieval.py --output ../retrieval-inputs
../retrieval-python/bin/python tools/quality/prepare_native_retrieval.py \
  --data-dir ../retrieval-inputs/scifact --model-dir ../retrieval-inputs/minilm \
  --output ../retrieval-inputs/prepared --threads 2 --batch-size 16
python3 tools/quality/run_native_retrieval.py \
  --input ../retrieval-inputs/prepared --output ../retrieval-results/run-1
```

Select `--profile release` for optimized performance measurements. The default
`dev` profile reuses the focused contract build and records diagnostic native
latency; it is not release or device performance. The runner records the exact
Git revision, dirty paths, source hashes, compiler, executable hash, profile,
platform, inference configuration, and input hashes. It refuses to run if a
source changes while its executable is being compiled, and refuses to overwrite
an earlier result directory.

## What the result means

The four comparisons are shipped strict lexical search, a separately labelled
literal-any-term lexical policy, dense retrieval, and shipped hybrid retrieval.
The any-term comparison does not change the meaning of the application's strict
Boolean query mode. All comparisons use 100 returned documents and the same
owner scope, judgments, and complete corpus. Explicit language operators in the
original queries retain their normal parser semantics in shipped modes; any
error is retained in the output and receives zero quality credit.

`result.json` contains nDCG@10 with graded gains, reciprocal rank@10,
recall@100, average precision@100, failed/zero-result counts, and native search
mean/p50/p95/p99 latency. `queries.jsonl` retains per-query result identities,
scores, metrics, errors, and latency for independent inspection. Metric unit
contracts verify graded ranking, misses, and the denominator rather than merely
mirroring the runner's data flow. The receipt also records ingestion time and
the model preparation's separate inference time.

The default model uses masked mean pooling, L2 normalization, and its 256-token
limit. A document vector represents `title + newline + body`; truncation is
counted explicitly. Each abstract is ingested as one document chunk for this
public comparison. This qualifies the native retrieval component under that
recipe, not the desktop chunking/model adapter, source extraction, OCR, HTTP,
interactive rendering, multilingual documents, or a user's private corpus.
For this corpus the native benchmark uses an exact vector scan; approximate
index quality requires a separate measurement with its build settings recorded.

For an authorized private corpus, prepare BEIR-shaped `corpus.jsonl`,
`queries.jsonl`, and `qrels/test.tsv` files and give the preparation command an
explicit `--corpus-name`. Freeze judgments before running comparisons. Keep
source and query text on the authorized machine and share only appropriate
aggregate results. The scripts check unique identities, referential integrity,
positive judgments, vector lengths, normalization, and input hashes. Do not
remove failed queries or revise judgments after seeing a ranking.
