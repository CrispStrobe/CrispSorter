# Native mobile OCR probe

This program executes the paired public photo API and real DBNet + Tesseract
inference on a synthetic three-line page. The standalone probe disables the
Tesseract segmentation router so its detector actually runs; application defaults
are not changed. It checks blank captures, byte-exact
identity perspective, a known five-degree rotation and its measured correction,
recognized text, and native review snapshot availability. Both the original and
prepared page must retain the document number and stay below the declared
normalized text-error threshold.

The input helper downloads two public model fixtures only after checking their
committed SHA-256 pins. The fixture receipt records the model pins, text, font
hash and rendered image hash. Weights are temporary CI inputs and are not
bundled with the application artifacts.

The native receipt and logs are separate from the full application startup
receipt. A passed native probe does not establish camera or document-picker
operation, mobile installer behavior, broad language/corpus accuracy, memory
headroom on a physical phone, or sustained performance. Those require device
qualification.

Build with the same source headers and native runtime revision pinned in
scripts/ocr-mobile-runtime-lock.json. Do not link this probe or the new Rust
binding against an older released native library. The paired mobile workflow
provides the Android emulator and iOS simulator build/run commands.

## Android application command smoke

The x86_64 emulator lane also connects to the installed debug application's own
WebView socket and invokes the real Tauri commands. This separately exercises
the private HTTPS model installer, independent artifact hash checks, raw image
import, native photo transforms, persisted recognition settings, real OCR, and
reviewed PDF staging and cleanup. The installed APK hash must match the clean
source build receipt and the supplied artifact. Allocated native code/data in
the APK must match the paired runtime even if packaging strips debug symbols.
Its fresh model store must begin empty; no fixture adapter supplies
recognition or pretends to install weights.

The command receipt preserves raw recognition output before assertions and
records input, model, APK, and runtime-build hashes. It samples application RSS,
kernel high-water RSS and PSS around each native recognition, repeats eight
actual recognitions with durable per-page saves, and enforces a declared process
RSS budget. The repeat case alternates two synthetic rasters; it does not
establish diverse-corpus or large-document performance.

When the app advertises native document import, the same harness imports the
hash-pinned two-page PDF through actual raw IPC and native rasterization. It
checks physical dimensions, UTF-8 metadata, source/archive identity, real OCR
on both page rasters, malformed PDF rejection, and the advertised AVIF/EXIF
orientation fixtures. The fixtures are supplied by
`tests/fixtures/mobile-documents/manifest.json`; no host PDF renderer can satisfy
these app assertions.

The UI case opens the real OCR workspace and creates/saves a separate project
using its actual controls. This preserves the first project's results and
respects source deduplication. It supplies a synthetic image to the camera file
input, verifies native import/autosave and the preparation view, then starts
recognition from the actual workspace button. This is a
capture **file handoff**, not a claim that the system camera opened or produced
the fixture.

On the dedicated Android emulator, the next case stages the pinned PDF under
one unique name in Downloads. **Add documents** opens the actual system picker.
The case cancels once and checks that the project is unchanged, then selects
only the exact observed fixture filename. Fresh UI Automator XML and screen
captures precede every tap; unexpected packages/permission prompts, ambiguous
filenames, missing controls and unsupported layouts fail the case. It checks
the returned content URI, reads bytes through the actual provider grant, checks
the private archive hash and verifies both natively rendered pages in the saved
project. The driver removes only its uniquely named staged file and XML scratch
file. Physical devices never enter this automatic picker case. Cloud providers,
provider access after restart, physical camera capture and representative corpus
qualification remain separate. PDF signature checks do not establish text-layer accuracy.
The iOS lane currently executes the standalone native probe and app startup;
it does not claim the Android application's command coverage.

The driver also accepts `--device-kind physical` with an explicit authorized
serial and exact already-installed debug APK. It does not install, erase data,
or select hardware automatically. See the
[device protocol](../../docs/ocr-device-qualification.md) for commands, evidence
boundaries and observed camera/provider cases. Read actual receipts before
treating a defined check as executed or passed.
