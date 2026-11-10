# Search pipeline and inspection workflow

This guide records the search, ingestion, recovery, and inspection work on
`feat/search-pipeline-20261110`. It also defines the evidence needed before
describing a retrieval configuration as a quality improvement.

The integration includes the remaining workflow changes from pull request 37
and the reviewed OCR editing and Office export work from pull request 36,
reconciled with the runtime integration already on the main branch. Runtime
source and binary pins remain aligned with the release configuration.

## Capability map

| Area | Behavior | Primary implementation and qualification |
| --- | --- | --- |
| Query language | Strict Boolean, phrases, fields, wildcard/fuzzy terms, and positional proximity; malformed input fails visibly | `crates/crisp-search-query`, `tests/search-contract` |
| Hybrid retrieval | Explicit lexical conditions and supported metadata filters constrain every retrieval lane; result hydration uses a consistent index generation | `src-tauri/src/index/search.rs`, `crisp-index-server/src/index.rs` |
| Inspection | Query help and builder, indexed-word browser, passage navigation, literal hit navigation, compact results, sorting, selection, and source preview | `src/lib/components/IndexSearch.svelte`, `SearchQueryTools.svelte`, `SearchPassage.svelte`, browser contracts |
| Search evidence | HTML, CSV, and JSON reports preserve the executed query, filters, search time, selected scope, document identities, and source provenance | `src/lib/search/inspection.ts` and its unit tests |
| Index writes | Prepared complete-document replacements, durable repair intent, shared writer leases, and recovery before ready/search | `src-tauri/src/index/ingest.rs`, `local_index.rs`, `tests/index-write-contract` |
| Source identity | Content fingerprints are distinct from owner/location occurrences; refreshes preserve an existing safe identity | `shared/source_identity.rs`, native and server storage contracts |
| Background work | Durable pending/in-flight/error state, paused recovery, explicit retry, and visible persistence failures | `src-tauri/src/bg_ingest`, `BackgroundIngestStatus.svelte` |
| Mail and web text | Bounded byte-aware MIME/mbox parsing and HTML visible-text extraction | `src-tauri/src/extractors/eml.rs`, `html.rs`, `tests/extraction-contract` |
| Remote operation | Versioned lexical capability negotiation, durable queued batches, guarded owner-scoped mutations, and safe text-index migration | `crisp-index-protocol`, `crisp-index-server`, `tests/server-search-contract` |
| Synchronization | Bounded resumable snapshot pages, inclusive completed watermarks, and policy-aware deletion propagation | `crisp-index-protocol/src/sync.rs`, server sync cursor, desktop sync contracts |
| Retrieval evaluation | Judged relevance metrics, hard constraints, exact report recipes, input digests, and recorded latency | `scripts/evaluate_search_reports.py`, `docs/retrieval-evaluation.md` |

## Query behavior

The desktop and server share the parser and analyzer. A request must parse in
full; trailing tokens, unbalanced parentheses, invalid distances, and unsupported
operator combinations do not silently become a different query. Latin accents
are folded consistently while non-Latin text is retained by the analyzer.

Examples:

```text
warranty AND NOT draft
"service agreement"
title:(invoice OR receipt)
body:(payment pre/3 due)
policy w/5 cancellation
headings:renewal
translated:"delivery date"
```

`w/N` allows either order and `pre/N` requires the left operand first. `N` is
the number of intervening words: `/0` means adjacent positions. Phrase, OR,
wildcard, and fuzzy operands have bounded expansion. Unsupported nested AND/NOT
or chained proximity operands are rejected rather than approximated. Boolean
negation uses a positive universe with exclusions, including negative-only
queries.

The parser bounds query size at 8 KiB, token count at 512, nesting at 32, and
proximity distance at 1024. Term expansion is bounded at 128 alternatives with
a bounded dictionary scan. These limits are errors the caller can act on, not
an invitation to truncate a query silently. Indexed-word browsing uses a
bounded prefix and returns live document counts.

In hybrid mode, an explicit Boolean, field, phrase, or proximity condition
remains a condition on all contributing candidates. Synonym and fuzzy rewrites
preserve token boundaries and query structure. A prepared lexical snapshot is
reused for expansion, gating, and paging. Local cache keys include the durable
Lance version as well as filters and result limits, so a completed write from
another process cannot reuse an older cached answer.

