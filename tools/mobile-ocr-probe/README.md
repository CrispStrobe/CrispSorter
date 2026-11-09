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
reviewed PDF staging and cleanup. The installed APK hash must match the built
artifact. Its fresh model store must begin empty; no fixture adapter supplies
recognition or pretends to install weights.

The command receipt preserves raw recognition output before assertions and
records input, model, APK, and runtime-build hashes. This is a synthetic command
integration test, not interaction coverage for the mobile UI or a physical
camera, system document picker, performance budget, or representative document
corpus. PDF signature checks do not establish the accuracy of its text layer.
The iOS lane currently executes the standalone native probe and app startup;
it does not claim the Android application's command coverage.
