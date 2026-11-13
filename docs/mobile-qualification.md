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

For the error-propagation candidate, manually dispatch
`ocr-mobile-native.yml` with `target: android-x86_64` and
`candidate_runtime: true`. This checks out the exact source in the committed
`scripts/ocr-mobile-error-propagation-candidate-lock.json` and reuses artifact
`13985650215` from successful build/link run `43307953414`. The committed
`scripts/ocr-mobile-candidate-artifact-lock.json` binds the producing workflow
commit, archive SHA-256, and every library, notice, lock, receipt, and probe file.
The staging helper verifies safe extraction, the clean source/submodule pins,
notice bytes against source, and the Android dependency closure. A missing,
expired, or mismatched artifact fails the candidate lane without rebuilding.
The archive expires on 2026-11-26; a later replacement requires a newly reviewed
artifact pin. Default dispatches and pushes continue building the original OCR
runtime. Existing workflow concurrency remains in force.

The candidate lane runs the existing Android app build and emulator gates. Its
application build receipt records the explicit committed runtime lock and
`candidate-reuse.json`, rechecks the pinned staged files, and retains existing
packaged ELF, ONNX Runtime, notice, and source validation. The receipt tool's
optional `--runtime-lock` accepts only a repository-contained lock with exact
committed bytes; omitting it retains the production OCR lock. Preparing this
route does not establish a successful app build or executed emulator result.
The emulator runner forwards the selected lock and source through
`OCR_MOBILE_RUNTIME_LOCK` and `OCR_MOBILE_RUNTIME_SOURCE`. Before the app-command
consumer starts, its launcher validates that lock against the application and
runtime receipts, including the lock hash, source/submodule identities and
runtime receipt hash. Local invocations without these variables use the
production OCR lock and reject candidate receipts.

The candidate app-command lane additionally defines a bounded error/empty check
through the real WebView IPC, rebuilt Rust wrapper, and native pipeline. After
the normal baseline installation, it deliberately configures the installed,
hash-verified `dbnet-det` detector as an incompatible `unified` recognizer with
permissive numeric acceptance gates. Passing requires the exact Rust
pipeline-run failure message produced by the native NULL-full-text sentinel;
an earlier model/configuration rejection, timeout, or successful empty response
fails the gate. It retains the raw response/rejection before asserting.
It then imports a generated white 512×256 PNG through the app and recognizes it
using the existing DBNet/Tesseract English baseline, requiring zero regions,
the native empty text string, and a native review snapshot. The PNG and complete
returned diagnostics are retained. This adds no model download and only runs
when the validated app receipt contains candidate reuse provenance.

A passed candidate run would establish that this native initialization failure
reaches the rebuilt application's error boundary and that this Tesseract blank
remains a valid empty response. The white raster is not positive decoded OCR;
the existing text fixtures establish their own Tesseract recognition behavior.
These controls do not exercise Ovis/Qwen3.5 decoded output or its image patch
bound, failure after successful VLM initialization, EOS-only VLM text, recovery
in the same native context, prior-result clearing, or multi-stage success/blank fallback.
Those cases need separate native/model-specific and rebuilt-consumer execution.
Malformed PDF, missing-file/model, and policy errors elsewhere in the lane do
not establish that they crossed the native recognition boundary.
The [retained candidate observation](../tests/results/ocr-mobile-candidate/run-43313212722-boundary.json)
records a real native initialization failure reaching the rebuilt app wrapper
and WebView IPC in run `43313212722`. The run failed on an obsolete harness
error-context assertion; the exact current context is now checked against the
actual Rust source and frozen rejection. The blank and subsequent gates did not
execute. The corrected harness has not been rerun on an emulator.
The manual `ocr-mobile-native.yml` route with `candidate_replay: true` reuses the exact APK,
receipts and retained PNG from that run, recording original application and
corrected harness source identities separately. Its explicit
`candidate-boundary` qualification mode requires emulator/candidate provenance
and stops after the native rejection and Tesseract blank controls. It installs
only the already pinned English baseline through the app and has no app/native
build or host model-fixture download step. It does not report completion of the
default full OCR/UI/document/export flow. The original artifact must remain
available, with its committed hash pins intact; no replay has been executed.
Set `target: android-x86_64` for replay. This explicit input skips the registered
workflow's contracts and normal mobile build jobs; default dispatches and pushes
retain their existing path. Replay shares the original, unchanged workflow
concurrency block.

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
earlier source revision. The next source preparation uses 0.15.1 consistently in
the frontend, Tauri config, app Cargo manifest and both lockfiles. The release
preflight now checks those declarations before creating or changing a draft.
The independent workspace default and registry dependencies retain their own
versions. This preparation creates no tag, draft, binary or App Store upload;
the final integrated commit must be the new draft's exact source target.

