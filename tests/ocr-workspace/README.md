# OCR workspace browser verification

This harness mounts the production `OcrWorkbench.svelte` component with its typed
`WorkspaceClient` adapter. All source pages and recognition responses are synthetic
fixtures. It exercises the actual UI, project model, queue and autosave code without
native model downloads, user files, or a desktop runtime.

## Run

Install the repository's normal npm dependencies, run `npx svelte-kit sync`, and
make Playwright available (from an isolated install through `PLAYWRIGHT_MODULE_PATH`,
locally as a development tool, or in the configured primary runtime). Use a
Playwright Chromium installation, a system `/usr/bin/chromium`, or set
`OCR_SMOKE_CHROMIUM` to the browser executable.

```sh
node tests/ocr-workspace/smoke.mjs
```

`PLAYWRIGHT_MODULE_PATH` is the absolute package directory, for example
`/path/to/browser-tools/node_modules/playwright`. The workflow installs its pinned
browser tools outside the repository so they cannot replace locked application
dependencies.

The browser uses the host's default temporary directory. If a restricted runtime
requires a different location, set `OCR_SMOKE_TMPDIR` to a short writable absolute
path. Do not point it into a deeply nested artifact directory: Chromium uses Unix
socket paths whose length is limited. Browser launch failures are recorded in
`report.json` as a `browser setup` failure, with no interaction tests marked as
passed; the workflow uploads that report even when screenshots cannot be captured.

The script starts an isolated Vite server, runs Chromium headlessly, and writes
`report.json` plus screenshots to the ignored `tests/ocr-workspace/artifacts/`
directory. A failed assertion or browser exception causes a nonzero exit code.
Use `--output=/absolute/path` to choose another artifact directory.

For interactive inspection:

```sh
node tests/ocr-workspace/smoke.mjs --serve --port=4178
```

The fixture can be selected using `?scenario=review`, `?scenario=word-review`, `?scenario=native-word`,
`?scenario=proofreading`, `?scenario=proofreading-save-failure`, `?scenario=proofreading-load-failure`,
`?scenario=text-repair`, `?scenario=structure`, `?scenario=page-draft`,
`?scenario=zone-empty`, `?scenario=zone-delay`, `?scenario=queue-failure`,
`?scenario=queue-control`, `?scenario=extraction-delay`, `?scenario=extraction-objects`,
`?scenario=table-continuation`,
`?scenario=project-recovery`, or `?scenario=prepare-unchanged`. The queue and
extraction scenarios hold native responses until the harness releases them. The recovery scenario includes an
unsupported saved project, and saves can be held to exercise revision races
without relying on debounce timing.

The word-review scenario supplies synthetic, explicitly aligned Unicode character
recognition scores, plus contradictory token metadata and detector-only evidence.
It verifies the UI contract for capable adapters; it does not assert that current
native engines supply verified per-word confidence. Its uncertain word has a high
mean score but one low character score; other words cover medium and high scores.
A leading emoji exercises UTF-16 text selection offsets, and punctuation and
whitespace must survive the flowing colored-text display unchanged.

The native-word scenario supplies synthetic `tesseract_tsv` word evidence with
exact UTF-16 spans and full-page pixel boxes. It preserves a hyphenated native
token and a leading emoji outside the measured words. A 42% native score and a
null native score both take precedence over 99% character scores; the null score
retains its source box without acquiring a fabricated percentage. This fixture
does not execute Tesseract. Live engine output and parser qualification are
recorded separately using the [workspace qualification guide](../quality/ocr-workspace-qualification.md).

The proofreading scenarios reuse the native word evidence and add independent
user-vocabulary storage. Failure scenarios retain an existing user term while
failing the first load or save. The harness can inspect successful user saves
separately from project autosaves, so project Undo cannot masquerade as a global
preference rollback and failed loads cannot silently replace existing vocabulary.

The text-repair scenario contains a single synthetic multiline passage with extra
inline spaces, a tab, a lowercase end-of-line hyphen, a paragraph break, a
hyphenated name, an identifier, and a list item with a negative number. Repairs
must preserve unselected content and original character evidence; the fixture
does not imply semantic proofreading or model retraining.

The structure scenario supplies eight synthetic source regions: a heading
candidate, an emoji-prefixed multiline paragraph, a list candidate, two body
passages, a picture, a caption, and a note. Their two-column source layout is
independent of the logical structure authored in the UI. The `[1]` note marker
is linked explicitly; proximity and marker text alone do not create a link.
Formatting tests use the isolated browser's clipboard for a real plain-text
paste and verify exact UTF-16 ranges before and after it.

