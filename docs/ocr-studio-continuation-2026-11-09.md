# OCR studio continuation — 9 November 2026

This record follows the [first recovery checkpoint](ocr-studio-checkpoint-2026-11-09.md).
Work is saved incrementally in upstream feature branches and remains reviewable in
[CrispSorter draft PR #29](https://github.com/CrispStrobe/CrispSorter/pull/29) and
[CrispEmbed draft PR #189](https://github.com/CrispStrobe/CrispEmbed/pull/189).
No release qualification is inferred from a source change or a queued workflow.

The combined application candidate is saved on `feat/ocr-document-studio` at
`b876fb43eb19cc7de44760400e50735aa27c2c78`. Its native runtime and bindings both
pin `9c44a69cad0d949e26a200e2bda1ccd58e3f9b8a`. Additional evidence checkpoints
remain on `feat/ocr-streaming-export` while that immutable app source runs.

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
| Phone corner editing | A focused view gives the page most of the screen, with 44-pixel touch handles, zoom, exact numeric coordinates and reachable Back/Apply controls. Leaving the editor preserves the image until Apply; Undo restores the original geometry and review data. |
| Native dependency notices | Packaged builds derive their dependency inventory from the actual locked Cargo graph, target and enabled features. The mobile profile includes the protocol feature added by the pinned CLI. Missing or inconsistent native entries fail packaging. |
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
| Combined frontend | 239 tests passed; Svelte check: zero errors, 54 existing warnings in eight other files | Exact integrated source `e37fa4372b20a681234e4b5b66b0ada3e568d908`; includes exact cancellation-error classification and focused phone controls |
| Build/probe helper contracts | 89 Python tests and 17 Node tests passed | Same integrated source; these validate helper behavior, packaging, receipts and dependency inventories, not target execution |
| Hosted browser | 28 scenarios and 218 unit tests passed; production build passed; nine screenshots retained | [Run 42613157935](https://github.com/CrispStrobe/CrispSorter/actions/runs/42613157935), exact source `5b0ab78d7ec70a051ae415b56066e90495c40484`; includes failed-import-save recovery; native adapters remain synthetic |
| Focused phone editor | Trusted touch, numeric validation, Apply/Back/Escape and Undo passed; page measured 342×478.8 within a 390×620 canvas at a 390×844 viewport | [Run 42631253524](https://github.com/CrispStrobe/CrispSorter/actions/runs/42631253524), source `fbc03f74`; overall 31 scenarios passed and two failed. Both failed selectors are retained and repaired in the combined candidate; this is not a whole-browser pass. |
| Export progress in browser | Both new phone export lifecycle scenarios passed | [Run 42625583821](https://github.com/CrispStrobe/CrispSorter/actions/runs/42625583821), source `d0f9baa9`; overall 31 passed and one archival-option assertion failed. Native DOM state and keyboard behavior replace that ambiguous query in the next source. |
| Paired native application modules | 100 Rust cases passed; 15 live/model cases explicitly ignored; native renderer 51/51 | [Run 42614585554](https://github.com/CrispStrobe/CrispSorter/actions/runs/42614585554), Sorter `7104a35c` with Embed `d49a8967`; successful Rust test links and two metadata-only feature checks, not a complete app build |
| Model storage and export control | 25 native tests passed, zero failed/ignored: ten storage, six atomic output and nine cancellation | [Run 42627812221](https://github.com/CrispStrobe/CrispSorter/actions/runs/42627812221), exact source `8ea0a7e7`; actual production modules, including late completed/failed request IDs and bounded expiry |
| JPEG integrity | Six actual decoder/guard controls passed | [Run 42616849573](https://github.com/CrispStrobe/CrispSorter/actions/runs/42616849573), source `8dad704`; three damaged JPEGs accepted by the decoder are rejected by the guard; three valid controls retain exact pixels |
| Page-asset reuse | Nine portable native tests passed | [Run 42621716073](https://github.com/CrispStrobe/CrispSorter/actions/runs/42621716073), source `715cfc14`; production asset helper and image integrity controls, not full-app reimport |
| iOS document provider | 19 tests passed, zero failures | [Run 42612700801](https://github.com/CrispStrobe/CrispSorter/actions/runs/42612700801), exact source `76de6dad`; simulator native PDF, pinned HEIC fixture, AVIF, orientation and failure controls |
| Android document provider | 20 tests: 19 passed, one AVIF decoder skip, zero failures | Same source and run; Android 10 emulator native PDF/image/provider contracts, including the same pinned HEIC input. A skipped codec is not a passed decoder case. |
| Native registry metadata | C and sanitizer controls plus four Rust wrapper tests passed | [Run 42600802983](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42600802983), exact source `f2a0fec94d36af268c0d0c4bc8e0d0d39a258dfc`; includes a failure-propagation negative control |
| iOS native runtime and app startup | Actual English OCR and prepared-photo text matched the three-line fixture; known +5° tilt recovered; real ORT Add inference and installed app startup passed | [Run 42619149388, iOS job 129170291385](https://github.com/CrispStrobe/CrispSorter/actions/runs/42619149388/job/129170291385), source `646dbc48` with older Embed `d49a8967`; iPhone 16 Pro/iOS 18.5 simulator. Inbox startup does not establish OCR workspace navigation or the complete mobile workflow. |
| Android app compilation and package identity | ARM64 and x86_64 apps compiled and packaged; corrected bounded inspection independently confirms native hashes, load alignment and dependency closure | Same `646dbc48` run; both hosted jobs failed at the old receipt size limit before emulator execution. The corrected inspector is in the combined source. Neither APK is counted as a runtime/inference pass. |

Downloaded artifact hashes and source digests are retained under
`tests/results/ocr-studio/hosted-5b0ab78d/`,
`tests/results/ocr-studio/paired-7104a35c/`,
`tests/results/ocr-studio/storage-cancel-d90c/`,
`tests/results/ocr-studio/storage-cancel-8ea0/`,
`tests/results/ocr-studio/mobile-646-ios/`,
`tests/results/ocr-studio/mobile-646-x86_64/`,
`tests/results/ocr-mobile-ui/`,
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

Exact application source `b876fb43` has accepted
[paired desktop/native](https://github.com/CrispStrobe/CrispSorter/actions/runs/42634608358),
[mobile target](https://github.com/CrispStrobe/CrispSorter/actions/runs/42634608233), and
[browser](https://github.com/CrispStrobe/CrispSorter/actions/runs/42634608257)
workflows. Their creation is not a pass. The earlier invalid-workflow records,
caused by a runner context in a job-level environment, are preserved in
`tests/results/ocr-studio/native-scheduling-e37/`; the correction resolves the
path in a runner step. The full gates include the real writer crate, application
exports, Linux executable link/startup, corrected Android package inspection,
mobile native loading/inference and the actual dependency inventory.

The retained iOS startup screenshot shows Inbox without visible app navigation.
A small simulator UI-test probe is separately checking the unchanged installed
app's accessibility frames and Tools → OCR reachability before any layout claim.
Neither a native probe nor browser adapters substitute for this application path.

CrispEmbed's final native component `9c44a69` passed 70 C controls, ten ordinary
Rust writer contracts plus the separate large-file case, 11 artifact methods,
13 independent PDF/A-2b positives and both damaged negative controls. Its
11-page output was 138,448,604 bytes; page counts, physical sizes, text and
independent image checks passed. Observed hosted peak RSS was 59,128 KiB and
elapsed time 10.48 seconds for that bounded case, not a mobile memory guarantee.
The [source-bound writer receipt](https://github.com/CrispStrobe/CrispEmbed/blob/b16df2c4607bc8b2264fb57ae096985e78e641d1/tests/results/ocr-render/writer-9c44a69.json)
retains the exact scope. PDFium supplementary-character search, a dense-page
Poppler stress case and general bidirectional reader interoperability remain
explicit compatibility limits. The bidi capability stays disabled; the prior
failed whole workflows and all experimental reader outcomes remain available.
See [streaming export](ocr-streaming-export.md) and
[export controls](ocr-export-control.md).

The same `9c44a69` source passed the actual Windows UTF-8 path boundary: 21
production translation-unit checks, 18 standalone checks, nine historical ANSI
controls and four independently read PDFs with exact image pixels. The
[Windows receipt](https://github.com/CrispStrobe/CrispEmbed/blob/b16df2c4607bc8b2264fb57ae096985e78e641d1/tests/results/ocr-render/windows-9c44a69.json)
binds the binaries, inputs and verified artifact. This qualifies file I/O and
rendering on that Windows job, not the entire Windows application.

A separate retained-camera fixture lane at CrispEmbed `8021ca2` adds existing
iPhone 7 photographic pixels, independent perspective mapping, known added-tilt
controls and paired English/French text scores. Its hosted result is pending.
The [protocol](https://github.com/CrispStrobe/CrispEmbed/blob/8021ca2603ba84f948057b30bb3adf88e1afd04a/docs/ocr-retained-camera-fixtures.md)
distinguishes an existing phone photograph from a newly observed capture.

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