Nearest-neighbor retrieval remains approximate. Candidate limits, restrictive
filters, selected models, reranking depth, and corpus composition can affect
recall and may produce fewer results than the requested window. Exact query
semantics do not by themselves establish semantic relevance quality.

## Inspect and export a search

The search recipe is captured when a search executes. Editing the input box
afterward does not relabel existing hits or their exported report. Searches
whose responses arrive after a newer request are discarded. Relevance ties
retain backend ordering; explicit title, filename, year, or location sorting
operates on the returned document groups. Unknown years sort after known years.

The indexed-passage view displays the stored chunk, its chunk index, source
fingerprint when available, and original UTF-8 byte offsets. The find box
navigates literal occurrences in that stored text, including keyboard next and
previous actions. It does not invent literal matches for semantic or fuzzy
results. Display text is escaped before highlights are inserted; markup is
decoded only when an input is explicitly identified as marked text.

The source-file view remains available separately. The distinction matters:
the original file may have changed since indexing, and a source preview is not
proof of what a previous search actually indexed. The UI caps the inspected
passage at 256 KiB and literal occurrence navigation at 1,000 matches.

Reports can cover displayed documents or the selected displayed documents.
Hidden selections do not enter actions or exports. Reports include the result
scope and requested result window, so a bounded export cannot be mistaken for
an enumeration of every corpus match. The interactive search window is capped
at 500 results. A failed save leaves the result and selection available for
another attempt.

For relevance comparisons, export JSON with the relevance sort and no
display-only tag filter, then follow [the evaluation guide](retrieval-evaluation.md).
The evaluator rejects selected-only, reordered, incomplete, or query-mismatched
inputs. It calculates precision, recall, graded nDCG, reciprocal rank, unjudged
coverage, explicit constraint failures, and recorded request latency. No model
score is inferred from a synthetic storage or browser fixture.

## Ingestion and source integrity

Accepted work is stored before a queue operation reports success. Failed
extraction, empty text, unsupported model output, or invalid vector dimensions
must not remove a previous searchable document. Text and vectors are prepared
before the storage mutation. A complete replacement removes obsolete tail
chunks without leaving stale text-index entries.

The paired write sequence is:

1. Acquire the shared index write lease and recover unfinished intents.
2. Verify applicable file-byte receipts at the commit boundary.
3. Persist the intended document generation and repair evidence in SQLite.
4. Commit the complete document mutation to Lance.
5. Commit the corresponding full-text mutation and retire the intent.

If the vector transaction succeeds but the full-text commit fails, the intent
remains durable. Startup and guarded reads repair it before declaring the index
ready. Recovery checks generation, row, and absence receipts; it cannot use an
unrelated newer generation to justify applying old text. Both process-local and
cross-process writers use the same lease. The native contract suite injects
failure between stores and reopens the real databases to check these properties.

For ordinary local files, a source fingerprint is the SHA-256 of file bytes.
Extraction and model work can take time, so verified paths are hashed again
under the writer lease before commit. A changed or missing source causes a
retryable failure and preserves the last good index entry. Streaming hashes
avoid making another whole-file allocation. This detects changes at the checked
boundaries; it does not create an immutable operating-system snapshot of a file.

Content equality and location identity are separate. Two identical files at
different locations or under different owners remain separate occurrences.
An exact existing owner/location match can retain its established document ID,
including after a move. A new file arriving at an old path cannot take over a
moved document's ID. Explicit imported identities and Keep Both copies remain
authoritative. Legacy collisions are resolved only for a selected occurrence,
with untouched rows preserved; no global identity rewrite is assumed.

Metadata refresh is monotonic with respect to extraction level. A filesystem or
metadata scan cannot silently downgrade an already extracted document. Paired
metadata updates retain per-chunk evidence and vectors while refreshing the
searchable title, language, and related fields. Explicit skip state is distinct
from a retryable extraction failure.

## Recovery controls

The foreground queue restores accepted rows, keeps whole-job counts correct
beyond the visible page, and records errors before claiming another batch.
Acknowledgements are idempotent and scoped to the job. A late error cannot turn
an acknowledged successful row into a failed one.

Background work has a durable owner/path queue, an active-worker lease, typed
OCR settings, and coalesced follow-up requests when a source changes during an
active run. Interrupted work is recovered into a paused state. The Add and
Sources views show pending, completed, and failed work with explicit resume,
retry, pause, stop, and clear controls. Clear is refused while a worker is active.
Storage and settings failures remain visible instead of being dismissed or
reported as a successful save.

