# Mobile qualification

The desktop browser suite uses simulated Tauri responses. A narrow viewport is
not an Android or iOS build, and a successful archive/signature inspection does
not establish that an app has launched on a device. The standard release recipes still omit native CrispEmbed. The OCR feature
branch adds an explicit `ocr-mobile` profile and a paired build/execution lane,
while leaving release pins unchanged. Physical-device performance and full mobile
parity remain unqualified until the runtime, model files and app bindings have
been exercised on representative devices.

`scripts/mobile_simulator_smoke.py` provides a minimal **native startup** check
for an already booted, explicitly selected emulator or simulator. It installs
the supplied application, starts it, checks that its process survives, captures
a screenshot and writes a JSON receipt. It does not clear simulator storage.
Use a clean, dedicated test simulator: installation replaces an existing app of
the same identifier. Screenshot contents may contain user data.

Android requires `adb`, Android SDK build-tools `aapt`, a signed APK and an
emulator whose supported ABI list overlaps the APK's native libraries. The
current release APK is arm64; an x86-only emulator is insufficient. For example:

```sh
python scripts/mobile_simulator_smoke.py --platform android --target emulator-5554 \
  --artifact CrispSorter_0.14.0_android-aarch64.apk --output android-smoke
```

iOS requires a booted simulator UDID and a **simulator-built** `.app`; the
signed device IPA from the release cannot run in the simulator. For example:

```sh
xcrun simctl list devices booted
python scripts/mobile_simulator_smoke.py --platform ios --target SIMULATOR-UDID \
  --artifact /path/to/CrispSorter.app --output ios-smoke
```

These checks establish startup and capture a screen for inspection. They do not
verify the contents of the screen, document-picker grants, persistence after
restart, import, search, review, moves, native inference or a physical device.
A device qualification should explicitly test those flows with disposable
fixture files and preserve the logs/screenshots, build identity, OS/device model
and actual outcome. Do not interpret a simulator startup receipt as a full mobile
parity or device-execution result.

## Provider imports

Android batch import now uses a registered Storage Access Framework plugin.
The file picker uses `ACTION_OPEN_DOCUMENT`; the folder picker uses
`ACTION_OPEN_DOCUMENT_TREE`, recursively enumerates the selected tree, and
retains the original `content://` document URIs. It requests a persistent
**read-only** grant and reports an error when the provider will not grant one.
Batch extraction reads directly from the provider; it does not substitute a
local copy for the original. PDF, DOCX, EPUB, TXT, Markdown and HTML are
supported, with a 16 MiB per-document read limit. Unknown sizes are checked
while streaming. Folder enumeration fails explicitly beyond 5,000 entries or
32 levels, and the picker reports unsupported or oversized files it skips.

Provider items remain available for extraction and review, but cannot be
moved, copied, deleted, included in a sort script, or sent to the path-based
index and document tools. This is a read-only slice, not folder-sort parity.
The original URI and persisted grant are intended to survive restart; device
verification of that behavior is still required, and revoked or unavailable
provider access must produce a visible error.

The iOS batch picker opens documents or folders in place through
`UIDocumentPickerViewController`. It retains each original `file://` URL as
the batch identity and stores a minimal security-scoped bookmark for reopening
the document. Reads resolve the bookmark, start and stop scoped access, and use
`NSFileCoordinator`; missing, stale or revoked grants require the user to select
the document again. The same file types and limits apply; symbolic links in a
selected folder cause an explicit error. Provider items remain read-only, and
iOS path-based sorting is disabled because older batch rows may have come from
copies. Other mobile picker entry points are not covered by this flow.

