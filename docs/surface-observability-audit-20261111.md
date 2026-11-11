# Surface consistency and operation-log audit — 11 November 2026

## Integration boundary

The [remote ledger](remote-integration-audit-20261111.md) records all 74 live
branch heads and current PR dispositions. Merge `07548c56` incorporates the
five current SMB/storage commits on top of the shared OCR branch. Older
feature and PR work is already present through main. The historical July MAS
prototype is retained as an alternate branch; its remaining local dependency
workarounds would remove current supported functionality.

This follow-up connects the incoming code to production consumers and makes
operation outcomes observable across the CLI, GUI, browser extraction,
background ingestion and native workers. The direct-Rust SMB qualification
crate remains experimental; it is not advertised as a registered app drive.
OS-mounted SMB and other filesystem mounts use the existing LocalDrive route.

## Shared behavior and critical steps

| Surface | Consistent behavior | Logged critical steps and terminal outcomes |
| --- | --- | --- |
| CLI commands | Structured results remain on stdout; diagnostics use stderr. The global verbosity switch follows the GUI levels. | Command start and completion/failure, with the registered command name and a correlation ID. |
| CLI OCR, KIE, table, zone | Read saved app settings or the same `OcrPipelineConfig` JSON; only explicit OCR flags override it. | Policy loading/validation, selected execution route, preparation, recognition and publication. |
| GUI image and PDF extraction | One immutable selection per job; native application dispatch or capability-checked browser OCR. | Input validation, native dispatch/browser worker acquisition, page decisions, recognition and resource cleanup; failure/cancellation remains distinct. |
| Native image/PDF/TIFF extraction | Shared selected policy with explicit errors for incompatible routes; no successful truncated document after a page fails. | Resolved policy, input/page validation, digital-text/OCR decision, preparation/model-cache/pipeline stages, page failure and final result. |
| OCR Workbench | Capability gate includes installed legacy recognizers; project choices remain authoritative. | Import, project save, recognition, preparation/transforms, specialist operations, export and cancellation. |
| CPU OCR worker | Shares the same selection and output contract; its events reach the parent even after successful execution. | Worker launch, typed child events, completion/failure; child events carry the parent operation ID. |
| Background ingestion | Captures saved OCR policy; queue success follows completed extraction/indexing. | Queue processing, extraction, index write and terminal outcome. |
| Search and reports | UI errors are retained; cancellation is recognized by the exact error contract. | Search/count, summary generation, report publication and cancellation/failure. A filename containing `cancelled` is an ordinary filename. |
| Batch processing | Final persistence is part of completion; a failed checkpoint cannot emit success. | Processing, final checkpoint, successful completion/partial result, failure/cancellation and lock release. |
| Settings | Success is displayed only after the requested save finishes. | Apply/save boundary, persistence success or failure. |
| Drive download, sync pull, backup restore, archive promotion | Download to private staging through the transfer queue, validate counts, check cancellation and then publish. | Queue/attempt/progress, retry, publication and terminal result. CLI and GUI call the same file-target API. |
| Mounted/FUSE reads | Seek/range reads for local mounts; bounded disk-backed cache for other providers. | Queued reads and transfer/cache outcomes without full-document log content. |
| Output files | Shared atomic output helper for project/output publication. Watermarking finishes before publication. | Staging, writing, flush/sync, commit, completion/failure; a failed renderer leaves the previous complete output intact. |

The operation boundaries above are the audited critical paths. This does not
claim that every unrelated command in the application has newly instrumented
internal stages; all dispatched CLI commands have a top-level operation receipt.

## Event contract

Native events use a JSON object after the `operation ` message prefix. It
contains `operation_id`, `operation`, `phase`, `elapsed_ms` and an allowlisted
`context`; optional fields include `stage`, `code` and `parent_operation_id`.
Frontend events follow the same lifecycle and carry an `fe-…` correlation ID.
The raw-image IPC carries that ID into native processing. Each scope has at
most one terminal event: `completed`, `failed` or `cancelled`. A dropped native
scope without a terminal outcome records `incomplete`; unwinding records
`panicked`. Logging failures cannot change a successfully committed result.

Events contain counts, durations, dimensions, engine/format identifiers and
fixed outcome codes. New operation events do not include document text,
queries, prompts, credentials, model paths, filenames, URLs or arbitrary
error messages. Detailed errors remain available through the operation's
error result and UI. This is an allowlist for the new event contract, not a
claim that every pre-existing diagnostic elsewhere in the app was rewritten.

