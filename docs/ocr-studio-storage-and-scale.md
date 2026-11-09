# OCR model storage and large projects

## On-device model storage

The mobile OCR workspace includes **Manage model storage** in the on-device model setup panel. It reports downloaded payload bytes, reclaimable bytes, installed models and models that the running application has used.

**Clean unused downloads** removes abandoned partial downloads and superseded model generations. Cleanup also runs after a successful install and, on a best-effort basis, when the private model store initializes. Current installed generations are retained even if OCR has not used them yet.

**Remove** deletes a selected installed model after an in-product confirmation. The request carries the exact version displayed to the user. If another window installed a newer version meanwhile, removal fails with a refresh instruction. Documents, original images, reviewed text and user-managed model files outside the private model store are unaffected.

Models already used by an OCR engine remain protected until the application exits. Restart the application and remove them before recognition if their space is needed. This conservative lifetime protects engines that retain mapped weights or companion files after model resolution returns; it does not guess when a native context has released its last file.

### Storage contract

- An application mutex and a SQLite transaction serialize installs, resolution and cleanup across windows/processes. Cleanup never races an active download.
- Each used generation has a retained SQLite read transaction. Cleanup requires an exclusive transaction for that generation before removal. A process exit releases its locks automatically.
- Registry readiness checks do not retain usage locks. Explicit local paths into the managed store receive the same lifetime protection as registry names.
- Manifests identify the model by its SHA-256 directory and an immutable UUID generation. Removal checks the expected generation before changing the current pointer.
- Cleanup refuses symbolic links, unexpected subdirectories, malformed manifests and over-budget directory walks. It reports preserved entries instead of recursively deleting an ambiguous tree. SQLite lock/journal paths receive the same private-file checks.
- Removing an installed model first clears its current pointer while holding all generation locks. A crash afterward leaves unpublished generations that the next cleanup can reclaim.
- Payload size totals exclude the small control manifests and SQLite lock records. Unused generation lock records are removed after their payload, while the store lock is still held.

Native tests cover current/in-use preservation, abandoned downloads, stale-version removal, unsafe manifests/links/journals, store contention, and a child process retaining an old generation while another process attempts cleanup. Their execution belongs to the paired native workflow; frontend tests do not substitute for them.

## Large-document history

The default undo history retains at most 50 snapshots and 24 MiB of serialized UTF-8 snapshot data. The live project is excluded from this history budget. Object/renderer/native memory can exceed the serialized size; this budget is not a whole-application memory limit.

Each retained defensive snapshot now carries its byte size. Edits no longer serialize every older snapshot just to recount the budget. A reusable 64 KiB UTF-8 buffer avoids a second project-sized byte array. A snapshot larger than the entire budget is not cloned into history only to be immediately discarded. Coalesced typing remains part of the live project and is measured again when it becomes a retained snapshot.

`tests/ocr-workspace/history-benchmark.mjs` bundles the actual production model and compares it with the frozen history implementation from `78edd8101bee3651b59cc50af183385527cd92bc`. Each measurement uses a separate Node process. Both implementations must preserve the same final multilingual text, page identifiers and byte budget. Timings are recorded without a fragile wall-clock pass threshold.

Run after installing the existing JavaScript dependencies:

```sh
node tests/ocr-workspace/history-benchmark.mjs test-results/ocr-history-benchmark.json
```

The retained receipt `tests/results/ocr-studio/history-benchmark-2026-11-09.json` records source digests, runtime, fixture sizes, timings and memory. The local x64 Linux / Node 24.22.1 run measured:

| Fixture | Original median edit | Updated median edit | Original total | Updated total |
| --- | ---: | ---: | ---: | ---: |
| 1,000 pages / 4,000 regions / 24 MiB history | 180.3 ms | 37.5 ms | 4,352.5 ms | 998.7 ms |
| 5,000 pages / 20,000 regions / 1 MiB history | 338.4 ms | 154.4 ms | 7,166.1 ms | 3,219.8 ms |

Totals cover 20 edits and up to three undo/redo pairs. The smaller history budget in the second case deliberately exercises snapshots that cannot fit, leaving zero retained undo entries while preserving the edited project. The first case keeps ten snapshots within 24 MiB.

Memory did not improve uniformly. Process maximum RSS for the 1,000-page case was 305,992 KiB originally and 318,976 KiB after the change; for the 5,000-page case it was 563,100 KiB and 416,948 KiB respectively. Garbage collection and sampling affect these figures. These are model/history measurements, excluding page pixels, WebView rendering, native model inference and physical-device capture. The mobile native qualification harness records those workloads separately.