The IPA upload identity check requires one provisioned application, the expected
bundle ID and version, a nonempty executable, and an exact numeric build number.
The retained upload route also requires its existing pinned SHA-256; it remains
available for that 0.15.0 artifact when its checks pass. Normal iOS uploads check
the archive against the current source version before contacting App Store
Connect. These archive checks supplement signing and source-bound receipts;
they do not prove that an artifact was built from a given commit or executed on
a device, and they introduce no approval requirement.

The upload-only `testflight_current` selector consumes the existing standard IPA
without rebuilding or publishing a release. Run it from the same source ref as
the completed draft build:

```sh
gh workflow run release.yml --repo CrispStrobe/CrispSorter --ref <same-source-ref> \
  -f platform=testflight_current -f mobile_profile=standard \
  -f publish=false -f mac_appstore=false
```

It checks that the current version's unpublished draft resolves to the exact
checkout SHA, then checks its standard iOS receipt's source/version/hash and the
downloaded IPA's bytes and identity. macOS verifies the actual extracted code
signature before upload. The actual IPA supplies the build number, and the UTC
cutoff recorded before upload binds internal distribution to that fresh build.
Public release and macOS App Store jobs cannot run under this selector; invalid
publication flags fail before artifact selection. There is no new consent gate.
If the source ref advances after the draft build, source matching rejects the
old receipt. Legacy upload/resume selectors retain their separate old IPA pin.

Current-source completion still requires new binaries for Linux, Windows, macOS,
Android and iOS, their matching source/version/hash receipts and required signing
evidence, and a newly uploaded iOS build selected by its exact build number and
upload cutoff. A retained upload route can use the new IPA only after its exact
version/hash are available from that source-bound artifact; the existing old
IPA cannot establish new-source TestFlight coverage. Windows Authenticode and macOS Developer ID credentials
are still required for signed desktop publication; see
[desktop signing](desktop-signing.md).

### Resume an accepted current-source TestFlight upload

`testflight_current_resume` is an upload-free recovery selector. Supply
`testflight_uploaded_after` with the **original** upload's UTC cutoff, exactly in
`YYYY-MM-DDTHH:MM:SSZ` form. The upload step prints that value before contacting
Apple. Do not substitute the retry time: processing/assignment recovery must
remain bound to the original accepted upload. Missing, malformed, naive or future
timestamps fail before any draft lookup or environment update.

Resume retains `publish=false`, `mac_appstore=false`, standard-profile admission,
exact current source/draft/tag/receipt binding, final IPA hash/size/version checks
and platform signature verification. It derives the build number from that
verified IPA. It then queries/assigns only the exact app marketing-version and
build-number pair uploaded at or after the explicit original cutoff. A reused
build number from another marketing version, missing version relationship,
missing exact build or non-VALID processing state cannot select another build.
There is no fallback to the newest or older valid build.

This selector never runs altool upload, rebuilds the app, starts another release
platform or publishes a release. It only recovers processing/group assignment
for an already accepted upload; an upload that was not accepted needs the
ordinary current-source upload selector instead. The existing legacy
`testflight_resume` refusal remains unchanged. No new manual approval is added.
The new route is source/test preparation, not evidence that a particular upload
or internal group assignment has completed.

## Current release execution

The standard v0.15.1 draft build
was dispatched from main commit `b739a3c0b70f39d3ae19e1bfcb412f3b44f77794`
with publication and App Store upload disabled. Android and iOS artifacts and
receipts completed; desktop jobs require their own results. Standard mobile
packages omit native OCR. The current-source TestFlight run
uploaded marketing version 0.15.1, build 0.15.1 and assigned the valid build to
internal testers. Use `testflight_current_resume` with the original logged UTC
cutoff if distribution recovery is needed; do not upload the same build again.

