# OCR studio continuation — 9 November 2026

This record follows the [first recovery checkpoint](ocr-studio-checkpoint-2026-11-09.md).
Work is saved incrementally in upstream feature branches and remains reviewable in
[CrispSorter draft PR #29](https://github.com/CrispStrobe/CrispSorter/pull/29) and
[CrispEmbed draft PR #189](https://github.com/CrispStrobe/CrispEmbed/pull/189).
No release qualification is inferred from a source change or a queued workflow.

The combined application candidate is saved on `feat/ocr-document-studio` at
`b876fb43eb19cc7de44760400e50735aa27c2c78`. Its native runtime and bindings both
pin `9c44a69cad0d949e26a200e2bda1ccd58e3f9b8a`. Further integrated source is saved on `feat/ocr-streaming-export` at
`0437d26d1ee631b977e3be88865d99461b329f42` while that immutable app source runs.
It adds mobile Word/text export, early format checks, exact import-cancellation
classification and preservation of compiled iOS artifacts before simulator checks.
CrispEmbed's main OCR feature branch is saved at `4c98a9f864166e0c09f77d9a9a289e92a6f1c28f`,
including verified retained-camera results; its runtime pin remains unchanged.

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
| Reviewed text alternatives | Selected-page preflight rejects unsupported bidirectional PDF and invalid rich-document controls before opening a destination picker. Available Text, HTML and Word alternatives preserve the reviewed logical text. The portable Word feature is enabled by mobile and desktop profiles. |
| Import error identity | Only an exact native cancellation marker becomes cancellation. Password, permission and provider errors retain their identity even when a source filename contains that marker. |
| iOS artifact retention | The exact compiled app is archived with an unqualified source/hash receipt before simulator startup. A later simulator failure cannot discard the completed build or imply runtime qualification. |
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
| Combined frontend | 250 tests passed across 29 files; Svelte check: zero errors, 54 existing warnings in eight other files | [Exact merged source and complete logs](../tests/results/ocr-studio/combined-text-5382/receipt.json), based on `ebcc9b5a` plus `5382ccc5`; the saved `0437d26d` source includes the same tested modules |
| Build/probe helper contracts | 89 Python tests and 17 Node tests passed | Integrated source `e37fa437`; these validate helper behavior, packaging, receipts and dependency inventories, not target execution |
| Hosted browser | 28 scenarios and 218 unit tests passed; production build passed; nine screenshots retained | [Run 42613157935](https://github.com/CrispStrobe/CrispSorter/actions/runs/42613157935), exact source `5b0ab78d7ec70a051ae415b56066e90495c40484`; includes failed-import-save recovery; native adapters remain synthetic |
| Focused phone editor | Trusted touch, numeric validation, Apply/Back/Escape and Undo passed; page measured 342×478.8 within a 390×620 canvas at a 390×844 viewport. Both phone and desktop corner paths pass after the desktop selector correction. | [Run 42634301391](https://github.com/CrispStrobe/CrispSorter/actions/runs/42634301391), source `9e0f0fe1`; overall 32 passed and one older archival-option assertion failed. The original 31/2 run and its screenshots remain retained. |
| Archival format availability | All 32 browser scenarios and 233 unit tests passed; zero browser errors. Native disabled-option state, exact reason, keyboard skipping and zero accidental exports verified. | [Run 42633072468](https://github.com/CrispStrobe/CrispSorter/actions/runs/42633072468), source `76bf5d8`; [receipt](../tests/results/ocr-studio/archival-option-76bf5d8.json). This isolated source predates the newest phone and text changes. |
| Export progress in browser | Both new phone export lifecycle scenarios passed | [Run 42625583821](https://github.com/CrispStrobe/CrispSorter/actions/runs/42625583821), source `d0f9baa9`; overall 31 passed and one archival-option assertion failed. Native DOM state and keyboard behavior replace that ambiguous query in the separately passing `76bf5d8` source. |
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
`tests/results/ocr-studio/combined-text-5382/`,
`tests/results/ocr-studio/archival-option-76bf5d8.json`,
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

The [retained photographic-input job](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42635475023/job/129222355005)
also passes at `8021ca2`, with production geometry hashes identical to the pinned
`9c44a69` runtime. An existing iPhone 7 photo rectifies to 755×1259 pixels and
matches independent OpenCV mapping at mean channel error 0.104874/255 (99th
percentile 1/255). Both photographs recover the independently added −4.2° and
+6.7° rotations relative to their original estimates.

| Printed-page input | Raw CER | Deskew-only CER | Deskew + whitening CER |
| --- | ---: | ---: | ---: |
| Original | 0.1677% | 0.4696% | 0.3690% |
| −4.2° added tilt | 12.2442% | 0.2348% | 0.4025% |
| +6.7° added tilt | 61.4894% | 0.3355% | 0.5032% |

The prepared arm was declared before execution; every arm's actual text, TSV and
scores is retained in the [verified receipt](https://github.com/CrispStrobe/CrispEmbed/blob/4c98a9f864166e0c09f77d9a9a289e92a6f1c28f/tests/results/ocr-phone-photo/camera-8021/artifact-identity.json).
The original was already readable, and whitening slightly worsened it. Cleanup
therefore remains an explicit operation with original comparison and undo.
The historical Fraktur/manuscript photo supplies geometry evidence only; it has
no invented OCR transcript. These bounded cases do not establish a new physical
capture, broad document corpus or native mobile recognition-model quality.

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
workflows. The native library, actual locked desktop inventory/production frontend
and whole-crate writer steps have passed; application Rust verification continues.
Both Android architectures have entered actual app compilation, while iOS is
queued. These intermediate steps do not constitute completed application gates. The earlier invalid-workflow records,
caused by a runner context in a job-level environment, are preserved in
`tests/results/ocr-studio/native-scheduling-e37/`; the correction resolves the
path in a runner step. The full gates include the real writer crate, application
exports, Linux executable link/startup, corrected Android package inspection,
mobile native loading/inference and the actual dependency inventory.

The retained iOS startup screenshot shows Inbox without visible app navigation.
A small simulator UI-test probe is now running against the unchanged installed
app to check accessibility frames and Tools → OCR reachability before any layout
claim. An earlier attempt timed out during host simulator discovery before app
installation, and its original failure evidence remains saved.
Neither a native probe nor browser adapters substitute for this application path.

CrispEmbed's final native component `9c44a69` passed 70 C controls, ten ordinary
Rust writer contracts plus the separate large-file case, 11 artifact methods,
13 independent PDF/A-2b positives and both damaged negative controls. Its
11-page output was 138,448,604 bytes; page counts, physical sizes, text and
independent image checks passed. Observed hosted peak RSS was 59,128 KiB and
elapsed time 10.48 seconds for that bounded case, not a mobile memory guarantee.
The [source-bound writer receipt](https://github.com/CrispStrobe/CrispEmbed/blob/b16df2c4607bc8b2264fb57ae096985e78e641d1/tests/results/ocr-render/writer-9c44a69.json)
retains the exact scope. The final strict reader diagnostic has also completed:
five supplementary-search assertions and the unchanged dense Poppler page fail.
A valid independent producer reproduces the full-scalar search failure with
exact copied Unicode; the original dense page returns the exact first 50,000
of 65,542 characters in that Poppler version. These and general bidirectional
reader interoperability remain explicit compatibility limits. The bidi capability stays disabled; the prior
failed whole workflows and all experimental reader outcomes remain available.
See [streaming export](ocr-streaming-export.md) and
[export controls](ocr-export-control.md).

The same `9c44a69` source passed the actual Windows UTF-8 path boundary: 21
production translation-unit checks, 18 standalone checks, nine historical ANSI
controls and four independently read PDFs with exact image pixels. The
[Windows receipt](https://github.com/CrispStrobe/CrispEmbed/blob/b16df2c4607bc8b2264fb57ae096985e78e641d1/tests/results/ocr-render/windows-9c44a69.json)
binds the binaries, inputs and verified artifact. This qualifies file I/O and
rendering on that Windows job, not the entire Windows application.

The retained-camera results above have passed. A later `fe0e279` run separately
checks enhanced identity-PNG preservation; the original job's identity assertion
passed but its identity PNG was omitted. The complete original text/TSV evidence
and all image hashes remain saved without reclassifying that omission.

The new mobile Word/text module at `5382ccc5` has accepted
[portable/Android/iOS contracts](https://github.com/CrispStrobe/CrispSorter/actions/runs/42640418541)
and [browser preflight](https://github.com/CrispStrobe/CrispSorter/actions/runs/42640418500)
runs, currently queued. These checks compile the actual production writer and
independently read DOCX/HTML/plain-text artifacts. The Android WebView
probe now defines a separate reviewed fixture, saves/reloads it, exports Word
through the actual app command and independently reads the saved OOXML. It
checks the correction, mixed logical text, spacing and private staging cleanup,
while preserving raw recognition and all existing pages. The six reader
controls pass locally; actual target execution remains pending. See the
[mobile Word probe](ocr-mobile-word-probe.md). None of these pending
checks is reported as a completed mobile Word round-trip. See the
[text-export contract](ocr-text-export-compatibility.md).

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
