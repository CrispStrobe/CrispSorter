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

The fixture can be selected using `?scenario=review`, `?scenario=queue-failure`,
`?scenario=queue-control`, `?scenario=batch-recovery`, `?scenario=extraction-delay`,
`?scenario=project-recovery`, `?scenario=prepare-unchanged`, `?scenario=import-control`, or `?scenario=mobile-models`. The queue and
extraction scenarios hold native responses until the harness releases them. The recovery scenario includes an
unsupported saved project, and saves can be held to exercise revision races
without relying on debounce timing.
The `batch-recovery` fixture stores an interrupted two-page selection with
recognition settings that differ from the toolbar and an unrelated pending page.
The adapter can also reject saves so the harness can verify recovery after a
successful recognition result cannot be committed.
The `mobile-models` fixture starts with an empty project and no installed models.
It holds explicit install requests so the harness can exercise progress, interruption,
retry, and recognition readiness without network requests or real model weights.

## Assertions

- A recovered three-page project retains page/source identities and revision leases.
- Corrections preserve original recognition, undo/redo works, and editing invalidates
  page approval.
- The review queue traverses uncertain text and pending pages.
- Preparation that returns the same image preserves corrections, recognition
  evidence, physical dimensions, and approval while showing its returned warning.
- Incomplete output is blocked; an explicit approved page scope exports exactly that
  scope. A custom 150 DPI PDF export changes physical page points while preserving
  raster dimensions, source pixels, and text region geometry.
- Batch errors remain attached to their pages; retry processes only failed work.
- Pause/resume/stop never overlaps native operations or drops pending pages.
- Interrupted batches use their original page selection and settings, retain
  completed results, and leave unrelated pending pages untouched.
- A failed result checkpoint halts further recognition without turning a good
  result into an OCR error. Saving and resuming completes only unfinished pages.
- Starting another batch, changing projects, and Undo remain locked until the
  final batch checkpoint has actually saved, even after the native calls finish.
- Model setup is reachable in a new empty mobile workspace and never starts a
  download merely by opening the workspace or model controls. Explicit installation
  shows byte progress and locks document actions; failure remains retryable, and
  successful installation is followed by recognition with the selected engine and
  language. The phone progress screen is captured as `ocr-phone-model-setup.png`.
- Camera import rejects unsupported image formats before transferring bytes,
  preserves a valid photo, and opens preparation without starting recognition.
- Held document imports expose correlated progress and a reachable phone Cancel
  control, stay locked until the current native step finishes, and save completed
  documents while rejecting the cancelled document without starting later files.
- A failed import checkpoint stops later documents, keeps the imported pages in
  the pending project, and allows Save to retry them without importing duplicates.
- Four-corner correction exposes touch-sized keyboard-operable handles, rejects
  invalid geometry, invalidates stale text after correction, and restores the
  original review evidence with Undo.
- Measured tilt can be adjusted manually within bounds. Shadow cleanup uses the
  corrected page without repeating geometric repair; both steps remain undoable.
- Table model output becomes inert cell text, with active HTML removed.
- Delayed extraction remains attached to its captured page after navigation;
  editing and undo restore both the visible cells and exported CSV.
- Invalid project input leaves the original autosaver usable, and reopening the
  current project flushes pending edits without rolling back text or its revision.
- Confirmation dialogs suppress page navigation shortcuts and retain their target.
- Replacing after a query change never applies stale offsets to another match.
- Desktop and phone layouts produce screenshots and retain the review controls.
  On phones, Pages, Page image, and Text & tools each remain reachable; selecting
  another page changes the reviewed text, and view controls have touch-sized
  targets. The harness checks 390-pixel and 320-pixel portrait widths and an
  844-by-390-pixel landscape screen with the page preview inside the viewport.
  Fit page keeps the complete paper within its viewport, fills one available axis,
  and requires no scrolling; this catches duplicate padding deductions.

The `OCR workspace browser verification` GitHub Actions workflow runs the same
harness on OCR changes pushed to `feat/ocr-document-studio` and on relevant pull
requests, and attaches its JSON report and screenshots as
`ocr-workspace-browser-report`. This provides a browser execution environment when
local container restrictions prevent Chromium from creating its process sockets.

These tests validate interaction and integration contracts. They do not measure
recognition accuracy or substitute for native engine, PDF conformance, or installed
application testing. Photo input and transform responses are deterministic
fixtures: the browser checks the actual production controls and state changes,
while native geometry and actual text-quality tests remain separate.
