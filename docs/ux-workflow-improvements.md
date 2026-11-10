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
| P0 | Batch additions appeared before storage completed, repeated Stop could release active workers, and sort-report cleanup could discard recoverable rows. | Persisted additions with retryable partial progress, cancellation that drains workers before unlocking, and persistence-first cleanup that retains copied originals and failed operations. |
| P1 | Optional startup failures could interrupt restoration, and Library action notifications replaced the result list with a search error. | Independent startup notices and retries; result-action feedback preserves results, preview and selection and retries the failed action. |
| P1 | Library result headers nested interactive controls, actions clipped on phones, and the preview had a desktop minimum width. | Separate native passage buttons, independent keyboard actions, wrapping touch controls and a full-width phone preview with focus restoration. |
| P0 | History import and resume cleared the saved batch before the replacement was safely stored. | Complete validation, a concrete replacement confirmation and one SQLite transaction for rows and extracted text; failed replacements preserve current rows and drafts. |

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

Library search now also fits narrow screens. Query controls and result actions
wrap, with 44-pixel phone action targets and wrapping long titles and passages.
The matching-passages control is a native button with an explicit expanded state;
using Enter or Space on another action no longer expands the document accidentally.
On screens up to 800 pixels wide, document preview fills the results area instead
of competing with it. Close restores focus to the result that opened it, retaining
selection and expanded passages. Starting another search reveals its results or
validation message. Desktop keeps the adjacent results and preview layout.

### Recover startup and document actions

Preferences, runtime capabilities and the saved batch have independent recovery
states. A failed optional preference load does not prevent the other startup
steps. Unavailable or malformed capability data keeps unsupported tools hidden
until a successful retry. A failed batch restore keeps any current work intact
and blocks new additions until restoration succeeds. Retrying does not register
duplicate listeners or repeat unrelated startup work.

Folder-watcher arrivals during failed restoration are retained in a deduplicated
queue for the current app session, including shell remounts. A successful restore
replays them. A failed replay retains the path and exposes its own Retry; it does
not restart watchers or report an addition before storage confirms it.

Library actions now report their outcome separately from search retrieval.
Copying a path, exporting a search excerpt, adding to a reading list or adding
documents to the sorter leaves the current results, preview and selection in
place. Retry runs the failed action without rerunning search. Multiple matching
passages count as one document when adding to the batch, and the result reports
new additions, existing entries and unsupported cloud items. If copying a newly
created share link fails, retry reuses that link instead of creating another.

### Add, stop and clean up batches without losing work

File additions are serialized and persisted before they become visible. Overlapping
additions of the same path produce one row. Large imports save in chunks; if a
later chunk fails, the successfully saved files remain and a retry skips them.
An arrival during another batch update waits for that update to finish.

Stop now enters a visible **Stopping…** state. Repeated clicks cannot release the
batch while extraction or analysis workers are still running. The queue waits for
those workers and the final save before another run can start. Successfully
extracted text remains available for resumption, and unfinished-only batches can
use Resume. A late progress event cannot overwrite a newer run.

The sort report keeps successful copies whose original could not be removed
visible as **Copied; original kept**. **Keep problematic** retains these files and
failed operations for follow-up. Report cleanup commits the database change before
removing rows or closing the report; a storage error leaves both available for
retry. Active processing and sorting cannot be force-reset into an overlapping run.

### Import or resume a batch safely

History validates every incoming file before offering to replace the current
batch. The confirmation names the source, incoming and current file counts, and
the current unapplied draft count. Cancel and Escape retain the current batch;
keyboard focus returns to the initiating action. Empty imports also require
confirmation. Loading, failed history reads and failed operations have distinct
states and retry controls, and History actions fit narrow screens.

Replacement holds the batch update lock through reading, confirmation and the
native commit. One SQLite transaction stages extracted text, replaces rows and
restores text. If any step fails, rollback preserves the previous rows and full
text. The visible batch changes only after that transaction succeeds; drafts for
removed files are pruned afterward. Stored text is reused only when the exact
source path, recorded size and modification time match. Explicit imported text,
including an empty string, takes precedence. Reused IDs for different sources
do not inherit unrelated text.

