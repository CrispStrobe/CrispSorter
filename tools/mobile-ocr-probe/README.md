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