The page-draft scenario uses the same source layout with a CRLF passage and
existing heading, list, inline formatting, caption, and note metadata. The native
textarea Undo test must affect only the unapplied draft, while Apply/project Undo
restores OCR text and structure together. Recovery tests switch to a new project
and actually reload the saved fixture through the client; merely reopening the
current-project shortcut would not exercise deserialization and draft recovery.

The extraction-objects scenario supplies two independent table areas, two formula
areas, and reviewed text for a field set. The first table has a merged header;
the second includes a leading-zero identifier. Responses are distinct, repeatable
synthetic model outputs. The fixture checks source capture and object persistence;
it does not measure native table, math, or field recognition quality.

The table-continuation scenario seeds three saved table objects through the
production recognition and extraction helpers, one on each source page. Two
tables share a two-column header; the third has three differently ordered
columns. Exact leading-zero strings and distinct source images make column
mapping, header removal, ordering, and captured-evidence assertions observable.
The fixture does not infer a continuation or run a native model.

Partial OCR fixtures honor the requested crop: returned regions remain in
full-page coordinates, only contained regions are included, and
`diagnostics.effective_zone` identifies the exact crop. The zone-empty scenario
returns no text. The zone-delay scenario holds recognition until explicitly
released, allowing page navigation before the result arrives.

## Assertions

- Enabling versioned structure retains legacy passage text, geometry, source IDs,
  and original OCR evidence. Roles, heading/list properties, alignment, and breaks
  save separately from the text.
- Formatting a selection after an emoji uses exact UTF-16 offsets. A plain-text
  paste removes formatting on replaced text and preserves the provably unchanged
  suffix; Ctrl/Cmd+I formats a selection, and Ctrl/Cmd+Z/Shift+Z restore text and
  formatting together while the structure textarea has focus.
- Reading-order changes undo independently. Explicit sections/column boundaries,
  figure descriptions, caption anchors, and selected note references survive
  save, undo/redo, and JSON export without moving source-image boxes.
- Page text drafts persist without changing OCR text or approval, support native
  clipboard and Undo, and survive project reload. Preview cancellation and a
  stale captured source retain the draft. Applying a single-passage change keeps
  its ID/evidence and preserves unchanged CRLF; project Undo also recovers the draft.
- A draft edit cannot cross an omitted picture. A valid cross-passage edit needs
  explicit combined-coverage acceptance and creates one manual paragraph, with
  source components retained and unaffected passages unchanged. Its line breaks,
  source history, and remaining structure survive save, project Undo/Redo, and JSON.
- A recovered three-page project retains page/source identities and revision leases.
- Corrections preserve original recognition, undo/redo works, and editing invalidates
  page approval.
- Words show numeric scores only for validated native word evidence or verified character recognition evidence.
  Token, detector-only, and unaligned results remain unscored. The weakest
  character flags a word even when its mean and region score exceed the threshold.
- Green/high, amber/medium, red/low, gray/unscored, and blue/edited words have
  distinct text and background colors. Bands follow the weakest character;
  changing the project threshold updates colors and flags consistently. The
  optional score labels leave source punctuation, spacing, and text intact.
- Clicking a word or using Next word to check selects its exact Unicode text in
  the editor. Corrections clear scores for current text, preserve read-only
  original evidence, and survive save, undo/redo, and export without rerunning OCR.
- Native words retain provider token boundaries, direct score semantics, and
  their genuine source boxes. Selecting a word highlights and magnifies that
  box; an unscored native word still has its box. Corrections clear stale source
  selection. Keyboard inspection of original evidence restores the original
  crop without changing the corrected text. Save, history, and JSON export keep
  the original word envelope, with no current evidence attached to edited text.
- Vocabulary, ignored suggestions, and protected identifiers preserve native
  confidence. Accepting a vocabulary suggestion previews the exact replacement,
  cancellation leaves text unchanged, and one Undo restores an accepted edit
  while user-wide vocabulary remains saved.
- Literal rules respect protected identifiers, approved text, and the chosen
  page scope. A preview can select one occurrence without changing other matches;
  the resulting text edit is one undoable action.
- A failed user-vocabulary save retains the draft for explicit retry. A failed
  load disables user-wide edits until retry succeeds and never overwrites the
  unread existing preferences.