The independent signed OCR draft runs for iOS and Android used their own
recorded source commits and failed their final qualification gates as described
below. The iOS run retained an unqualified IPA; the Android run retained no APK.
Neither published or uploaded a package, and neither establishes physical-device
or OCR quality behavior.

The standard release's Windows leg failed before packaging: the locked
`ort-sys` build script assumed Cargo's older output-directory layout and could
not stage a native dependency under the runner's newer toolchain. The Linux,
macOS, Android and iOS legs continued independently. A Windows-only workflow
fix is being prepared; do not relabel another revision's binary as this draft's
source. Preserve the draft's exact target and record any separate workflow-tooling
revision if a narrow retry builds the original application commit.

The iOS OCR draft built and exported a signed IPA but failed final provenance:
the final executable did not expose the ONNX Runtime C API symbol required by
its validator. The retained package is explicitly unqualified and was not
uploaded. The executable contains ONNX Runtime version strings but those alone
cannot establish that the required code was linked. A draft-only symbol-retention
change is prepared for validation; keep the existing native/ORT provenance gate.


The Android OCR draft run checked out `336c584b21ba1a6a155e60e5b4c650b41693c572`.
Both Rust release compilations and the unsigned universal APK completed, then the
native license check correctly rejected changed Cargo.lock bytes. Signing and
final provenance never ran, and the artifact listing is empty: the retention step
reported no files. This run provides no signed or qualified Android artifact.

The mutation is reproduced exactly by **metadata resolution only**, with pinned
sibling manifests and no compilation. Initial/committed lock SHA-256 was
`cc0a1af878bec8ee7cbe6ea29a0a2981cf878fa1dc89a330cbe98092eb9c00b4`;
Cargo's normalized lock is
`2059e4df62369fb4e56db8ad6b539361d02d9f17f635ccc41da2607dbc813210`.
The only change moves `rustls` and `rustls-native-certs` after `rten` and
`rusqlite` in the application dependency array. Their insertion in the earlier
opt-in transport change did not follow Cargo's canonical order. Parsed packages,
versions, sources, checksums and dependency edges are identical; actual full
locked/unlocked metadata is identical and selects the same 865 backend licenses.
The logs bound mutation to after successful frontend license generation; they
cannot identify which of the subsequent Cargo invocations first rewrote it.

The local follow-up commits the canonical ordering and forwards `--locked` to
Cargo via the Android OCR draft's Tauri runner arguments. The strict post-build
license identity check remains in place. Regression controls run real offline
Cargo metadata on local-only packages using the committed dependency order, and
reproduce rejection of the historical drift. They do not compile or sign an APK.
No build was rerun, concurrency changed or publication attempted. The active
standard release source `b739a3c0b70f39d3ae19e1bfcb412f3b44f77794` remains unchanged;
a future build using this fix has its own source identity.


`b739a3c0` contains the same initial noncanonical lock bytes. A separate offline
metadata-only reproduction using the standard Android target **without native
OCR features** produces the identical `cc0a1af8…00b4` → `2059e4df…3210`
normalization. The standard build commands are also unlocked, so they can incur
this ordering rewrite. This establishes a reproducible possibility, not the
actual post-build lock bytes of the standard release: those bytes were not
retained or checked by this investigation.

The current standard `crispsorter-release-receipt-v1` records checkout HEAD,
package hashes, versions, runtime pins, sibling commits and applicable signature
checks. The earlier source-layout check records the clean starting tree/lock.
Neither receipt proves that Cargo.lock stayed byte-identical after compilation;
there is no final dependency-lock digest or post-build clean-lock assertion in
that standard receipt. Package hashes/signatures still identify their recorded
bytes, but HEAD alone does not prove the final lock bytes or dependency graph of
those packages. Our exact metadata-only reproduction found no dependency upgrade;
it does not establish every dependency used by an already built standard asset.
Keep this source-binding limitation explicit. A retry from b739 that permits
only this known metadata-only reorder must record both original/effective lock
hashes and the separate workflow-tooling identity; HEAD must not stand in for
proof of a byte-identical post-build tree. A future source commit containing the
canonical lock has its own application source identity.

