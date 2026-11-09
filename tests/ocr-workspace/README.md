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

The fixture can be selected using `?scenario=review`, `?scenario=word-review`,
`?scenario=zone-empty`, `?scenario=zone-delay`, `?scenario=queue-failure`,
`?scenario=queue-control`, `?scenario=extraction-delay`,
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

Partial OCR fixtures honor the requested crop: returned regions remain in
full-page coordinates, only contained regions are included, and
`diagnostics.effective_zone` identifies the exact crop. The zone-empty scenario
returns no text. The zone-delay scenario holds recognition until explicitly
released, allowing page navigation before the result arrives.

## Assertions

- A recovered three-page project retains page/source identities and revision leases.
- Corrections preserve original recognition, undo/redo works, and editing invalidates
  page approval.
- Words show numeric scores only for verified character recognition evidence.
  Token, detector-only, and unaligned results remain unscored. The weakest
  character flags a word even when its mean and region score exceed the threshold.
- Green/high, amber/medium, red/low, gray/unscored, and blue/edited words have
  distinct text and background colors. Bands follow the weakest character;
  changing the project threshold updates colors and flags consistently. The
  optional score labels leave source punctuation, spacing, and text intact.
- Clicking a word or using Next word to check selects its exact Unicode text in
  the editor. Corrections clear scores for current text, preserve read-only
  original evidence, and survive save, undo/redo, and export without rerunning OCR.
- Source overlays flag low character evidence and unapproved corrections
  consistently with the review queue. Word controls fit desktop and compact
  layouts, with dedicated screenshots for both.
- The passage editor accepts multiline sentences and paragraphs, while save,
  undo/redo, and export preserve their source geometry and original evidence.
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

These tests validate interaction and integration contracts. They do not measure
recognition accuracy or substitute for native engine, PDF conformance, or installed
application testing.