- Source overlays flag low character evidence and unapproved corrections
  consistently with the review queue. Word controls fit desktop and compact
  layouts, with dedicated screenshots for both.
- The passage editor accepts multiline sentences and paragraphs, while save,
  undo/redo, and export preserve their source geometry and original evidence.
- A caret split requires an explicit source-coverage choice. Shared children keep
  exact text, including leading whitespace, and an empty boundary separator.
  Repeated split/merge operations retain the same flat source components without
  duplication. The source inspector exposes original text, native word evidence,
  and the captured image. Adjacent merges preview their chosen separator and
  preserve exact strings and source records through history, save, and JSON export.
- Text-repair previews do not alter a passage until accepted. Spacing changes,
  line joins, and dehyphenation begin unchecked. The selected repair preserves
  tabs, paragraph breaks, names, identifiers, list markers, and negative numbers;
  one Undo restores the original, with source scores retained in JSON export.
- Partial OCR presents current and proposed text before applying changes.
  Cancel preserves corrections; acceptance replaces only the covered regions;
  Undo restores the prior result. Drawing a small area expands to its complete
  source passage, and Escape cancels an unfinished gesture without recognizing.
- Empty partial results retain corrected text and approval. A delayed result
  stays attached to its captured source page after navigation, and the preview
  blocks page-navigation shortcuts until the comparison is resolved.
- The review queue traverses uncertain text and pending pages.
- Preparation that returns the same image preserves corrections, recognition
  evidence, physical dimensions, and approval while showing its returned warning.
- Incomplete output is blocked; an explicit approved page scope exports exactly that
  scope. A custom 150 DPI PDF export changes physical page points while preserving
  raster dimensions, source pixels, and text region geometry.
- Batch errors remain attached to their pages; retry processes only failed work.
- Pause/resume/stop never overlaps native operations or drops pending pages.
- Table model output becomes inert cell text, with active HTML removed.
- Two tables, two formulas, and a field set append as five independently selectable
  objects. Each retains its own captured source, exact raw response, and original
  structured data. Merged-cell spans are retained without invented cell boxes.
- A multiline table-cell correction affects only its selected object. Focused
  Ctrl/Cmd+Z/Shift+Z restores the project object as well as the visible cell;
  save and an actual project reload retain the correction.
- Selected CSV, LaTeX, and field JSON exports use the selected object's current
  data. CSV preserves multiline text and merged covered positions; field JSON
  includes the complete object's original output, baseline, and source captures.
  Document JSON preserves the entire versioned object collection.
- Table-structure previews expose the complete text of fully removed cells;
  cancellation leaves the current object unchanged. Inserting and deleting rows
  and columns preserves leading zeros, cell strings, and unaffected source data.
- A rectangular merge previews its explicit separator and exact row-order text.
  One Undo/Redo restores the complete object. Splitting retains all merged text
  in the top-left cell and creates empty neighboring cells, without interpreting
  a line break as a cell boundary.
- Table repairs survive save and actual project reload with original output and
  structured data unchanged. The selected CSV uses the repaired current grid.
  The repair dialog works with keyboard activation at 390px width without
  horizontal page overflow.
- Cross-page continuation joins reject missing mappings for differing widths,
  then honor explicit per-table column destinations and chosen input order.
  Header removal is explicit, previews the exact omitted values, and warns when
  the removed header differs from the first table's retained header.
- Cancel preserves all inputs. Create adds one separate, included result on the
  first input's page without changing any input or its inclusion setting.
  Flat source snapshots preserve original model data and the current input
  values captured at joining.
- One Undo/Redo restores the entire join. Save/reload retains the result, source
  snapshots, and initial joined structure; CSV preserves the selected column and
  row order. A later input correction marks the join historical while keeping its
  saved strings and source snapshots unchanged.
- Changing page pixels retains earlier extraction objects as historical source
  records with their corrections and captured images still inspectable.
- Delayed extraction remains attached to its captured page after navigation;
  editing and undo restore both the visible cells and exported CSV.
- Invalid project input leaves the original autosaver usable, and reopening the
  current project flushes pending edits without rolling back text or its revision.
- Confirmation dialogs suppress page navigation shortcuts and retain their target.
- Replacing after a query change never applies stale offsets to another match.
- Desktop and compact layouts produce screenshots and retain the review controls.
  Fit page keeps the complete paper within its viewport, fills one available axis,
  and requires no scrolling; this catches duplicate padding deductions.

