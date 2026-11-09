# Mobile indexing runtime qualification

CrispSorter's existing ONNX dense, sparse and token-aware indexing paths use
`ort = 2.0.0-rc.11`, whose C interface is ONNX Runtime API 23. That crate's
automatic binary catalogue does not supply Android x86_64. The OCR mobile
workflow therefore stages official platform artifacts through
`scripts/prepare_mobile_ort.py` and the committed
`scripts/ocr-mobile-ort-lock.json`.

Android uses Microsoft's Maven `onnxruntime-android` 1.23.2 AAR. The selected
ARM64/x86_64 C API library is checked against the whole-archive and per-library
SHA-256 pins, the selected ELF ABI, its dependency closure and 16 KiB load
alignment. Only the C API runtime is needed; the Java JNI wrapper is not linked
by the Rust application. The pinned C runtime depends only on public Android
system libraries. Any future non-system dependency fails staging until its
own library is supplied and validated.

iOS uses the published `onnxruntime-c` 1.23.0 CocoaPod archive. The 1.23.2 iOS
package is not published; the explicit 1.23.0 package supplies the same API-23
header bytes. The selected device/simulator platform, architecture, static
archive slices, version and whole-archive/library hashes are checked. Cargo
receives the qualified XCFramework path, and the final Xcode link explicitly
includes that same static archive and CoreML. API compatibility is subsequently
tested by executing the probe; a matching version label alone is insufficient.

The application build receipt requires these pinned input hashes and forced
Cargo paths, matches Android packaged allocated code/data with the qualified
runtime, checks the defined iOS C API symbol, and verifies that the original
MIT license and third-party notices actually appear in the APK/app bundle.
Static linking transforms an archive, so the iOS record describes archive,
configuration and symbol evidence without claiming executable byte equality.

## Actual target inference

`probe.cpp` uses the public C API directly. Its self-authored, weight-free graph
adds two FLOAT matrices of shape `[2,2]` and must produce exactly
`[11,22,33,44]` on three runs. It verifies the pinned runtime version, obtains
API 23, creates a real CPU session, executes the graph and checks output type,
shape and every value. `make_fixture.py` generates the embedded ONNX protobuf
from the public schema; CI requires byte-exact regeneration. No network model
or embedding weights are needed for this ABI/runtime check.

The Android emulator runs the probe against `libonnxruntime.so` extracted from
the exact built APK, after checking its build-receipt hash. The iOS simulator
runs a target binary linked against the same qualified static framework used
by the app. Both lanes first run a deliberately wrong expected version and
require a nonzero result plus a specific failure receipt; then the correct
version must execute all additions successfully. Raw logs and both JSON
receipts are retained even when a gate fails.

This tiny graph establishes actual runtime loading, the required C API and a
basic CPU inference path. It does not establish embedding model quality,
mobile accelerator support, OCR accuracy or physical-device performance. Read
the executed target receipts before reporting the gate as passed.
