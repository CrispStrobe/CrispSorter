# Workflow UX improvements

Review date: 10 November 2026. Baseline: `0fa27178` on `main`.
Implementation branch: `feat/ux-workflow-essentials`.

## Outcome and audit findings

CrispSorter already has a broad document toolset. The priority is to make the
existing daily workflows reliable and easy to discover. This increment implements
the improvements below on the feature branch. Review and CI are tracked in
[pull request #37](https://github.com/CrispStrobe/CrispSorter/pull/37); these notes
do not describe a published application release.

| Priority | Observed issue | Delivered behavior |
| --- | --- | --- |
| P0 | The batch duplicate panel grouped files by size or size and name, then offered deletion; it did not use the separate hash-based detection. | Content-verified groups, separate batch-removal and disk-deletion actions, verification immediately before deletion, and retained failures. |
| P1 | Library controls omitted filters from the local command. The full-text retrieval path also failed to apply most metadata filters. Saved searches restored only part of the state. | Filters reach the native command and apply before final result selection and hybrid fusion; complete saved searches and history restore their options; visible filter chips expose removal and reset. |
| P1 | Select-all compared counts rather than membership; bulk actions could include selections hidden by a filter. | Select-all uses visible membership. Bulk actions use selected visible rows, and the toolbar reports hidden selections excluded from those actions. |
| P1 | Focusing another document discarded unsaved metadata. Empty filtered results looked like an empty batch. | Recoverable per-document drafts, explicit Save/Discard, Save & next, stage-aware filters, and distinct empty/filter states. |
| P2 | Numerous destinations were hard to discover; short windows clipped sidebar actions and collapsed navigation lacked accessible names. | Searchable quick navigation, capability-aware destinations, keyboard/focus handling, accessible active states, and scrolling navigation. |

## Using the improved workflows

### Verify duplicates before cleanup

Open duplicate review from the batch toolbar. File size is now only a candidate
prefilter: every displayed duplicate group requires matching complete SHA-256
hashes. The panel reports progress, skipped files and verification failures.
Processing/sorted entries, provider documents, repeated paths and files above
the 500 MiB verification limit are excluded with reasons.

Choose **Keep this entry** to remove other entries from the batch while leaving
files on disk. **Delete other copies** presents the retained path and exact
deletion targets for confirmation. Failed checks or deletions leave their entries
in the batch for review and retry.

Deletion verifies the retained file and each target again. The native command
also checks regular-file paths, resolved-path aliases, contents, and file
metadata before removing the resolved target. This closes the gap between a
browser-side confirmation and the actual filesystem operation. See
[the duplicate workflow](../src/lib/batch/duplicates.ts) and
[the native guard](../src-tauri/src/duplicate_file_ops.rs).

### Review documents without losing edits

Title, author and year edits become per-document drafts. Drafts survive focus
and tab changes and are persisted separately from applied metadata. A draft badge
and summary make unfinished edits discoverable; storage failures remain visible
with Retry. Editing is blocked until existing drafts have loaded, so an early
edit cannot replace an unread saved draft.

Use **Previous document**, **Next document**, or **Save & next** to work through
the current view. **Save** applies the draft and recalculates the destination;
complete title/author metadata follows the existing acceptance behavior.
**Discard draft** restores the applied metadata. Enter saves a metadata input;
Escape discards its draft. If applied metadata changed after the draft began,
the details panel flags the conflict for comparison. A failed apply keeps
the draft and restores the previous metadata in the view.

The toolbar distinguishes visible selected rows from hidden selections. Bulk
review, reprocessing and removal act on the selected rows in the current view;
select-all reflects actual membership. Active batch filters can be cleared, and
an empty filtered view offers recovery rather than implying the batch is empty.
Accepted files with outstanding drafts must have those drafts saved or discarded
before sorting. Sorting still requires reviewing and confirming the sort plan.
See [draft persistence](../src/lib/batch/reviewDrafts.svelte.ts) and
[batch review](../src/lib/components/BatchReview.svelte).

### Search with reproducible filters

Local Library search now carries file type, language, year range, folder prefix,
source-URL substring, tag, audio duration and camera metadata through the native
filter contract. Fuzzy matching and synonyms reach the text retrieval path;
ColBERT and cross-modal options require the native runtime and compatible models.
Invalid year/duration ranges produce an actionable error before dispatch.

Text and hybrid search filter full-text candidates against Lance metadata before
hydration/fusion. They continue beyond an arbitrary initial candidate window to
find eligible documents. Metadata-filtered searches exclude filename-only
catalog matches, whose records cannot establish those metadata predicates; the
filter panel explains this distinction.

Bookmarks and recent history now preserve query, mode, limit, filters, smart
parsing and explicit document scope. Existing bookmarks restore clean defaults
instead of inheriting unrelated current filters. Equal query text with different
options produces distinct bookmarks. Initial restoration and bookmark writes
are guarded against overlapping replacement, with Retry on restoration failure.

Filter chips remain visible when the options panel is closed. Removing a chip or
resetting filters changes the search configuration; after an earlier search, a
message explicitly asks the user to search again to refresh results. Unsupported
remote filter combinations produce clear errors rather than silently returning
unfiltered matches. A failed optional reranker leaves the original results
visible. Clearing or replacing a search ignores stale result and suggestion
responses. See [search recipes](../src/lib/search/recipe.ts),
[search UI](../src/lib/components/IndexSearch.svelte), and
[native retrieval](../src-tauri/src/index/search.rs).

### Find a workflow from the keyboard

Open **Quick navigation** from the sidebar, the mobile Tools menu, or
**Ctrl/Command + K**. Search a tool name or a task such as “scan”, “models” or
“rename”; English and German task terms are supported. Arrow keys navigate,
Enter opens a destination, and Escape closes the dialog and restores focus.
Unavailable capabilities are excluded. The sidebar can scroll in short windows,
and collapsed navigation retains accessible labels and active-state semantics.
Tab and Shift+Tab cycle explicitly between the search input and Close; arrow
keys select destinations. The expanded sidebar keeps the full English or German
trigger label above its shortcut instead of clipping it into one crowded row.
See [navigation definitions](../src/lib/navigation.ts).

### Read and review files on a phone

The compact batch table fits selection, status, acceptance and filename into the
viewport. Filenames wrap and act as explicit buttons for opening details. Other
metadata is available in the details pane, which scrolls independently below the
list. Bulk actions wrap instead of disappearing offscreen. The responsive table
uses column identities rather than positional hiding, and retains desktop column
widths when the window grows again.

This fixes a concrete issue in the first CI screenshot: hiding desktop metadata
columns left the desktop table width in place, spreading the remaining columns
so far apart that the filename was entirely outside a 390-pixel viewport.

### Preserve proofread text when a recognition retry finds nothing

An OCR zone retry that returns no regions, or only whitespace, now leaves the
project unchanged and explains that the existing text was preserved. It keeps
corrections, approval, provenance and Undo/Redo history. The zone merge helper
independently enforces this behavior; an explicit region deletion remains a
separate action. The regression covers failed and empty retries, successful
recognition, Undo, persisted project revisions and tab navigation in the current
OCR workspace.

## Verification snapshot

Changes were committed and pushed incrementally to
`feat/ux-workflow-essentials`. The first completed **Workflow UX verification** run
at `a096e285` passed 37 browser tests and all five isolated native duplicate guard
tests. Its seven browser failures exposed a palette focus problem, overly broad
or mismatched test selectors, a short cold-start wait, and OCR fixtures still
targeting the previous workspace. The follow-up corrects those cases and adds
the mobile layout and empty OCR retry protections described above. Consult the
[checks on PR #37](https://github.com/CrispStrobe/CrispSorter/pull/37/checks)
for the latest rerun and full native compilation results.

| Check | Observed result |
| --- | --- |
| `npm run check` | Passed: zero errors; 54 pre-existing warnings. |
| `npm test` | Passed after the OCR guard: 199 tests in 29 files. |
| `npx vite build` | Passed: frontend production bundle generated. |
| Playwright discovery | 48 tests across six files, including phone layout and current OCR workspace regressions. |
| Browser execution | First remote run: 37 passed, seven failed; fixes and additional coverage are awaiting the next remote run. |
| Isolated native duplicate guard | All five filesystem tests passed on the remote Unix runner. |
| Existing frontend CI | Type checking, unit tests, extractor regressions, pin checks and license generation passed remotely. |
| Local browser execution | Blocked: Chromium could not create a socket under the workspace permissions. Discovery is not a browser pass. |
| Local Rust execution | Not run: no Rust toolchain was available. |

The new [Workflow UX verification job](../.github/workflows/workflow-ux.yml)
runs frontend checks/tests, Chromium browser regressions, and five standalone
native duplicate filesystem-guard tests on its Unix runner. Those standalone
tests use a deterministic hash callback to exercise filesystem invariants; the
application command supplies streaming SHA-256. They do not require the complete
application/model build. Native search regression tests are also present,
including a matching document beyond the first candidate window; their execution
remains pending the full native CI environment.

Browser tests use explicit IPC fixtures to exercise draft recovery, hidden
selections, duplicate failures, complete search restoration, stale responses,
bookmark persistence and keyboard/focus behavior. They do not establish native
filesystem correctness, model accuracy or physical-device behavior. The frontend
bundle is not a packaged desktop/mobile release.

## Limits and prioritized follow-ups

1. **Complete execution qualification before merging.** Review the latest browser
   and native CI results, particularly the native search-filter fixture. Real
   platform checks remain necessary for filesystem and
   desktop/mobile behavior beyond the fixture-backed UI.
2. **Measure selective search on large libraries.** The new search path reuses one
   query/searcher snapshot, doubles the explored ranked prefix, and hydrates
   newly exposed candidates in batches of at most 512 IDs. A rare metadata filter
   can still require examining all matching text hits, and TopDocs memory grows
   with the deepest prefix. Large-corpus latency and memory are unbenchmarked.
3. **Keep remote filter behavior explicit.** The current remote-index protocol
   supports fewer options than the local engine. Any future parity work must
   extend and test the actual protocol before enabling those combinations in
   the UI; this increment rejects unsupported requests.

Duplicate deletion's content/path/metadata checks are **not an atomic lock
against external filesystem writers**. Another process can still modify paths
between the final check and removal. The new guard materially narrows the
existing risk; it does not claim to eliminate that race. Recognition model-quality work
and a new application release were outside this increment.