The `OCR workspace browser verification` GitHub Actions workflow runs the same
harness on feature-branch changes and attaches its JSON report and screenshots as
`ocr-workspace-browser-report`. This provides a browser execution environment when
local container restrictions prevent Chromium from creating its process sockets.

The [recorded browser run for commit 46a3e04719ef36e09bac4cd6c6d0bcb95f5a4cab](https://github.com/CrispStrobe/CrispSorter/actions/runs/42692620197/job/129416051499)
completed on 2026-11-09 with **32 browser interactions passed, 0 failed** and
**201 OCR unit tests passed**. It includes native word selection/source evidence,
vocabulary and correction workflows, passage splits/merges and repairs, document
roles and inline formatting, clipboard/keyboard history, columns, and caption/note
links. Its report and screenshots are available in
[artifact 13709565396](https://github.com/CrispStrobe/CrispSorter/actions/runs/42692620197/artifacts/13709565396).
This result is specific to that source revision.

The later full-page draft revision `ed0480a7e7bfab6f0c7b92bb5fb802689fa6fbe5`
also completed its [browser job](https://github.com/CrispStrobe/CrispSorter/actions/runs/42696088961/job/129428191684)
on 2026-11-09: **34 browser interactions passed, 0 failed**, **343 unit tests in
38 files passed**, and **0 Svelte errors with 54 existing warnings**. Both
page-draft flows passed, including native draft Undo, durable reload and stale
recovery, hidden-source barriers, and explicit combined-source application.
Its report and screenshots are in
[artifact 13711038096](https://github.com/CrispStrobe/CrispSorter/actions/runs/42696088961/artifacts/13711038096).

The original OCR-05a revision `4904354fb21d32cd7a73d8ca6a7192603da8c772`
completed its [browser job](https://github.com/CrispStrobe/CrispSorter/actions/runs/42699692705/job/129445404867)
with **33 interactions passed and 2 failed**. Its **379 unit tests in 41 files**
passed, with **0 Svelte errors and 54 existing warnings**. The failures were the
new lifecycle test comparing capture bounds against a nonexistent nested region
field, and the delayed table test receiving object JSON instead of CSV. Both were
already corrected in `5ecd539612465e47831b17757ffbe28a29cde2c5`; the original run
does not qualify that correction. Its failed report and screenshots remain in
[artifact 13713200211](https://github.com/CrispStrobe/CrispSorter/actions/runs/42699692705/artifacts/13713200211).

The export-dispatch and bounds correction at `5ecd539612465e47831b17757ffbe28a29cde2c5`
passed **382 unit tests in 42 files** locally, with **0 Svelte errors and 54
existing warnings**. Script syntax and whitespace checks passed. Local Chromium
still cannot create its required Unix socket; no interaction pass is claimed
from that launch attempt.

The OCR-05b table-repair checkpoint `691be05` passed **405 unit tests in 43 files**,
including 23 new operation cases, and the Svelte typecheck with **0 errors and the
same 54 existing warnings**, both locally and in CI. Its
[browser job](https://github.com/CrispStrobe/CrispSorter/actions/runs/42701880471/job/129452159224)
passed **34 interactions and failed two**. The lifecycle test had not switched
from Extract to Review before approving the page. The compact repair test exposed
a Save button whose hidden label removed its accessible name; its status wait
also assumed the full-width status text was visible. The correction gives Save a
stable accessible name, exposes the actual save-state value for the harness, and
navigates to Review explicitly. It does not infer a browser pass. The original
[report and screenshots](https://github.com/CrispStrobe/CrispSorter/actions/runs/42701880471/artifacts/13713598457)
retain the failed-run evidence.

The OCR-05c continuation-join checkpoint passed **421 unit tests in 45 files**,
including 12 new join-core cases and four lifecycle cases, and the Svelte
typecheck with **0 errors and the same 54 existing warnings**. Its final browser
harness passes JavaScript syntax and whitespace checks.

The harness now contains **37 browser flows**. The corrected extraction-object,
table-repair, and continuation-join flows require a successful browser run against
their own source revision before they can be reported as passing. No browser-pass
evidence has yet been recorded for the hotfix or OCR-05b revisions. These tests validate interaction and integration
contracts; they do not measure recognition accuracy or substitute for native
engine, PDF conformance, or installed application testing.