Pausing or stopping preserves accepted pending work. A currently executing
write finishes its safe boundary before the worker stops. Resuming skips work
already acknowledged as complete and retries the stored failures. Persistence
does not store account secrets in the scheduler's OCR configuration.

## Remote migration and synchronization

The server publishes a versioned full-text generation only after reconstruction
from the stored chunks succeeds. It retains the previous generation until the
new one is ready. Known chunk overlap is removed using offsets; unknown legacy
boundaries retain separate positions rather than fabricating cross-boundary
phrases. Durable repair keys prevent a failed text commit from becoming a
successful-but-incomplete search response.

Clients negotiate the query-language version and require the response header
to confirm the chosen version. A legacy server may serve supported plain
queries, but advanced syntax is refused when its semantics cannot be verified.
Unsupported remote filters fail visibly instead of being ignored. Owner and
document identifiers are scoped and URL-encoded for mutations; ambiguous legacy
identities are refused.

Sync pages use a pinned Lance snapshot and stable timestamp/owner/document
ordering. The continuation cursor is persisted only after the page's paired
local application succeeds. The completed timestamp watermark is inclusive,
so an equal-timestamp write is replayed rather than skipped. An expired snapshot
restarts from the completed boundary. A legacy response that needs continuation
without supplying a safe cursor asks for a server upgrade.

Remote deletion evidence is durable and participates in the bounded feed.
Local conflict policy decides whether to apply a deletion, preserve the local
copy, or expose a reviewable conflict. A conditional local delete checks that
the revision inspected by policy is still current. Cursor advancement cannot
acknowledge an unapplied page. See the [server guide](../crisp-index-server/README.md)
for migration, capabilities, and deployment details.

## Extraction boundaries

MIME processing decodes encoded headers, base64 and quoted-printable bodies,
and supported declared character sets. It prefers the text alternative of a
multipart message, handles nested messages, and includes textual attachments.
Binary attachment names are recorded with an explicit name-only extraction tag;
their names are not evidence that their binary contents were indexed. An mbox
is represented as a bounded document with labeled message sections.

Limits include 64 MiB per message, 256 MiB per mailbox, 10,000 mailbox messages,
2,048 MIME parts, nesting depth 32, 64 KiB headers, and 16 MiB extracted text.
Malformed encodings fail visibly. Recoverable missing delimiters carry a
warning. Plain text containing angle brackets remains plain text.

HTML uses an HTML5 parser, declared/BOM character encoding handling, entity
decoding, visible-text traversal, heading/title extraction, image alt text, and
HTTP(S) canonical/source URLs. Script, style, template, hidden, and explicitly
invisible content is excluded. It does not execute scripts, fetch resources,
or compute external stylesheets. HTML input and node counts are bounded.

Other supported formats retain their own extraction and platform requirements.
Format recognition does not guarantee fidelity for protected, damaged, unusual,
or arbitrarily complex documents. OCR and reviewed Office package fixtures
qualify specific geometry, text, review, and package contracts; they are not an
accuracy benchmark for all page layouts or languages.

## Verification and release gates

Run the focused suites from the repository root:

```sh
npm run check
npm test
npx vite build
cargo test --locked --manifest-path tests/search-contract/Cargo.toml
cargo test --locked --manifest-path tests/index-write-contract/Cargo.toml
cargo test --locked --manifest-path tests/server-search-contract/Cargo.toml
cargo test --locked --manifest-path tests/extraction-contract/Cargo.toml
python3 tests/test_search_evaluation.py -v
```

The standalone native packages compile production storage/parser code against
real databases. They intentionally exclude unrelated model adapters and the
desktop shell; unavailable model operations fail rather than returning invented
embeddings. Browser tests use controlled IPC fixtures and qualify interactions,
keyboard behavior, state transitions, and report output. Those are complementary
gates, not substitutes for native application integration.

Hosted workflows run the full native application and platform checks, reviewed
OCR and Office package contracts, browser workflows, source/runtime pin parity,
and dependency inventories. Native CI lets an active run finish while retaining
the latest pending branch revision, so frequent saved checkpoints cannot cancel
every compiler run. Before merge or release, inspect completed results for the
final commit and run the applicable platform packaging and device qualification.
Retrieval quality additionally requires a representative judged corpus and
measured model configuration; no universal quality or format-parity claim is
made by this implementation guide.
