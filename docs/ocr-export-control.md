# Export progress and cancellation

The OCR workspace shows byte progress while exporting the selected, reviewed
pages. Its **Cancel export** button has a minimum 44-pixel touch target. It is
available during rendering and writing; pressing it changes the status to
**Stopping…** and keeps editing, import, and additional exports locked until the
native operation has stopped. Successful cancellation clears the busy state and
allows a retry. Write failures retain the original error and also release the
busy state.

Publication has one winner: either cancellation is accepted before the output
is committed, or the completed file is published. Once finishing begins, the
button is disabled. A stop that loses this race does not replace a successful
export with a cancellation message. The project checkpoint and frozen export
request are preserved through the existing replacement-confirmation flow.

## Mobile destination handoff

Mobile export first renders into private application staging. If the signal is
aborted after rendering and before the native destination picker starts, the
client reports cancellation, skips the picker, and removes the private file.
The **Saving to a destination** phase begins immediately before the system
picker and copy operation. The app's Cancel button is disabled in this phase;
the system dialog owns completion or cancellation.

The private file remains alive until the provider operation returns. The client
then removes staging on success, provider cancellation, or error. A staging
cleanup error does not hide the original export or provider error. Closing the
workspace requests cancellation of a render that is still running. An OS-owned
picker or copy that has already started cannot be stopped by the app's render
cancellation command.

## Native transport

`WorkspaceClient.exportDocument(request, options?)` accepts optional
`{ signal, onProgress }`. Existing callers may omit these options. Tracked
exports generate a UUID and subscribe before invoking native work:

| Operation | Contract |
| --- | --- |
| `ocr_workspace_export` | Existing request plus optional `requestId` UUID. |
| `ocr_workspace_cancel_export` | `{ requestId }`; returns whether cancellation was accepted. `false` means publication cannot be stopped. |
| `ocr-workspace-export` event | `{ request_id, phase, bytes }`, where the native phase is `rendering`, `writing`, or `committing`. |
| Client destination phase | Same progress shape, with `phase: "destination"`, before the mobile provider handoff. |
| Cancelled result | Native `OCR_EXPORT_CANCELLED` marker maps to a DOM `AbortError`. A completed native result remains successful even if its signal is now aborted. |

Progress is correlated by UUID. Invalid byte counts, unknown phases, events for
another operation, and events after completion are ignored. After committing,
earlier rendering or writing phases cannot re-enable cancellation. A refused
stop updates the client to finishing even if the final progress event is delayed.
The progress listener and mobile staging are retained until the render settles;
requesting a stop alone is not completion.

The UI displays B, KiB, or MiB. These are bytes reported by the writer, not an
estimated percentage or remaining duration. The native backend defines when
the output is committed; the UI does not infer success from an abort signal.

## Verification and limits

The client transport suite covers subscription ordering, UUID correlation,
invalid and late events, accepted cancellation, refused and already-committing
stops, exact native error preservation, listener failures, pre-aborted work,
mobile staging cleanup, cancellation before the picker, and an OS-owned copy
that finishes after the application signal has been aborted.

The browser fixture holds export completion independently of the AbortSignal.
Its phone scenarios check byte progress, a reachable touch-sized Cancel button,
locked controls until native completion, cancellation and retry, failed writes,
and disabled cancellation during committing and destination transfer. The
report remains explicitly `synthetic_ui_only`: this exercises the production
Svelte component through a controlled transport. It does not demonstrate native
file publication, physical-device camera use, or a cloud provider copy.

Native atomic-publication and cancellation tests qualify the backend separately.
The combined app must include both the frontend transport and matching native
command/event registration. Hosted run receipts must identify the exact source
under test; a newly added browser scenario is not a passing result until that
run completes.

## Retained hosted phone evidence

Both phone export cases passed on source `d0f9baa9d424a4aaa3d439ae08965191de715cc8`, run `42625583821`: progress and cancellation waited for simulated native completion, retry preserved the committed result, failures unlocked retry, and in-app cancellation was disabled during destination transfer. These are production-component interaction tests using a synthetic client; they do not execute native PDF rendering or an operating-system picker.

The complete historical browser run had **31 passed and 1 failed case**, with zero browser errors; all 233 frontend tests and the production build passed. The failure was the archival-format negative assertion's disabled-state query, retained unchanged and addressed separately by `76bf5d8d`. The [qualification receipt](../tests/results/ocr-mobile-ui/hosted-d0f9baa9/qualification.json), original report, export-progress screenshot, and failure screenshot retain this scope and outcome. The downloaded artifact SHA-256 is `a0ac91d50aafc3a8ee06f416a94148a0c3dc958835cffbb55e0069ab2e98835e`.
