# Search workspace continuation — 2026-11-11

The recovered feature branch is `feat/search-workspace-completion-20261110`,
tracked by [pull request 42](https://github.com/CrispStrobe/CrispSorter/pull/42).
The PR now targets `main`. Main revision
`537764811dc415a544ecd4be513a9599370a31c5`, including the completed OCR controls,
is integrated. A fresh review found 71 remote branch heads and one open PR.

## Remote checkpoints

| Commit | Purpose |
| --- | --- |
| `8117c42214ff11cce0c1b7e65aee49b025ff7083` | Integrate current main and its OCR controls. |
| `aea2bc800bcf908611510bc1f016d8b03c83820f` | Correct portable source-path tests and browser interactions for provider controls, field review, query accessibility and narrow result titles. |
| `57bd68c3a6c0311c3ed2d5b2184d7490f2d6877e` | Add explicit query interpretation, shared local/remote planning, verified query evidence, and six-mode native measurements. |

Each checkpoint was published to the feature branch and the local checkout was
verified against its exact remote tree. No merge into main or deployment is
part of this continuation.

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

## Executed local verification

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

The six-mode runner compares strict lexical, diagnostic any-term lexical,
dense, advanced Hybrid, Natural language lexical and Natural language Hybrid
against the same 5,183-document corpus and all 300 judged queries. It retains
parser errors and zero-result queries in every mean. The new production plans
use unchanged rank weights and no tuning to judgments.

The runner records the clean source revision, source and executable hashes,
model and dataset receipts, development profile and native thread limits.
Model inference uses the pinned 384-dimensional MiniLM ONNX configuration.
This comparison uses one chunk per abstract and an exact vector scan. It does
not qualify approximate-index builds, desktop extraction/chunking/model routing,
HTTP or rendering. Development-build latency is diagnostic, not release latency
on the user's machine. See [the run procedure](../tools/quality/native-retrieval.md)
and [measurement receipts](../tests/quality/measurements/README.md).

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
omitted media retain their stated limits. These boundaries are not evidence
of a missing implemented query, reporting or container feature, and they must
not be converted into universal quality claims.