The standard Linux release job built a DEB at the same release source, but its
live OCR-preset gate stopped before recognition because the Qwen model license
was not accepted. Its final release receipt therefore was not produced. The
retained DEB's published digest, package metadata, cold dynamic-library load,
version/doctor output and OCR-worker error protocol were independently checked.
A separate bounded one-core offline control ran that exact DEB against the
retained PDF and hash-verified cached DBNet/TrOCR models: it exited successfully
and produced the expected hOCR text. It performed no model download or Qwen
license acceptance. This is package-specific DBNet/TrOCR evidence, not a passed
Qwen preset gate or a substitute for the missing automated release receipt.

### Same-run signed iOS draft input retention (2026-11-13)

The signed iOS OCR draft now attempts allowlisted input retention after final
validation, including when that validation fails. `replay-inputs/` is uploaded
beside the recorder's `artifact/` directory for 14 days. It contains the original
`libcrispembed.a`, native receipt/notices, exact device ONNX framework and headers,
ORT receipt/notices, preparation/layout records, minimized Cargo binding summary,
and generated Cargo config/license inventory. The original Cargo metadata graph
is deliberately omitted; the summary records its original snapshot hash and
validated graph hash. The collector never walks the
native build directory, runner home, signing directories, or arbitrary paths
named by receipts. No separately built input may replace a missing original.

`replay-inputs.json` binds every retained file's bytes and SHA-256 to the source
commit/tree, production runtime/ORT lock hashes, run ID/attempt, and retained
unqualified IPA hash. `input_retention_status: complete` means these inputs were
copied, **not** that the signed app passed provenance, device execution, OCR
quality, or publication qualification. Failed/partial retention writes an
`incomplete` manifest and fails its step; the subsequent always-run artifact
upload can still preserve diagnostic inputs. It never writes a successful
`signed-draft.json` or bypasses the existing final verifier.

The explicit ORT file list must be reviewed if the pinned package changes. Each
file is regular, source-contained, size-bounded and checked against its original
receipt where applicable; receipts cannot add files to the copy list. Total
retained inputs are capped at 768 MiB (native archive at 512 MiB, graph at 64 MiB;
the current pinned ORT device binary is 38,398,152 bytes). Actual next-run size
will be recorded by the manifest; it has not been measured by another build.
The IPA is already retained separately and is hashed without duplicating it.
Staging uses hosted runner temporary storage, not local root storage.

Privacy: retained metadata includes original runner paths, dependency package
metadata, source identities and public license text. The Cargo config must match
exactly the generated three forced-path template; unrelated config, comments or
values are rejected. Every raw copied JSON record has an explicit recursive
field/type schema, including nested notices, components and license entries;
unknown or duplicate JSON fields fail before copying. The original recorder
applies these checks to its successful iOS metadata copies too, and uploaded
failure records omit raw exception text. Arbitrary Cargo workspace/package
metadata is never copied; only typed source/target/version/count/hash binding
fields enter the minimized summary. Existing public license text, author and
repository fields remain public package metadata; schema checks do not prove
that arbitrary text placed inside an otherwise allowed public field is secret
free. No environment dump, API key, certificate, keychain,
standalone provisioning profile, signing export directory, build log, or whole
checkout is copied. The separately retained signed IPA naturally contains its
embedded public provisioning/signature material.

Signed-app provenance replay still requires the exact app checkout and sibling
source commits, the original paths referenced by the retained Cargo
configuration (or a
separately reviewed mapping that never rewrites historical evidence), macOS
signature tools and the complete original signed IPA. Run the unchanged full
verifier; retention is no replacement for its source, native, ORT, notice,
signature, and app linkage checks. Full license-graph replay remains unavailable:
the original arbitrary Cargo metadata graph is intentionally not retained, and
the binding summary cannot substitute for it. The prior draft did not retain these
original inputs; its recovered symbol proof remains partial evidence and cannot
be upgraded by substituting another run's archive/framework. This change does
not rerun that build or enable TestFlight/public OCR distribution.

