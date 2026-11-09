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
| Export lifecycle | Reviewed PDFs can stream into a staged file with bounded writes; cancellation and progress follow the worker until it finishes. Publication is atomic, existing outputs need explicit replacement, and late stops cannot cancel a completed result. Mobile destination handoff is explicit. |
| Asset integrity | Reusing an existing content-addressed page requires a bounded byte comparison and regular-file checks. Truncated, corrupted or aliased assets fail without replacing the source. |
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
| Combined frontend | 233 tests passed; Svelte check: zero errors, 54 existing warnings in eight other files | Integrated frontend through `2757114091f55620873e4d4245a6a42e1ecf8313`; export controls, archival selector, import recovery and mobile CPU provider |
| Build/probe helper contracts | 83 Python tests and 11 Node tests passed | Same integrated build/helper sources; these validate helper behavior, packaging, receipts and controls, not target execution |
| Hosted browser | 28 scenarios and 218 unit tests passed; production build passed; nine screenshots retained | [Run 42613157935](https://github.com/CrispStrobe/CrispSorter/actions/runs/42613157935), exact source `5b0ab78d7ec70a051ae415b56066e90495c40484`; includes failed-import-save recovery; native adapters remain synthetic |
| Paired native application modules | 100 Rust cases passed; 15 live/model cases explicitly ignored; native renderer 51/51 | [Run 42614585554](https://github.com/CrispStrobe/CrispSorter/actions/runs/42614585554), Sorter `7104a35c` with Embed `d49a8967`; successful Rust test links and two metadata-only feature checks, not a complete app build |
| Model storage and export control | 22 native tests passed, zero failed/ignored: ten storage, six atomic output and six cancellation | [Run 42622751482](https://github.com/CrispStrobe/CrispSorter/actions/runs/42622751482), exact source `d90c4612`; actual production modules with locked dependencies. Three later terminal-ID regressions need their own run. |
| JPEG integrity | Six actual decoder/guard controls passed | [Run 42616849573](https://github.com/CrispStrobe/CrispSorter/actions/runs/42616849573), source `8dad704`; three damaged JPEGs accepted by the decoder are rejected by the guard; three valid controls retain exact pixels |
| Page-asset reuse | Nine portable native tests passed | [Run 42621716073](https://github.com/CrispStrobe/CrispSorter/actions/runs/42621716073), source `715cfc14`; production asset helper and image integrity controls, not full-app reimport |
| iOS document provider | 19 tests passed, zero failures | [Run 42612700801](https://github.com/CrispStrobe/CrispSorter/actions/runs/42612700801), exact source `76de6dad`; simulator native PDF, pinned HEIC fixture, AVIF, orientation and failure controls |
| Android document provider | 20 tests: 19 passed, one AVIF decoder skip, zero failures | Same source and run; Android 10 emulator native PDF/image/provider contracts, including the same pinned HEIC input. A skipped codec is not a passed decoder case. |
| Native registry metadata | C and sanitizer controls plus four Rust wrapper tests passed | [Run 42600802983](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42600802983), exact source `f2a0fec94d36af268c0d0c4bc8e0d0d39a258dfc`; includes a failure-propagation negative control |

Downloaded artifact hashes and source digests are retained under
`tests/results/ocr-studio/hosted-5b0ab78d/`,
`tests/results/ocr-studio/paired-7104a35c/`,
`tests/results/ocr-studio/storage-cancel-d90c/`,
`tests/results/ocr-asset-integrity/qualification.json`, and
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
184.6 ms to 28.1 ms; on its 5,000-page fixture, from 395.3 ms to 165.1 ms
in the retained `5b0ab78d` run. The
5,000-page fixture deliberately uses a history budget too small for any snapshot.
Memory did not improve uniformly: process maximum RSS increased for the
1,000-page case and decreased for the 5,000-page case. These are Node history-model
measurements, excluding decoded page pixels, WebView paint and native OCR.

## Qualification in progress

The paired native module run now passes, with its exact limits retained: no model
weights were exercised, application feature checks emitted metadata, and that
older run rewrote two local dependency-lock entries. The tracked lock correction
is present in the newer source. Complete desktop executable link/startup and
packaged Android/iOS applications still need their successful source-pinned runs.
Official runtime hashes and helper tests do not establish target loading or
inference. The later export-control browser cases and terminal-ID cancellation
regressions are being qualified separately.

CrispEmbed's frozen streaming candidate `2111db64` passed 70 native controls,
nine actual Rust writer controls, and 13 independent PDF/A-2b positives while
rejecting both damaged controls. It wrote a 138,448,604-byte, 11-page file through
the bounded writer and preserved the independently checked pixels, physical
sizes, page count and copied text. The overall [run 42622422668](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42622422668)
remained failed: PDFium supplementary-character search and a Poppler dense-page
stress case require explicit compatibility diagnostics. General bidirectional
PDF export remains disabled because the strict multi-reader matrix still fails.
The integrated writer requires matching native code and bindings before final
application qualification; it must not be paired with the earlier runtime pin.
See [streaming export](ocr-streaming-export.md) and
[export controls](ocr-export-control.md).

Windows Unicode image paths are repaired in the production loader and crop
writer, with a hosted Windows test of the actual translation units, exact pixels,
PDF image preservation and an executable historical ANSI negative control. That
platform run must succeed before the path is called qualified.

Physical devices are not connected to this workspace. Actual camera capture,
cloud-provider lifetime/revocation, low-memory and thermal behavior, sustained
large-document workloads and representative camera documents require observed
device evidence. The [device protocol](ocr-device-qualification.md) and bounded
native harness specify the inputs, source/binary hashes, memory/timing measures,
failure retention and explicit device selection needed to collect that evidence.
An emulator, injected camera input or a native provider helper is never counted
as physical capture.

The source-pinned native application harness now also defines accented German
recognition and an English page with a spanning heading and two columns. It
installs the verified 513,952-byte German model through the real app command,
retains Unicode CER and required diacritics, checks column order independently
from text matching, and saves/reloads the resulting project. Input hashes are
retained in `tests/results/ocr-studio/native-quality-inputs.json`; they are input
verification, not a native recognition pass. The combined helper controls at
`456cee5244db25d35b00dac3e2e8a66dd5e272d7` passed 62 Python and eight Node tests.
