# Search workspace: capability review and implementation

This work continues the integrated search, OCR, library, and recovery changes
on `feat/search-workspace-completion-20261110`. It focuses on reliable document
retrieval, inspectable evidence, complete saved workflows, and extraction coverage.
The implementation is tracked in [pull request 42](https://github.com/CrispStrobe/CrispSorter/pull/42).

## Integration baseline

The starting commit is `427357aba55ef45fce0b1ff1659dc79bacce7cd5` from pull
request 41. Its ancestry already contains pull requests 39, 38, 37, and 36,
including the search pipeline, reviewed OCR workspace, Office exports, and
recovery controls. The runtime update from pull request 40 is incorporated,
keeping source and binary dependency pins aligned. Main subsequently integrated
pull requests 39 and 40; its merge commit
`657b86041cafc826869d536d1ebe4bdfbcb2b2e1` is also an ancestor of this branch.

The initial review covered all 70 preexisting remote branches. The continuation
on 2026-11-11 inspected all 71 current remote heads and integrated main commit
`537764811dc415a544ecd4be513a9599370a31c5`, including the completed OCR controls
from pull request 41. Pull request 42 now targets `main`. The separate sandbox
distribution experiment remains separate. Development checkpoints are committed
to the remote feature branch.

## Capability decisions

| Capability | Implementation and expected behavior |
| --- | --- |
| Exact query language | Preserve the existing complete-input Boolean, phrase, field, wildcard, fuzzy, and positional grammar. The builder adds ordered/unordered proximity from literal words or phrases, validates both operands and distance, and respects the query byte limit. |
| Document-level retrieval | Hydration streams every stored passage for ranked document candidates, retaining one best passage per document. Long documents cannot consume a shared row allowance and erase other ranked documents. |
| Owner isolation | Keep owner/document pairs through lexical candidates, passage hydration, result grouping, selection, previews, report identities, and relevance judgments. Equal legacy document IDs must not borrow another owner's text. |
| Semantic candidate diversity | Local vector retrieval grows its candidate window until enough distinct documents are found or its bounded candidate budget is exhausted. Each document retains its best chunk. |
| Federated ranking | Each source contributes at most once per document identity. Preserve the best finite raw-score evidence separately from the fused score and use deterministic ties. Owner-aware local identities avoid conflating different documents. |
| Initial passage selection | Use parsed positive body terms and the index analyzer. Operators, excluded words, unrelated fields, and substring fragments do not receive invented literal scores. |
| Structured constraints | Local multi-tag constraints use AND semantics. Wildcard characters in folder, camera, and URL substring filter values are literal. Hash prefixes are case-normalized. |
| Remote filtering | Negotiate extension, multi-tag AND, hash-prefix, document-scope, folder, URL, audio duration, camera, translated-language, review-status, and recorded-volume constraints alongside owner, language, and year. Preserve ingest metadata, apply constraints before limits, and require the response receipt. |
| Query interpretation | Explicit Advanced syntax preserves the exact language; Natural language treats punctuation and operator words as ordinary text, retrieves any indexed words, and keeps the original model input. Generated expansion does not become a mandatory Hybrid condition. Local and compatible selected remote indexes use the same planner. |
| Saved workspace | Version 4 recipes retain query interpretation, filters, scope, sort, compact layout, preferred preview, report format, displayed tags, and tag-cloud visibility. Legacy recipes retain Advanced syntax. Request identity includes retrieval interpretation and excludes view-only changes. |
| Complete local counts and facets | Scan all matching stored rows under a consistent index lease, deduplicate owner/document identities, and count matching extensions, languages, years, and tags. Report omitted facet values explicitly. Cancel work cooperatively and invalidate stale summaries. |
| Complete local reports | Stream all lexical matches as CSV or JSON in a selected global metadata order, with query, filters, generation, source provenance, and total count. Atomic publication preserves an earlier report on pre-publication failure or cancellation and protects indexed sources and index storage. |
| Complete remote counts and reports | Use authenticated, disk-backed lexical sessions tied to exact query, owner/filters, connection, and index generation. Return complete facets and globally ordered pages; verify every page, total, and identity before atomic CSV/JSON publication. |
| Keyboard investigation | Ctrl/Cmd+Shift+F focuses search; Arrow Down enters results; arrows/Home/End traverse titles; Enter opens; Alt+Up/Down traverses previewed documents; Escape closes and restores the current document's focus. |
| Stored evidence | Navigate actual previous/next stored chunks even when only one passage was returned. Display actual offsets, stored fingerprint, and a notice if the index changed since the search. |
| Remote evidence | An authenticated, versioned endpoint returns the exact requested owner/document passage, actual sparse navigation, and current stored provenance. Clients verify response identity, the capability receipt, and the bounded response. Legacy servers fail visibly. |
| Query evidence | Evaluate the complete Boolean, field, phrase, wildcard, fuzzy, and proximity expression against the actual document postings. Highlight bounded positive witnesses in the selected stored passage with exact UTF-8 offsets. Semantic retrieval has no invented word-level attribution. |
| In-passage find | Case-sensitive and whole-word options preserve source offsets. Counts distinguish exactly 1,000 occurrences from capped results. Passage navigation retains find options and keyboard focus. |
| Result actions | Copy paths and citations with the executed query and provenance. Refine to displayed documents, remove individual display-filter chips, and recover from an empty filtered display. |
| Provider requests and saves | Bind credentials, endpoint, model, and response to immutable request inputs. Serialize secret migration before settings writes; preserve drafts made while saving and refuse plaintext fallback after a partial vault write. |
| Archive ingestion | Bounded ZIP, TAR, GZIP, TGZ and 7z extraction uses the native format/OCR dispatcher. The parent and each supported member are indexed independently; nested members retain complete provenance, stable identities and verified reopening. Desktop Add/Inbox avoids loading compressed archives into the webview. |
| Mail and presentation intake | Foreground EML, MBOX and PPTX use bounded snapshots and real parsers. MIME binary attachments pass through the native leaf dispatcher as independent documents; mailbox messages have distinct identities. Unsupported formats and unrecognized presentation media retain explicit omission coverage. |
| Portable index correctness | Export every text chunk to the portable index's full-text companion from the exported snapshot. Verify overlapping bytes and separate unknown source boundaries. |
| Retrieval evaluation | Require complete returned-window reports, consistent counts/limits, the exact executed query, relevance order, and optional document scope. Incomplete report fragments cannot qualify as complete evaluation runs. |

## Inspection workflow

1. Choose Full text, Semantic or Hybrid, then choose Advanced syntax or Natural
   language for a lexical mode. User-authored advanced constraints remain binding
   in Hybrid. Natural language preserves semantic-only candidates and all
   structured metadata restrictions.
2. Build a phrase or proximity condition. Zero intervening words means adjacent
   indexed positions; ordered proximity requires the left operand first. Press
   Search to execute the completed request.
3. Sort or compact the returned window, select documents, or narrow the display
   with tag chips. Display filtering and counts describe that returned window.
4. Open Indexed text and navigate stored passages. The independent find controls
   locate literal text occurrences. Query evidence separately shows actual
   document-level clause truth and positive source witnesses; see
   [query evidence](query-evidence.md) for its exact scope. An absent stored fingerprint is shown as
   unavailable, rather than borrowed from an older search result. Original-file preview remains separate:
   the original may have changed since it was indexed.
5. Save the query and its view preferences, or export displayed/selected results
   with the executed recipe and explicit coverage information. Local and
   compatible remote Full text searches also offer complete match counts and
   complete CSV/JSON exports.

The local interactive window is at most 500 documents; remote search accepts
at most 200. The UI offers limits compatible with the active backend. Restoring
a larger bookmark remotely adjusts the window visibly, and exported recipes
record the effective limit.

### Complete lexical counts and reports

Use **Count all full-text matches** to inspect exact matching-document
totals. This scan applies the executed lexical query, owner, document scope,
mounted-source scope, and structured filters independently of the interactive
window. It includes matches in late or sparse chunks. Each owner/document pair
counts once, and a tag counts a document once even when repeated across chunks.
Facet lists show the leading 25 values and disclose omitted and missing values;
clicking a value runs a new filtered query.

Complete exports use the same matching semantics and disk-backed deduplication.
Choose a global title, filename, year, or location order. These reports do not
invent a global relevance ranking from a truncated interactive window. Metadata
comes from the lowest matching chunk with deterministic tie-breaking; tags are
the union of matching rows. Filename-only catalog entries are outside this
indexed-text scope. JSON contains a report header and document records. CSV
contains an explicit `report` metadata record followed by `document` records,
so an empty match set still retains its query, filters, generation, and zero
count. Spreadsheet formula prefixes are escaped as literal text.

The generation identifies the paired text/vector index and mounted-source scope.
A count becomes stale when that generation changes. Export captures the selected
generation and rejects an index or source-scope change before publication.
Cancellation is accepted while scanning or writing the staged file; once atomic
publication begins, the UI waits for the save receipt. A failed or cancelled
pre-publication export removes its temporary file and preserves any existing
report. Source documents, index storage, and symbolic-link destinations cannot
be used as report destinations. Returned-window HTML, CSV, and JSON reports use
the same atomic writer.

Remote counts use the same document semantics and disclose their remote scope.
The client negotiates complete-search version 1, active full-text schema 3, and
metadata schema 2; older servers display a clear unsupported-feature error.
Generation receipts include the exact lexical query and filters, and client
tokens are also bound to the configured server URL and credential. Remote
fuzzy/synonym expansion preserves the original semantic query separately when
server-side embedding is requested.

The server materializes metadata in a private SQLite session and returns up to
200 documents / 4 MiB per page. Complete scans have a 120-second deadline,
1,000,000-document ceiling, two concurrent-build slots, eight retained-session
slots, and 15-minute expiry. Exceeding a bound is an explicit error, never a
truncated result represented as complete. Cancellation removes the session and
interrupts its SQLite worker. The client verifies scope, generation, page
continuity, global order, distinct owner/document identity, and total count;
zero-result reports retain the same metadata receipt as nonempty reports.

Remote volume IDs describe recorded source provenance. They are never inferred
from drives mounted on the desktop or treated as evidence that the remote
source is currently online. The desktop mount toggle is therefore absent for a
remote connection. The existing server bearer key grants server-wide access;
owner selection scopes results and sessions without adding per-user access
control. Semantic and Hybrid results remain explicitly bounded retrieval
windows; complete reports apply to lexical matching.

## Extraction coverage and source identity

The shared native dispatcher supports ZIP, TAR, GZIP, TGZ, 7z, EML and MBOX.
A container parent, its nested containers, each message and each supported
attachment or archive member are independent indexed documents. The parent
retains its own body or coverage summary; descendant text is never substituted
into it. A group refresh publishes the complete replacement atomically, removes
obsolete descendants and preserves the previous group on failure.

Member identity uses the root owner and root occurrence plus each step's
container kind, validated logical name and same-name occurrence ordinal.
Reordering unrelated entries or changing member bytes preserves that identity.
Receipts separately record physical positions, byte counts and exact decoded
SHA-256 values. Opening a member snapshots the complete root, verifies its
fingerprint and re-decodes every recorded step before publishing a verified
copy to the bounded managed cache. A changed source requires refresh.

Member names are provenance, never extraction destinations. Nested containers,
MIME attachment decoding and document packages share count, depth, expanded-byte
and text limits. Default limits are 128 MiB archive input, 256 MiB mailbox input,
32 MiB per decoded member, 128 MiB total expanded bytes, 16 MiB extracted text,
2,048 inspected entries and four entry-chain steps. EML retains its additional
64 MiB ceiling. ZIP/7z metadata and 7z dictionaries have separate preallocation
limits. These component budgets do not bound total process memory.

Supported binary MIME attachments use the normal native format/OCR dispatcher.
Transfer encoding, character sets, multipart alternatives, HTML bodies and
nested messages retain explicit boundaries and source receipts. Unsupported,
encrypted, linked, unsafe-name and textless entries have visible coverage;
coverage-only parents remain inspectable. Corrupt supported content or an
exhausted budget fails the expansion rather than silently losing members.

Presentation text, notes and comments are supported. Images, charts and embedded
presentation media retain explicit omissions. Foreground cancellation discards
a stale native result and does not forcibly interrupt a running synchronous
parser. See the [extraction contracts](../tests/extraction-contract/README.md)
for the exact supported decoder profile, budgets and verification boundaries.

## Text reconstruction and archive search

Offsets must describe the stored UTF-8 bytes. Overlapping chunks join only when
their shared bytes agree. Unknown offsets and actual gaps remain separate
positional regions so they cannot manufacture phrase or supported proximity
matches across missing text. Contradictory ranges fail before publication.

The durable full-text write intent carries these regions. Old intents without
the additive region field still deserialize; new intents retain their semantics
during repair. Portable text companions follow the same rules and use the
exported data snapshot, including translated text where available.

The pending version-103 full-text schema upgrade now reconstructs all stored
owner/document chunks into a sibling staging index. It verifies the committed
document count before publication and keeps the old index until the directory
swap. Recovery markers distinguish unfinished staging from completed work;
restart restores a displaced old directory and rebuilds incomplete staging.
Missing or empty full-text storage with existing Lance rows is rebuilt rather
than mistaken for a fresh installation. Field review releases the configuration
lock before awaiting the paired index lease, allowing report publication and
metadata updates to finish without a lock-order deadlock.

Previously exported `.cidx` files remain immutable: regenerate them to obtain
the complete late-chunk text companion. Existing local full-text postings
receive the new boundary rules when affected documents are refreshed, imported,
or reindexed. Updating the application alone does not rewrite historical
postings or establish that every older archive contains all source passages.
An already-completed historical schema upgrade is not silently rerun. A partial
current-schema index produced by an older version without recovery markers is
not reliably distinguishable from a deliberately small index; use refresh or
reindex to reconstruct its postings from the stored or original content.

## Verification

The verification layers cover frontend state and types, production builds,
actual native text/vector/journal databases, HTTP client/server semantics,
native archive/MIME/HTML/text extraction, browser interactions with controlled
IPC, and evaluation metrics with hand-calculated expectations.

```sh
npm run check
npm test
npx vite build
npx playwright test
cargo test --locked --manifest-path tests/search-contract/Cargo.toml
cargo test --locked --manifest-path tests/index-write-contract/Cargo.toml
cargo test --locked --manifest-path tests/server-search-contract/Cargo.toml
cargo test --locked --manifest-path tests/extraction-contract/Cargo.toml
python3 tests/test_search_evaluation.py -v
```

Standalone native packages compile production logic without downloading models
or building the unrelated desktop shell. Browser tests exercise controlled IPC
contracts. Platform application builds, packaging, rendering engines, and real
model inference need their corresponding hosted/device gates. See pull request
42 and its final commit checks for actual execution results; a regression test's
existence is not evidence that it ran on every platform.

### Executed qualification, 2026-11-10

| Check | Observed result |
| --- | --- |
| Complete frontend unit suite | 660 tests passed across 67 files. |
| Svelte and TypeScript check | 0 errors; 54 existing warnings in 8 files. |
| Production frontend build | Passed with the static adapter output generated. |
| Native query contract | 61 tests passed using the lockfile. |
| Native index, search, export, and recovery contract | 294 tests passed using the lockfile and default thread stack. Includes the production search engine, migration framework, federated rank merge, and CLI result mapper. |
| Native server/search contract | 79 tests passed using the lockfile and default thread stack. |
| Native extraction contract | 83 tests passed using the lockfile. |
| Retrieval evaluation contract | 11 tests passed. |
| Diff formatting | `git diff --check` passed. |
| Browser interactions | Updated scenarios were discovered. Local execution was blocked by Chromium socket restrictions; the alternate browser download also failed. No local browser pass is claimed. |
| Hosted browser and application builds | Queued or pending at final handoff; inspect the PR's latest commit checks for their eventual result. |

The subsequent remote-reporting checkpoint reran the complete frontend suite
(678 tests across 70 files), the locked native index suite (299 tests), and the
locked server suite (146 tests), all successfully on the default thread stack.
The focused summary/recipe tests passed 17 cases; the type check retained
0 errors and 54 existing warnings. Seventeen search-workspace browser scenarios
were discovered, including three remote reporting flows. Discovery does not
claim browser execution. The server suite includes real HTTP/storage sessions
over 633 documents and production client/atomic-report contracts.

The native packages use actual index databases, extractors, query execution,
and report writers. The search-engine tests use a fixed query vector where
dense candidate joins must be deterministic. They do not exercise model
inference or establish retrieval quality. Tauri command wrappers, application
packaging, and platform rendering remain outside these standalone packages.

## Boundaries that remain explicit

The supported extraction families, source receipts and lexical explanation
semantics are implemented. These remaining boundaries still constrain claims:

- Formats and codecs outside the documented native decoder profile, encrypted
  archive content and omitted presentation media retain explicit coverage limits.
- Source spans explain the selected stored passage and current index. They do
  not reconstruct missing cross-passage bytes, a past ranking score or the
  unchanged state of the current original file. Semantic word attribution is
  deliberately absent because passage similarity does not establish it.
- Natural language and advanced expanded Hybrid plans require the negotiated
  query-plan capability on a selected remote index. The separate Cloud and All
  sources workflows do not transport Natural language intent and reject that
  selection visibly. Semantic mode does not apply lexical query interpretation.
- Public-corpus measurements qualify the recorded components, model, data and
  hardware. They do not qualify a private corpus, desktop OCR/model integration,
  every language or format, or production latency on the user's hardware.
- Local browser execution is restricted in this environment. Hosted browser
  and full application checks must be assessed at the actual feature head.

See the committed [measurement receipts](../tests/quality/measurements/README.md)
for observed results. These boundaries must not become broader quality claims
without evidence.