The pinned official iOS ORT ZIP was independently rechecked for retention on
2026-11-13: its committed 49,207,123-byte SHA-256 matched, and all 27 staged
device framework/header/plist/notice paths match the explicit allowlist, with
no additional device framework files omitted. This inspection was read-only
and did not build or qualify an application.

Both retention and signed-draft recorder CLI entry points suppress raw parser
and exception diagnostics. Failures print one fixed bounded category
(`invalid-input`, `invalid-record`, `invalid-arguments`, `input-output-error`,
`external-command-failed`, `external-timeout`, or `internal-error`) without a
traceback or input values. Malformed license URL/timestamp checks also use
constant parser errors. Subprocess regressions cover both secret-bearing input
examples through both CLIs; their incomplete/failure evidence remains explicit.

The successful draft exposed an upload transport gap: GitHub's default hidden-file
exclusion omitted the original `.cargo/config.toml` despite its inclusion in the
local manifest. The collector now stores these exact validated bytes at the
non-hidden `source/cargo-config.toml`, with `original_source_path` set explicitly
to `.cargo/config.toml` and the unchanged original content hash. Replay must
restore that precise original source location. No blanket hidden-file upload
is enabled. The earlier run's retention manifest therefore overstates actual
transport completeness; its missing config must not be silently substituted.

### Successful signed iOS OCR draft: independent artifact observation

The signed OCR draft from exact app source
`b5a42295848a1768a67f9936004a46d6ead9e19b` completed its final provenance
checks. Independent inspection verified the retained IPA SHA-256
`55b8bfaac04acaa09d1208265c56024e2b5639071cc5919021bfb49c79652232`
(111,932,948 bytes), matching source/tree/build receipt, original native/ORT
inputs and packaged notices, and the defined app `_OrtGetApiBase` symbol.
The existing production runtime source pin remains unchanged.

All 44 transported replay inputs matched their recorded hashes. The original
transport remains **incomplete**: its declared hidden Cargo config was omitted.
A separate reconstruction now provides exactly the original 387 config bytes:
SHA-256 `d0af5f9979e9d57844d1de2257197a7cc6fecb07bd89af1fe47422d00c7ac285`
matches both the historical retention manifest and final build receipt.
Reconstruction used the source-pinned template and same-run path evidence,
accepted only on the full original length/hash. The original artifact and
receipts were preserved unchanged; no later build inputs were substituted.
This makes the exact config bytes available for reviewed provenance replay
preparation; a full verifier replay was not performed locally. Full
license-graph replay remains unavailable because the arbitrary original Cargo
metadata graph was deliberately omitted.

Offline checks verified 76,586 signed executable pages, 21 resource hashes,
app/profile CMS mathematical signatures, matching app/profile entitlements,
and the app signer certificate's inclusion in its provisioning profile. Linux
lacks Apple's `codesign`/`security` tooling here: these checks do not establish
Apple trust-chain or code-signing requirements acceptance. The runner's signed
provenance success does not establish physical device execution or OCR quality.
No distribution, candidate promotion, or new build was performed for this
observation. Sanitized details are in
`tests/quality/results/2026-11-13/signed-ios-ocr-draft-independent-verification.json`.

### Paddle recognition preprocessing boundary correction