JSON export includes applied metadata and the full stored extracted text, even
when the current view has not loaded that text yet. The UI explains that unapplied
metadata drafts remain in the app. Import normalizes saved in-progress statuses
to unfinished work, and a failed import or resume stays in History for retry.
See [transfer validation](../src/lib/batch/sessionTransfer.ts),
[History](../src/lib/components/History.svelte), and
[the native transaction](../src-tauri/src/batch_session/mod.rs).

### Preserve proofread text when a recognition retry finds nothing

An OCR zone retry that returns no regions, or only whitespace, now leaves the
project unchanged and explains that the existing text was preserved. It keeps
corrections, approval, provenance and Undo/Redo history. The zone merge helper
independently enforces this behavior; an explicit region deletion remains a
separate action. The regression covers failed and empty retries, successful
recognition, Undo, persisted project revisions and tab navigation in the current
OCR workspace.

## Verification snapshot

Changes are committed and pushed incrementally to
`feat/ux-workflow-essentials`. The first complete workflow increment at
`96813ae2` passed its browser and full native checks. The first recovery checkpoint
at `34134db4` subsequently passed all 57 browser scenarios. Later commits add
batch cancellation, partial-import, phone Library and session-transfer coverage;
[the checks on PR #37](https://github.com/CrispStrobe/CrispSorter/pull/37/checks)
record the result for each exact commit.

Local verification after the atomic replacement implementation at `82041212`
passed all **219 frontend unit tests in 32 files**, including 18 new validation
and replacement-contract tests. Seven native SQLite replacement regressions cover
rollback and retry, full and empty text, exact source identity and invalid input;
their execution is tracked by the native CI on the implementation commit.

| Check | Observed result and scope |
| --- | --- |
| Type checking | `npm run check`: zero errors and 54 existing warnings at the passing recovery checkpoint. |
| Frontend unit tests | 201 tests in 30 files passed at `34134db4`. |
| Production bundle | `npx vite build` passed for the first complete workflow increment. |
| Full-app browser execution | [48 of 48 passed at `96813ae2`](https://github.com/CrispStrobe/CrispSorter/actions/runs/42784281098); [57 of 57 passed at `34134db4`](https://github.com/CrispStrobe/CrispSorter/actions/runs/42799789873). |
| Native desktop CI | [Linux, Windows and macOS passed at `96813ae2`](https://github.com/CrispStrobe/CrispSorter/actions/runs/42784281278). Linux reported 1,646 passed and 46 ignored, including the filtered-search candidate-window regression. |
| Isolated duplicate guard | All five filesystem tests passed on the remote Unix runner, including at `34134db4`. |
| Other existing CI | Frontend tests, extractor regressions, pin checks, license generation and enabled cloud/facade checks passed for the first complete workflow increment. |
| OCR component verification | 67 OCR unit tests and 16 Chromium scenarios passed with no captured page errors; OCR source is unchanged by the recovery increment. Desktop and compact screenshots were inspected. |
| Local browser execution | Blocked: Chromium could not create a socket under the workspace permissions. Discovery is not a browser pass. |
| Local Rust execution | Not available: native compilation and execution use the repository's remote CI. |

The [Workflow UX verification job](../.github/workflows/workflow-ux.yml)
runs frontend checks/tests, Chromium browser regressions, and five standalone
native duplicate filesystem-guard tests on its Unix runner. Those standalone
tests use a deterministic hash callback to exercise filesystem invariants; the
application command supplies streaming SHA-256. They do not require the complete
application/model build. Full native CI additionally compiles the application and
runs the actual search and batch-session persistence regressions.

Browser tests use explicit IPC fixtures to exercise persisted state, failure and
retry behavior, cancellation, hidden selections, draft recovery, duplicate
failures, complete search restoration, stale responses and keyboard/focus
behavior. Phone tests check inner-panel geometry as well as the viewport, and
save screenshots for visual inspection. They do not establish native filesystem
correctness, model accuracy or physical-device behavior. The frontend bundle is
not a packaged desktop/mobile release.

## Limits and prioritized follow-ups

1. **Review the latest commit before merging.** Each follow-up runs the browser
   and native checks again. Real-device checks remain necessary for file pickers,
   platform permissions, cloud providers and desktop/mobile behavior beyond the
   fixture-backed UI.
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