`.github/workflows/mobile-provider-check.yml` builds both mobile plugins and
exercises the Android reader on an emulator against a test ContentResolver
provider. The Android instrumentation covers nested folders, case-sensitive
document IDs, reads, unavailable access, size limits and folder cycles. The iOS
simulator tests cover bookmark-record identity and bounded reads. They do not
exercise either system picker, a real provider's persistent grants, or the
complete apps. The [shared-fixture provider run](https://github.com/CrispStrobe/CrispSorter/actions/runs/42612700801)
at source `76de6dad49f9474aa1034e3030f95cd3f4a6d144` passed Rust/Swift plugin
checks, 19 Android emulator tests, and all 19 iOS simulator tests. The Android
suite contained 20 tests: its AVIF case skipped because the Android 10/API 29
runtime did not advertise that decoder. The iOS run used Xcode 16.4 and the
iPhoneSimulator 18.5 SDK; its HEIC encode/decode, AVIF, and EXIF cases ran and
passed. Both platforms decoded the same pinned HEIC fixture and checked its
dimensions, pixel colors, and unchanged original bytes. They also exercised PDF page count, nonblank pixels, dimensions,
rotation, protected/damaged input rejection, source preservation, cancellation,
and private-path/output limits. Export controls also cover exact-byte copying
and staging protection. The [qualification receipt](../tests/results/ocr-mobile-import/qualification.json)
retains every test result, artifact digest, and the earlier iOS failures fixed
in `dd8d9b1`. These are provider/decoder results; complete-app and physical
device qualification still require the separate checks below. The provider's
JPEG helper tests do not qualify the application's shared Rust JPEG importer.

For qualification, use a dedicated device or emulator and disposable local and
cloud-provider fixtures. Record the build and OS/device, and verify picker
cancellation, individual and nested folder selections, Unicode and case-sensitive
names, unsupported and oversized files, extraction, app restart, and provider
access after revocation. Confirm that provider items cannot enter any move,
copy, delete or script operation and that selecting a new grant after revocation
restores access. Inspect errors and confirm no local copy was presented as the
original. The historical `mobile_fs` module and `android-src/SAFBridge.kt` are
not registered and still contain stubs; they are not the implementation used by
this flow.

## Paired OCR mobile feature builds

The optional **ocr-mobile** profile enables native image recognition, explicit
photo assessment/perspective/tilt operations, confidence-aware review, and
reviewed PDF/text/structured exports without desktop helper processes. Mobile
PDF import uses Android `PdfRenderer` or iOS Core Graphics through the registered
provider plugin, so it does not require the desktop PDFium feature. The workspace
enables this import path only after the native capability query succeeds.
Standard releases keep their existing profile.

The OCR workspace uses the native provider picker for PDF, JPEG, PNG, TIFF, BMP,
and WebP. HEIC, HEIF, and AVIF are also accepted when the native platform reports
the corresponding decoder; conversion to JPEG/PNG is requested only when that
decoder is unavailable or the actual source cannot be decoded. Provider reads
remain limited to 16 MiB, while camera/raw-byte imports have a 32 MiB source
limit. This differs from the general batch extraction format list above. The
system picker may show additional types, but the workspace validates every
selection. It archives exact input bytes separately from the original provider
URI, bakes orientation into working pixels, and reports progress/cancellation.
Mobile PDF and phone-container rendering caps each raster at 16 million pixels,
each axis at 16,384 pixels, and total PNG output at 512 MiB. PDFs contain 1–10,000
pages, each physical axis no larger than 14,400 points, and target 200 DPI within
those caps. Protected PDFs require an unlocked copy. See [Mobile document import](ocr-mobile-import.md)
for the complete limits, archive identity, and failure contracts.

A fresh mobile installation uses the compact native Tesseract/English default.
The workspace's **OCR models** controls explicitly install the detector and the
selected language model. Opening the workspace never starts a download. The
installer uses Rust HTTPS streaming and the exact paired registry's immutable
SHA-256 pins, writes a complete primary/companion set into a private version
directory, and atomically publishes its manifest. Recognition reads installed
models without spawning curl/wget or changing a process-wide cache environment.
Failed downloads preserve the previous complete installation. Other provisioned
engines remain selectable; models without published pins or with unmet license
requirements produce a visible error. **Manage model storage** reports downloaded
and reclaimable bytes. **Clean unused downloads** removes interrupted staging and
superseded, unused versions; the same cleanup runs on initialization and after
successful model publication when the store lock is available. Current installed
generations and generations leased by an open application are protected.
Removing an installed model is a separate confirmed action, checks that its
version has not changed, and refuses a model still held by a running process.
Loaded models can remain in use until application restart. These operations
affect the managed `ocr-models` store; document archives, reviewed text, and
external local GGUF files are preserved.

On Android, this installer's Rustls client uses the pinned public certificate
roots from webpki-root-certs 1.0.8. It verifies certificate chains and hostnames
without requiring an uninitialized JVM certificate-verifier component. Android
user/private trust anchors are not consulted by this client. HTTPS-only transport
and the registry's separate SHA-256 artifact checks both remain required.

Use a clean sibling CrispEmbed checkout and initialized ggml submodule at the
exact commits in scripts/ocr-mobile-runtime-lock.json. The native builder
checks both identities and refuses a dirty or mismatched source tree. Do not
substitute older released libraries for the paired bindings. Install the normal
Tauri platform toolchain, a host `protoc` from Protocol Buffers, and npm
dependencies before the following commands. The app also links the real mobile
ONNX Runtime used by its ONNX embedding paths. `prepare_mobile_ort.py` verifies
the official archive, target library, API-23 header, and notice pins in
`scripts/ocr-mobile-ort-lock.json`; use a fresh, absolute output directory per
target. Mobile ONNX sessions use CPU, including when a catalog copied from
desktop requested Metal/CUDA. The UI and diagnostics report that provider;
CrispEmbed's separately linked GGUF backend remains available.

For Android, with the NDK configured:

```sh
python3 scripts/build_mobile_crispembed.py \
  --source ../CrispEmbed --output ../ocr-native-android \
  --lock scripts/ocr-mobile-runtime-lock.json --target android-arm64
export CRISPEMBED_SYS_LIB_DIR="$(cd ../ocr-native-android/lib && pwd)"
export OCR_ORT_ROOT="$(pwd)/../ocr-ort-android"
python3 scripts/prepare_mobile_ort.py --target android-arm64 --output "$OCR_ORT_ROOT"
export RUSTFLAGS="-Clink-arg=-landroid -Clink-arg=-llog -Clink-arg=-lOpenSLES -C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
python3 scripts/prepare_ocr_cargo_env.py --project . \
  --runtime-lib-dir "$CRISPEMBED_SYS_LIB_DIR" --protoc "$(command -v protoc)" \
  --ort-lib-dir "$OCR_ORT_ROOT/lib"
mkdir -p src-tauri/bin
cp ../ocr-native-android/runtime-build.json src-tauri/bin/ocr-mobile-runtime.json
cp ../ocr-native-android/notices/* src-tauri/bin/
cp "$OCR_ORT_ROOT/notices/LICENSE" src-tauri/bin/onnxruntime-LICENSE.txt
cp "$OCR_ORT_ROOT/notices/ThirdPartyNotices.txt" src-tauri/bin/onnxruntime-ThirdPartyNotices.txt
npx tauri android init --ci --config src-tauri/tauri.ocr-mobile.conf.json
CRISPEMBED_BUILD_DIR="$CRISPEMBED_SYS_LIB_DIR" \
  bash scripts/bundle_android_native_libs.sh --abi arm64-v8a --require-crispembed
cp "$OCR_ORT_ROOT/lib/libonnxruntime.so" src-tauri/gen/android/app/src/main/jniLibs/arm64-v8a/libonnxruntime.so
npx tauri android build --target aarch64 --features ocr-mobile --debug --apk \
  --ci --config src-tauri/tauri.ocr-mobile.conf.json
```

Preserve any additional target-specific Rust flags required by your own build.
For an Apple Silicon iOS simulator:

```sh
python3 scripts/build_mobile_crispembed.py \
  --source ../CrispEmbed --output ../ocr-native-ios \
  --lock scripts/ocr-mobile-runtime-lock.json --target ios-sim-arm64
export CRISPEMBED_SYS_LIB_DIR="$(cd ../ocr-native-ios/lib && pwd)"
export OCR_ORT_ROOT="$(pwd)/../ocr-ort-ios-simulator"
python3 scripts/prepare_mobile_ort.py --target ios-sim-arm64 --output "$OCR_ORT_ROOT"
python3 scripts/prepare_ocr_cargo_env.py --project . \
  --runtime-lib-dir "$CRISPEMBED_SYS_LIB_DIR" --protoc "$(command -v protoc)" \
  --ort-xcframework "$OCR_ORT_ROOT/onnxruntime.xcframework"
mkdir -p src-tauri/bin
cp ../ocr-native-ios/runtime-build.json src-tauri/bin/ocr-mobile-runtime.json
cp ../ocr-native-ios/notices/* src-tauri/bin/
cp "$OCR_ORT_ROOT/notices/LICENSE" src-tauri/bin/onnxruntime-LICENSE.txt
cp "$OCR_ORT_ROOT/notices/ThirdPartyNotices.txt" src-tauri/bin/onnxruntime-ThirdPartyNotices.txt
npx tauri ios init --ci --config src-tauri/tauri.ocr-mobile.conf.json
python3 scripts/prepare_ocr_ios_project.py src-tauri/gen/apple/project.yml \
  --ort-framework "$OCR_ORT_ROOT/onnxruntime.xcframework/ios-arm64_x86_64-simulator/onnxruntime.framework/onnxruntime"
npx tauri ios build --target aarch64-sim --features ocr-mobile --debug \
  --ci --config src-tauri/tauri.ocr-mobile.conf.json
```

The Android workflow also checks APK native-library alignment. The iOS profile
supplies the system frameworks used by the merged native archive, and the
generated-project helper links C++ while disabling signing only for the
simulator SDK. Device builds still require normal Apple provisioning.

The Cargo environment helper writes the exact native-library directory into the
isolated workspace root's Cargo configuration, which is found from either the
npm package root or the nested Rust project. This is required because the locked
iOS build launcher replaces the inherited environment before invoking Xcode.
Existing unrelated Cargo settings are preserved; a conflicting native-library
path is rejected. Do not commit the generated machine-specific configuration.
It also supplies forced `PROTOC` and the selected mobile ONNX Runtime settings:
`ORT_LIB_LOCATION`/`ORT_PREFER_DYNAMIC_LINK` on Android, or
`ORT_IOS_XCFWK_LOCATION` on iOS. These locations must match the selected target;
the helper rejects conflicting entries from a previous build.
Android retains the inherited environment, but the locked Tauri launcher supplies
its own target-specific Rust flags. The explicit flags above therefore retain
both its three Android libraries and the 16 KiB page-alignment arguments.

The [paired mobile workflow](../.github/workflows/ocr-mobile-native.yml) builds
Android ARM64/x86_64 and an iOS ARM64
simulator application against the same source lock. Its standalone native probe
runs known-angle correction and real baseline recognition on generated text,
with model and fixture hashes and failure observations preserved. Native and
application startup receipts are separate. These checks are defined in the
branch; inspect the exact workflow revision and results before treating a build
as qualified. A green browser fixture, link probe, native smoke, or startup
check does not establish physical camera capture, picker interaction, corpus
accuracy, or low-memory performance on a real phone.

The Android x86_64 lane additionally defines a test of the debug application's
Tauri commands through its own WebView socket. It checks the installed APK hash,
explicitly downloads the baseline through the app's private installer, verifies
the installed model hashes independently, imports PNG and multi-page PDF fixtures
through raw IPC, prepares and recognizes original/modified pages, reloads saved recognition settings, and
renders and discards a private reviewed PDF. Raw recognition and failure evidence
are retained. Its workspace UI gate supplies a synthetic image to the actual
camera-file input, saves the imported page, and triggers recognition through the
UI. The Android emulator gate also opens the real DocumentsUI picker, checks
cancellation, selects a pinned local PDF, verifies its provider URI/read grant,
and checks durable page import. These are defined gates; inspect their actual
run receipts before claiming execution. They do not establish physical-camera
capture, cloud-provider access, provider grants after restart/revocation, the
export save picker, or the extracted PDF text layer.
The iOS lane defines native inference and startup checks; it does not yet define
an equivalent application-command test.

## Released runtime integration constraints

The release recipes currently build Android and iOS without the `crispembed`
Cargo feature. The CrispEmbed `-sys` build script used by the pinned runtime
searches host build directories and links a dynamic library; its fallback CMake
invocation does not configure a mobile toolchain. In particular, the pinned iOS
release supplies a static archive, which that build script does not recognize
or link. Using those release artifacts for OCR still requires a matching mobile
link and packaging closure, model provisioning, and separate inference
qualification. The isolated desktop CPU OCR profile also relies on a helper
process. The paired mobile profile above uses linked native recognition, with
Tesseract as its initial default; the released runtime artifacts in this section
cannot substitute for that paired build.

The locked v0.17.12 `crispembed-android-arm64-v8a.tar.gz` contains only
`libcrispembed.so`. ELF inspection shows it needs `libggml.so`,
`libggml-cpu.so`, and `libggml-base.so`, which that archive does not contain.
It cannot be used alone in an APK. A replacement build or complete matching
release payload must provide the dependency closure. To stage a matching flat
native library directory, set `CRISPEMBED_BUILD_DIR` and run
`scripts/bundle_android_native_libs.sh --abi arm64-v8a --require-crispembed`.
The script checks the ELF architecture and transitive non-system dependencies,
and refuses conflicting same-named files from another runtime. These checks do
not verify runtime provenance or prove that the app builds or runs: verify the
native artifact and binding source against `scripts/crispembed-runtime-lock.json`
separately, and qualify the resulting APK on a device.

The locked Tauri dialog plugin 2.7.1 still returns `Folder picker is not
implemented on mobile`; Android batch import uses the dedicated SAF plugin above
instead. Full sorting on both platforms needs provider-aware destination
selection and operations, with explicit handling for providers that do not
support moves. The iOS flow compiled and passed focused simulator tests; it still must be
exercised through the real system picker and on a device. Both platforms need restart, revocation and
provider-backed move tests on actual devices.

The locked iOS `libcrispembed-static.a` also lacks the separately linked core
and ggml closure. `scripts/build_mobile_crispembed.py` prepares source-pinned
Android, iOS device, and iOS simulator builds without unpinned sibling sources.
The manual `mobile-crispembed-qualification.yml` workflow accepts
`target: android-x86_64` to build only an Android emulator candidate and link its
OCR C API probe with the NDK's `x86_64-linux-android24-clang`. The default `all`
still selects only Android ARM64 and the two iOS ARM64 targets; `android-armv7`
remains a separate candidate. Choose a repository-committed `runtime_lock`
(default `scripts/crispembed-runtime-lock.json`, or
`scripts/ocr-mobile-runtime-lock.json` for paired OCR bindings). The workflow
validates the committed lock before checkout and retains the library, notices,
source/build receipts, ELF inspection, and linked probe for 14 days under
`crispembed-mobile-candidate-android-x86_64`. It builds and links only; emulator
execution and complete-app qualification require the separate paired OCR lane.
The [build/link qualification](https://github.com/CrispStrobe/CrispSorter/actions/runs/40875226935)
completed successfully for Android ARM64, iOS ARM64, and iOS ARM64 simulator.
That older run built native libraries and linked probes; it did not execute
inference or build an OCR-enabled application. The paired OCR lane above supplies
separate app and execution gates. See
[HANDOFF_2026-10-29.md](HANDOFF_2026-10-29.md) for exact remaining steps.

## Release and TestFlight binding

The final publication gate requires the current version's Android APK and
provisioned iOS IPA, source-bound receipts, and matching downloaded SHA-256
hashes. Android also requires matching production-signing evidence. A draft
can still retain unsigned diagnostic artifacts, but publication must not
substitute them for these packages. Ordinary iOS and macOS TestFlight uploads
record the upload cutoff and select the exact uploaded build number; the resume
path requires the original cutoff explicitly. These checks validate artifact
identity and signing evidence, not physical-device behavior.

The existing v0.15.0 draft and internal TestFlight build were produced from an
earlier source revision. After the current source is qualified, prepare a new
version and source-bound draft, build each supported platform, verify its
receipts, and upload the new iOS build before reporting current-source
TestFlight coverage. Windows Authenticode and macOS Developer ID credentials
are still required for signed desktop publication; see
[desktop signing](desktop-signing.md).