The live Paddle control from source `3ceb2838` failed during SVTR preprocessing,
after detector region cropping succeeded. The SHA-verified pinned
`usls 0.2.0-alpha.3` height-fit implementation computes a line width beyond the
fixed recognition canvas and passes that width to `CroppedImageMut`, which
rejects it. This is a recognizer resize boundary, not an out-of-page detector
box. The app now pre-resizes only over-wide regions to the recognizer's actual
model dimensions, keeping the complete region and leaving narrow-line
preprocessing unchanged. This follows Paddle's own recognition width-clamping
policy in its [recognizer preprocessing](https://github.com/PaddlePaddle/PaddleOCR/blob/main/tools/infer/predict_rec.py).

Four standalone geometry tests and one CPU-only pinned resize-backend regression
pass. The latter reproduces the exact original error, then checks that bounded
resizing succeeds and retains both image endpoints. No usls/model/ORT/GPU build,
whole-app build, live recognition rerun, or quality qualification was performed.
The existing live Paddle fixture/cache/table test still needs to validate the
full integration. Reproduction commands are in `tests/paddle-resize/README.md`;
sanitized findings are in
`tests/quality/results/2026-11-13/paddle-svtr-canvas-boundary.json`.

### Single Linux job for the existing live Paddle fixture

The existing CI workflow now accepts manual `scope: paddle-live`. It admits
only its current Linux Rust job. The original fixture test body and ordinary
Paddle command are unchanged. All other jobs and unrelated Linux test/CLI
steps are excluded, including the cloud live matrix even if `cloud_live=true`.
This is a manual preparation route; it has not been dispatched for this change.

The Linux job retains its existing sibling checkout layout, system/toolchain
setup, verified production native bundle, dependency cache, `npm ci`, frontend
build and resource placeholder. The separate frontend *check* job is skipped;
frontend assets needed by `tauri-build` still exist. The runner builds the same
feature-rich library test binary and executes exactly
`extractors::ocr_paddle::tests::live_paddle_recognises_the_fixture_sentence`
with `--locked --ignored --exact --nocapture --test-threads=1`. A cold cache can
still require a substantial application test-binary compilation; this narrows
execution, not the compilation dependency graph. No dependency manifest or
application lock was changed to make the route work.

The source guard requires the actual checked-out HEAD to equal the dispatch
SHA, checks committed production inputs and dependency pins, and rechecks
source/build identity after the test. The known generated license inventory is
recorded using the existing source guard's explicit exception; other tracked
source/lock changes fail closed. Zero matching tests, nonzero test exit, changed
inputs or capture failure cannot produce a passed receipt. The one existing
test installs its pinned Apache-2.0 detection/English/table models (20,172,994
bytes including vocabularies), checks the known fixture words, a cached second
recognition, and the table session. It is not a GPU comparison.

The unchanged job deadline is 120 minutes. The scoped command has a 90-minute
limit, two Cargo build jobs, one test thread, CPU-only device visibility and an
8 MiB diagnostic capture cap. A capture limit/deadline kills its process group;
partial diagnostics and a failed receipt remain explicit. A 30-second heartbeat
reports progress without dumping raw diagnostics. The always-run artifact step
retains `receipt.json` and bounded diagnostics for 14 days. Runner paths,
operation UUIDs and credential-shaped diagnostic lines are redacted. These
receipts do not establish release, GUI/device or corpus qualification.

Seventeen model-free selector/source/evidence tests pass, including exclusion
of every unrelated job and step, contradictory cloud-live inputs, preserved
frontend setup, exact single-test matching, source mutation, partial capture,
and unchanged concurrency bytes. YAML/actionlint checks also pass. The live
fixture and full application build remain unrun locally. For a future authorized
run, select the reviewed immutable PR revision and `scope=paddle-live`; this
record is not an execution receipt.

Scoped-runner review follow-up: Linux native archives now live in runner
temporary storage for download/verified extraction; the generic clean-source
guard still rejects either archive if it appears untracked in the checkout.
Capture failures at every phase, including read/wait/cleanup failures after
partial output, retain the bounded buffer under a generic failure code.

Diagnostic retention conservatively withholds the entire captured diagnostic
when quoted/nested/escaped credential keys, header name/value pairs, URI
userinfo/query/fragment data, standalone bearer credentials, private-key
material or ambiguous encoding is detected. `diagnostics_status` explicitly
states withholding; raw sensitive text is never copied to the receipt or
artifact. Normal native markers, tensor vectors and model progress remain
readable. Withholding is a privacy decision and does not alter the separate raw
test exit/count or source checks; it does not fabricate a model result.
Twenty-eight model-free selector/source/capture/privacy tests and actionlint
pass after these fixes. No live fixture execution is implied.

### Subsequent source-bound Paddle fixture result

The single Linux Paddle fixture was subsequently run on the reviewed source and
passed once. The retained receipt binds the test, source, lock and native inputs;
an independent audit checked those bindings and the test's word, cache-reuse and
table assertions. The diagnostic filter withheld the raw output, so the decoded
OCR text and raw test summary cannot be independently reconstructed from the
artifact. This result supersedes the earlier statement that the integration
fixture had not been run, but does not qualify general recognition quality,
other languages, mobile devices or the release license inventory.
