# OCR studio continuation — 9 November 2026

This record follows the [first recovery checkpoint](ocr-studio-checkpoint-2026-11-09.md).
Work is saved incrementally in upstream feature branches and remains reviewable in
[CrispSorter draft PR #29](https://github.com/CrispStrobe/CrispSorter/pull/29) and
[CrispEmbed draft PR #189](https://github.com/CrispStrobe/CrispEmbed/pull/189).
No release qualification is inferred from a source change or a queued workflow.

## Implemented since the first checkpoint

| Area | Current behavior |
| --- | --- |
| Mobile PDF import | Android PdfRenderer and iOS CoreGraphics render bounded pages, preserve physical sizes and original source bytes, and report correlated progress and cancellation. Locked, malformed, truncated, and over-budget documents fail explicitly. |
| Phone image formats | Native decoder discovery controls HEIC/HEIF/AVIF availability. EXIF orientation is applied to working pixels; originals remain byte-identical in the private archive. Complete JPEG and ISO media container validation prevents silent partial decoding. |
| Import recovery | Each completed document is durably checkpointed. A failed save stops further imports and retains the new pages for Save retry. Starting another import first flushes pending work. Source URI and private archive URI remain distinct; duplicate originals are detected by archive identity. |
| Mobile model storage | Storage inspection, cleanup, and explicit version-checked removal are available in the workspace. SQLite usage leases protect mapped models across processes. Current installed generations and user-managed external files remain protected. |
| Large-project history | Retained snapshots carry their measured UTF-8 size, avoiding repeated serialization of all history. Oversized snapshots are not cloned only to be discarded. Undo/redo, corrected text and byte bounds remain intact. |
| Recognition settings | Interrupted batches retain the CPU pixel budget alongside their original selection, engine, language and profile. Mobile embedding providers report and select their actual CPU execution path. |
| Mobile runtime closure | Pinned official Android and iOS ONNX Runtime inputs preserve embedding/indexing capabilities alongside CrispEmbed OCR. Archive, binary, header and notice hashes are checked; build receipts and a target inference probe bind the runtime to the resulting app. |
| Native build recovery | The async registry command now returns the Tauri-compatible result type. The iOS build retains the host protobuf compiler and native dependency paths through Xcode's nested Cargo invocation. |

The existing workspace also retains confidence-aware review, page/region editing,
reading order, tables/fields/formulas, approved corrected-text indexing, explicit
export preflight, source-preserving photo correction, and resumable exact-selection
batches. See the [workspace guide](ocr-workspace.md),
[mobile import contract](ocr-mobile-import.md), and
[storage/scale notes](ocr-studio-storage-and-scale.md).

## Executed checks

Evidence is tied to the exact source that ran. A later documentation or unrelated
change does not turn these into tests of a different binary.

| Gate | Result and source | Evidence scope |
| --- | --- | --- |
| Combined frontend | 218 tests passed; Svelte check: zero errors, 54 existing warnings in eight other files | Local integration `33f456ee8c78f27b5ecb78d85aef6c88dc45b509`; includes import save recovery and mobile CPU provider changes |
| Build/probe helper contracts | 60 Python tests and five Node tests passed | Same integration source; fixture, packaging, receipt, device-selection and measurement logic; these are not target execution |
| Hosted browser | 27 scenarios and 218 unit tests passed; production build passed; nine screenshots retained | [Run 42608320490](https://github.com/CrispStrobe/CrispSorter/actions/runs/42608320490), exact source `7b5a7791c592e2e178b85724cb8cd26c5ac3558a`; real Svelte component with synthetic native adapters; predates the later import-save-failure scenario |
| Model storage | 10 native tests passed, zero failed/ignored; includes a subprocess holding a generation open | [Run 42609934442](https://github.com/CrispStrobe/CrispSorter/actions/runs/42609934442), exact source `f3cd96af7aa6ab0e1bb0b19598e1e31a9bc376ea`; actual production modules with locked dependencies |
| iOS document provider | 18 tests passed, zero failures | [Run 42609744777](https://github.com/CrispStrobe/CrispSorter/actions/runs/42609744777), exact source `dd8d9b1419e5b5d339c8a432118cb94218cdbce3`; simulator native PDF, HEIC/AVIF, orientation and failure controls |
| Android document provider | 19 tests: 18 passed, one AVIF decoder skip, zero failures | Same source and run; Android 10 emulator native PDF/image/provider contracts. A skipped codec is not a passed decoder case. |
| Native registry metadata | C and sanitizer controls plus four Rust wrapper tests passed | [Run 42600802983](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42600802983), exact source `f2a0fec94d36af268c0d0c4bc8e0d0d39a258dfc`; includes a failure-propagation negative control |

Downloaded artifact hashes and source digests are retained under
`tests/results/ocr-studio/hosted-7b5a7791/`,
`tests/results/ocr-studio/storage-f3cd96af/`, and
`tests/results/ocr-mobile-import/qualification.json`. The earlier native storage
receipt and failed provider receipts remain available. Correcting a test or
implementation does not erase the previous failure.

### Phone-photo quality

The [paired native photo report](https://github.com/CrispStrobe/CrispEmbed/blob/f2a0fec94d36af268c0d0c4bc8e0d0d39a258dfc/tests/results/ocr-phone-photo/quality-fde19180.json)
uses the public preparation API and independent Tesseract 5.3.4 on five synthetic
pages. Prepared CER was 0% for aligned, 4.2-degree clockwise, 6.7-degree
counterclockwise/shadow and 10-degree/shadow cases. Perspective plus shadow
improved from 14.5228% to 1.2448% CER. The shadowed results include background
whitening: deskew alone made those two rotation cases worse. The report retains
all arms and distinguishes public-API output from internal geometry experiments.

These results cover the declared synthetic images and independently measured
text. They do not establish native mobile model quality, a camera-produced corpus,
or multilingual accuracy.

### History performance

The hosted history benchmark verifies final text, page identities and byte budgets
before reporting timings. On its 1,000-page fixture, median edit time changed from
217.5 ms to 36.8 ms; on its 5,000-page fixture, from 457.4 ms to 190.4 ms. The
5,000-page fixture deliberately uses a history budget too small for any snapshot.
Memory did not improve uniformly: process maximum RSS increased for the
1,000-page case and decreased for the 5,000-page case. These are Node history-model
measurements, excluding decoded page pixels, WebView paint and native OCR.

## Qualification in progress

The complete paired native application check and newly packaged Android/iOS
applications need their own successful source-pinned runs. Official runtime input
hashes and C++ probe syntax do not establish target loading or inference. The
browser harness is being rerun on the combined integration source, including the
later failed-import-save recovery case.

Archival PDF and bidirectional reviewed-PDF support are being developed and
qualified separately. The archival validator has passed 15 generated PDF/A-2b
files and rejected both damaged controls in [run 42608270168](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42608270168),
source `c2f71f7c28f838c3c0b5d2ae5461507e0503b606`. Independent
text readers still expose bidirectional mapping issues there; that source is not
yet enabled in this integrated application. The existing reviewed-PDF capability
gate continues to protect unsupported output.

Physical devices are not connected to this workspace. Actual camera capture,
cloud-provider lifetime/revocation, low-memory and thermal behavior, sustained
large-document workloads and representative camera documents require observed
device evidence. The [device protocol](ocr-device-qualification.md) and bounded
native harness specify the inputs, source/binary hashes, memory/timing measures,
failure retention and explicit device selection needed to collect that evidence.
An emulator, injected camera input or a native provider helper is never counted
as physical capture.
