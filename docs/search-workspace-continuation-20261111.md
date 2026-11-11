# Search workspace continuation — 2026-11-11

The recovered feature branch is `feat/search-workspace-completion-20261110`,
tracked by [pull request 42](https://github.com/CrispStrobe/CrispSorter/pull/42).
The PR targets `main`. Main revision
`de802fed5d8f7d40525877ad826fd627835394b9`, including the recent OCR review and
PDF editor changes, is integrated. The earlier recovery reviewed 71 remote
branch heads and one open PR; the follow-up reviewed the newly advanced main
and feature heads before integrating them.

## Remote checkpoints

| Commit | Purpose |
| --- | --- |
| `8117c42214ff11cce0c1b7e65aee49b025ff7083` | Integrate current main and its OCR controls. |
| `aea2bc800bcf908611510bc1f016d8b03c83820f` | Correct portable source-path tests and browser interactions for provider controls, field review, query accessibility and narrow result titles. |
| `57bd68c3a6c0311c3ed2d5b2184d7490f2d6877e` | Add explicit query interpretation, shared local/remote planning, verified query evidence, and six-mode native measurements. |
| `e4e55ab0dd39b9ea0994b5e7a98e811530b9ceb3` | Save the independently audited six-mode retrieval result and continuation report. |
| `c9b3cfd3200e6840f2ce05844bf61cea7cddb49a` | Merge main's OCR review/PDF update while preserving the feature branch's Windows member-copy and provider-list fixes. |

Each checkpoint was published to the feature branch and the local checkout was
verified against its exact remote tree.

## OCR/PDF integration follow-up

The incoming OCR/PDF source commit `73c6070` adds the empty-pipeline embedded
Tesseract default, grouped line review, a continuous confidence view, compact
controls and badges, the PDF editor reopen-loop fix and progressive thumbnails.
The older OCR engine/CPU-budget branch is already an ancestor of main and did
not need replaying. Feature fixes `1f5d470` and `b6bc7fa` remain in history.

Focused integration fixes preserve a configured recognizer when language is
Automatic, retain native line/column order and untouched word-evidence regions,
and prevent incomplete or mixed-provenance scores from qualifying a complete
line. Thumbnail runs capture session/refresh identity before asynchronous work
and discard stale document loads and tile publication after close or refresh.
Stage and thumbnail callers share one preview load for the same session
snapshot; caller cancellation does not cancel that shared owner.
The thumbnail guard does not serialize the separate native open/edit/save
lifecycle or the stage renderer.

The [OCR engine inventory](ocr-engine-options.md) documents all workbench
choices, advanced stages, extraction fallbacks, language/model rules and the
exact measured System Tesseract path. No new cross-engine accuracy comparison
or OCR execution is claimed by this follow-up.

### Follow-up verification

| Check | Observed result |
| --- | --- |
| Full frontend unit suite | 753 passed across 74 files, including 11 new language-routing regression cases. |
| Svelte and TypeScript | 0 errors; 54 existing warnings in 8 files. |
| Production Vite build | Passed. |
| Focused PDF asynchronous replay | Six controlled scenarios passed against the actual extracted component functions: shared stage/thumbnail loading, caller cancellation, close during export or parse, pending-owner replacement, and a new snapshot with the same native ID. This is not a browser or native-rendering result. |
| Diff formatting | Passed. |
| Native grouping regressions | Four new cases cover column order, untouched-region barriers, score provenance and complete score coverage. Local execution was unavailable because this replacement environment has no Rust toolchain. |

The preceding feature head `b6bc7fa` completed 28 hosted checks successfully,
with one intentionally gated cloud-live check skipped. The integrated main
checkpoint `c9b3cfd` has a successful hosted browser run; native application and
paired checks were still in progress at inspection. Those earlier successes
do not qualify the later follow-up source. The follow-up's own hosted results
must be read from its commit checks before release.

## Closed implementation gaps

### Query interpretation and Hybrid consistency

The search workspace now distinguishes **Advanced syntax** from **Natural
language**. Advanced syntax preserves the existing Boolean, field, phrase,
wildcard, fuzzy and proximity language. Natural language uses the index analyzer
to treat prose and punctuation as literal words and creates an any-word lexical
expression. It keeps the original text for model input. Empty analyzed queries,
more than 128 distinct words and expansion beyond the query byte limit fail
explicitly instead of silently dropping text.

Generated OR or fuzzy syntax does not become a user-authored mandatory Hybrid
constraint. Semantic-only candidates remain eligible for a natural question or
an expanded plain advanced query. Explicit advanced restrictions remain binding.
Owner, document scope and all other structured restrictions remain mandatory in
both cases. Local search, selected remote indexes, exact lexical counts and
reports share the same planner and synonym implementation.

Remote requests negotiate query-plan version 1, provide the original intent and
expansion flags, and require a response receipt. The server recomputes the plan
and rejects a changed lexical expression or model input. Servers lacking that
capability display an upgrade instruction for these plans. Legacy plain
advanced requests remain compatible.

Version 4 saved recipes retain the interpretation. Older recipes default to
Advanced syntax; cache and request identities distinguish interpretations.
The CLI supports `index search --query-intent advanced|natural`. The separate
Cloud and All sources workflows do not carry Natural language intent and show
a clear unsupported-operation message without executing a changed search.
If a Natural language selection is retained while Semantic mode disables the
interpretation selector, All sources explains the required mode switch; the
primary Search action remains available for the selected index.

### Query evidence

The local desktop command and authenticated remote endpoint explain the
executed recipe against the current document's actual full-text postings.
Clause trees retain full Boolean, field, fuzzy, wildcard, phrase and proximity
semantics, including matched fields. Positive source witnesses use exact UTF-8
offsets in the selected stored passage and the same analyzer as retrieval.
Failed Boolean branches and excluded words cannot become positive highlights.
Tokens clipped by unknown passage boundaries are omitted conservatively.

The response binds query, interpretation, expansion, mode, owner, document,
chunk, stored text and provenance. The UI rejects stale or changed receipts.
Image search and similarity search clear the previous textual explanation;
editing the search box does not relabel evidence from an earlier request.
Literal Find in passage and sparse passage navigation remain independent.

Semantic retrieval has passage-level evidence without invented word attribution.
Hybrid mode can use semantic similarity, but the mode itself does not prove
that a dense channel contributed to every result. Explanation does not recreate
a past ranking, rerun structured filters or prove the original source remains
unchanged. Source matching has explicit bounds; native scoring latency on large
indexes has not been qualified. See [query evidence](query-evidence.md).

### Recovered container and reporting work

The recovered branch already contained complete authenticated remote lexical
counts, facets and reports; expanded metadata filtering; bounded ZIP, TAR,
GZIP, TGZ, 7z, EML and MBOX extraction; native binary attachment dispatch;
independent parent/member documents; stable member identities; and verified
member reopening. The continuation preserved and revalidated those paths.
The capability review now documents their actual implemented scope rather than
listing them as unimplemented.

Container refreshes publish a whole replacement group atomically and retain the
previous group on failure. Member opening verifies the root and every recorded
entry before publishing a generated cache file. Existing aggregate container
records need a fresh extraction/reindex to acquire independent member documents.
Unsupported or encrypted content remains explicit coverage, not implied success.
See [extraction contracts](../tests/extraction-contract/README.md) and the
[capability review](search-workspace-completion.md).

## Executed local verification at the retrieval checkpoint

These are results observed during this continuation, not inferred from the
existence of a regression test or a pending hosted job.

| Check | Observed result |
| --- | --- |
| Full frontend units | 742 passed across 74 files. |
| Svelte and TypeScript | 0 errors; 54 existing warnings in 8 files. |
| Production Vite build | Passed; static output generated. |
| Locked native query contracts | 72 passed. |
| Locked native index/write/search/report/recovery contracts | 295 passed, 0 failed; 27.26 seconds of test execution. |
| Locked native server/search contracts | 152 passed, 0 failed; the external-corpus benchmark is a separately invoked ignored test. |
| Focused native container-cache contracts | 3 passed. |
| Report evaluation contracts | 12 passed. |
| Independent retrieval audit | All 1,800 measured records and four relevance metrics recomputed against the frozen judgments; input, model, source and executable hashes verified. |
| Browser evidence and intent scenarios | 14 scenarios discovered in the two new specs; discovery is not execution. |
| Diff formatting | Passed. |

The native suites use real index databases, actual HTTP routes and production
query/report logic. Deterministic vectors in correctness contracts qualify
candidate joins; real model vectors are evaluated separately. These harnesses
do not compile every Tauri command wrapper or qualify platform packaging.

Local Chromium launch was blocked before tests by the environment's socket
restriction. Hosted browser and full application checks were queued or pending
when the source checkpoint was published; their eventual status must be read
from the PR's commit checks. No browser or full application pass is inferred.

## Public retrieval qualification

The completed [six-mode receipt](../tests/quality/measurements/retrieval-query-intent-57bd68c3.json)
measures clean source revision `57bd68c3a6c0311c3ed2d5b2184d7490f2d6877e`.
All six modes use the same 5,183 documents, all 300 judged queries and 339
judgments, giving 1,800 measured query records. Errors and empty results remain
in every denominator. Empty-result counts include the failed-query cases.
An independent recomputation checked nDCG@10, MRR@10, Recall@100 and AP@100
for every trace and aggregate with a denominator of 300 per mode. The maximum
aggregate floating-point difference was 7.78e-16. Input coverage, original
result and trace hashes, model bytes and native source fingerprints also passed.

| Mode | nDCG@10 | MRR@10 | Recall@100 | Empty results | Failed queries |
| --- | ---: | ---: | ---: | ---: | ---: |
| Advanced Full text | 0.0275 | 0.0283 | 0.0275 | 289 | 13 |
| Diagnostic any-term Full text | 0.5493 | 0.5254 | 0.8342 | 0 | 0 |
| Semantic (dense) | 0.6451 | 0.6047 | 0.9250 | 0 | 0 |
| Advanced Hybrid | 0.4667 | 0.4385 | 0.6583 | 85 | 13 |
| Natural language Full text | 0.5493 | 0.5254 | 0.8342 | 0 | 0 |
| Natural language Hybrid | 0.6483 | 0.6134 | 0.9410 | 0 | 0 |

Natural language Full text exactly matches the diagnostic any-term policy.
Natural language Hybrid recovers semantic-only candidates: its nDCG@10 is
0.6483 and Recall@100 is 0.9410, with no failed or empty queries. The advanced
Hybrid control scores 0.4667 and 0.6583 and has 85 empty results, including 13
parser errors on the prose inputs. The four control rows reproduce the earlier
ranking-quality values; their strict language semantics are preserved.

Dense retrieval is close to Natural language Hybrid on this corpus (0.6451
nDCG@10 and 0.9250 recall). This one comparison does not establish a generally
superior retrieval method. No rank weights, queries, judgments or thresholds
were adjusted after observing results. Both Natural language rows use the
production planner with fuzzy and synonym expansion disabled.

### Observed native latency

| Mode | Median native ms | p95 native ms |
| --- | ---: | ---: |
| Advanced Full text | 59.5 | 121.2 |
| Diagnostic any-term Full text | 208.3 | 296.2 |
| Semantic (dense) | 291.7 | 500.8 |
| Advanced Hybrid | 444.3 | 2849.4 |
| Natural language Full text | 191.8 | 269.9 |
| Natural language Hybrid | 448.3 | 797.3 |

These are development-build, component-only timings on the recorded x86-64
Linux environment, with `RAYON_NUM_THREADS=2` and `OMP_NUM_THREADS=2`. The modes
run once each in the listed order, after ten unmeasured warm-up queries per
mode. This is neither a randomized performance comparison nor a repeated
release/device benchmark. Query-model inference, HTTP, UI, extraction and
desktop rendering are outside the native query timings.

Corpus ingestion took 15.59 seconds. Separate batched model
preparation took 326.35 seconds for documents and
2.92 seconds for queries; these batch totals must not be treated as
interactive per-request inference latency.

### Input and audit scope

The run uses pinned 384-dimensional MiniLM ONNX vectors, one `title + newline +
body` chunk per abstract and an exact vector scan. The 256-token model limit
truncated 3,681 document inputs and no queries. It does not qualify approximate
index builds, desktop chunking/model routing, private data, multilingual
material or OCR.

Regenerated embedding-file bytes differ from the earlier receipt even though
the pinned model, tokenizer, corpus, judgments and inference configuration match.
The controlled comparison is therefore among the six rows in this run; no
bitwise inference reproduction is claimed. Source/executable hashes, all input
receipts and every failed-query entry are retained in the committed aggregate.
The full result hash is `8a9847c181b769a66d1b903b8aa833594d3bda5967e4c710f1e41c4d0bce68b7` and the query-trace hash
is `036324637fec6b6f2daf1da6a7f26c88e8429f64dab37ebae664ca248fc12af9`. Raw source/query text, model weights, vectors
and per-query traces remain outside Git. See the
[run procedure](../tools/quality/native-retrieval.md).

## Retained OCR qualification

The earlier six-page System Tesseract measurement remains matched to the current
production adapter source: `ocr_word_evidence.rs` has SHA-256
`ba18e7cf6ff20e5d30e3e4ae4521e5b203657764e0606caf5b200d46a14d1a82`.
The wrapper and both page-plan hashes also match their receipt. The receipt's
historical application revision predates the then-uncommitted fix; its explicit
adapter hash identifies the code that actually ran. No fresh OCR execution is
claimed in this continuation.

All six historical cases completed with validated words, boxes and confidence.
The diagnostic panel still shows poor automatic-layout coverage on the receipt:
word recall against its **partial** reference is 0.2632. Its error rates are not
full-transcription accuracy. This remains a measured quality limitation.

The measured harness directly calls the production recognizer. Application
page management, cropping and coordinate translation, UI, packaging and other
OCR engines were outside that panel. The source match retains the original
Tesseract 5.5.0/pinned-pack result; it does not qualify a new application binary.
See the [OCR measurement receipt](../tests/quality/measurements/system-ocr-explicit-output.json).

## Remaining qualification boundaries

Private-corpus relevance, the intended machine's release latency, full desktop
model and OCR routing, and hosted platform/browser results remain separate
qualification gates. Formats outside the documented decoder profile and
omitted media retain their stated limits. Broader release and quality claims
require those separate checks.