The GUI log panel already combines frontend entries and native `app-log`
events; frontend entries are not echoed into the native buffer a second time.
Native events also use the existing in-memory ring and stderr. CLI JSON stdout
contains only the requested result. The native pipeline's former `println!`
diagnostic is routed through the application log.

| Verbosity | Emitted events |
| --- | --- |
| `silent` | Errors only. |
| `error` | Errors and warnings. |
| `info` | Errors, warnings, operation starts and terminal outcomes. |
| `debug` | All of the above, plus stage/progress events. |

Settings serializes updates to the native threshold so rapid changes cannot
leave frontend and backend filters out of order. Worker transport is bounded
to 4 MiB and revalidates event identifiers and metadata before replay. It does
not copy arbitrary child stderr into the application log.

## Storage changes and limits

The incoming mounted-reader API had no real download consumers before this
follow-up. File destinations now use `submit_drive_download_to_file`, which
returns a byte count after publication. Consumers no longer build a full
`Vec<u8>` and then write the same content again. Each attempt has its own
same-directory temporary file; partial errors and cancellation remove it.
Verified size changes or a false provider byte count fail before publication.
Local streaming uploads use the same staging and size checks.
If a provider confirms a successful upload after cancellation was requested,
the queue reports the completed write. Cancellation before invocation prevents
the upload; a failed provider write remains a failure or cancelled retry with
its original error retained as the cause.

GUI byte-array previews are capped at 64 MiB. The existing shard-import
transport still requires the general in-memory download API and is not
covered by that cap. Small content-search
reads enforce the 256 KiB cap while reading, including a source that grows
after its metadata check. FUSE reads for mounted files seek to the requested
range, while other providers use a temporary disk-backed LRU rather than
retaining complete files in the process heap. Its disk budget bounds retained
cache entries; an oversized in-flight staging file can temporarily exceed it
and is not retained in the cache. A provider's default streaming
implementation may still buffer internally; these changes do not establish
bounded memory for every third-party adapter.

## Regression coverage and execution

Frontend unit regressions exercise policy/transport, logger filtering and
metadata, exactly one terminal result, cancellation/error classification,
worker ownership, page cleanup, Workbench capability gating and final batch
persistence. Browser regressions cover the user-visible batch checkpoint,
report cancellation classification and Settings persistence failures.

Native unit regressions exercise policy/CLI precedence, explicit false flags,
snapshot validation, model preservation, worker transport, event ordering,
redaction and atomic output outcomes. The storage contract compiles actual
production LocalDrive, queue, staging, cache and logger code in isolation;
it covers bounded reads, retries, partial errors, size mismatch, cancellation,
publication and concurrent requests. CI runs it on Linux, macOS and Windows.
The SMB fixture runner has separate Python tests for its qualification logic.

The Linux app test/build gate now enables `fuse`, which previously escaped
compilation in CI. It includes the direct Linux `libc` dependency and finishes
async provider setup before acquiring the synchronous mount registry lock.
Unit tests exercise real queue/range/cache reads, concurrent mount reservation
and mount-directory failure receipts without performing a kernel mount. The
mount lifecycle logs completion only when the blocking event loop returns;
creating its thread is not reported as a successfully established mount.

The isolated OCR qualification generator previously referenced a removed
cache function. It now imports the actual production OCR policy, preparation,
recognizer, worker, license/model resolver and logger, with direct Rust/FFI
dependencies and source hashes. Its Python regression generates all four
basic/panel/worker variants. Generation is checked separately from compiling
or running the generated native crate and does not count as recognition
qualification.

`scripts/verify_cli_operations.py` launches the actual built executable on
all three desktop platforms. It checks a complete JSON result, stderr-only
correlated receipts, global verbosity, failed input/configuration behavior,
redaction and absence of app-settings writes for a supplied snapshot. It
does not download a model or pretend to benchmark recognition quality.

Execution results, exact revision and any pending hosted checks are recorded
in PR #44. The local environment has no Rust toolchain, and local Chromium
cannot create its required socket under the workspace sandbox. Native and
browser execution therefore require the hosted workflows; source inspection
or discovery of a test is not counted as a test pass. Existing specialist,
platform and model capability boundaries remain documented in the
[OCR routing audit](ocr-routing-audit-20261111.md#capability-boundaries).
