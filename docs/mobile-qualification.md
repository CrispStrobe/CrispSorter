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
complete apps. The [qualification run](https://github.com/CrispStrobe/CrispSorter/actions/runs/40838687391) passed four Android emulator tests and three iOS simulator tests, along with Rust and Swift cross-checks. The device checks below remain necessary before qualifying complete provider workflows.

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
reviewed PDF/text/structured exports without desktop helper processes. It omits
PDFium-dependent PDF import; the UI reports this capability limit instead of
offering an unavailable importer. Standard releases keep their existing profile.

The OCR workspace uses the native provider picker directly for JPEG, PNG, TIFF,
BMP and WebP images, with the provider's 16 MiB read limit. That differs from the
general batch extraction format list above. The system picker may show additional
file types, but the workspace validates each selection. PDF rasterization remains
unavailable in the default mobile OCR profile; camera/photo selection has its
separate 32 MiB byte limit.

A fresh mobile installation uses the compact native Tesseract/English default.
The workspace's **OCR models** controls explicitly install the detector and the
selected language model. Opening the workspace never starts a download. The
installer uses Rust HTTPS streaming and the exact paired registry's immutable
SHA-256 pins, writes a complete primary/companion set into a private version
directory, and atomically publishes its manifest. Recognition reads installed
models without spawning curl/wget or changing a process-wide cache environment.
Failed downloads preserve the previous complete installation. Other provisioned
engines remain selectable; models without published pins or with unmet license
requirements produce a visible error. Model versions are retained; automatic
model-cache garbage collection is not implemented.

Use a clean sibling CrispEmbed checkout and initialized ggml submodule at the
exact commits in scripts/ocr-mobile-runtime-lock.json. The native builder
checks both identities and refuses a dirty or mismatched source tree. Do not
substitute older released libraries for the paired bindings. Install the normal
Tauri platform toolchain and npm dependencies before the following commands.

For Android, with the NDK configured:

```sh
python3 scripts/build_mobile_crispembed.py \
  --source ../CrispEmbed --output ../ocr-native-android \
  --lock scripts/ocr-mobile-runtime-lock.json --target android-arm64
export CRISPEMBED_SYS_LIB_DIR="$(cd ../ocr-native-android/lib && pwd)"
export RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384"
npx tauri android init --ci --config src-tauri/tauri.ocr-mobile.conf.json
CRISPEMBED_BUILD_DIR="$CRISPEMBED_SYS_LIB_DIR" \
  bash scripts/bundle_android_native_libs.sh --abi arm64-v8a --require-crispembed
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
npx tauri ios init --ci --config src-tauri/tauri.ocr-mobile.conf.json
python3 scripts/prepare_ocr_ios_project.py src-tauri/gen/apple/project.yml
npx tauri ios build --target aarch64-sim --features ocr-mobile --debug \
  --ci --config src-tauri/tauri.ocr-mobile.conf.json
```

The Android workflow also checks APK native-library alignment. The iOS profile
supplies the system frameworks used by the merged native archive, and the
generated-project helper links C++ while disabling signing only for the
simulator SDK. Device builds still require normal Apple provisioning.

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

## Released runtime integration constraints

The release recipes currently build Android and iOS without the `crispembed`
Cargo feature. The CrispEmbed `-sys` build script used by the pinned runtime
searches host build directories and links a dynamic library; its fallback CMake
invocation does not configure a mobile toolchain. In particular, the pinned iOS
release supplies a static archive, which that build script does not recognize
or link. Mobile support needs target-specific link configuration, packaging,
model provisioning, and on-device inference tests before the capability is
advertised. The isolated desktop CPU OCR profile also relies on a helper
process; it needs a mobile-safe equivalent or an explicitly different profile.

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
The [build/link qualification](https://github.com/CrispStrobe/CrispSorter/actions/runs/40875226935)
completed successfully for Android ARM64, iOS ARM64, and iOS ARM64 simulator.
That older run built native libraries and linked probes; it did not execute
inference or build an OCR-enabled application. The paired OCR lane above supplies
separate app and execution gates. See
[HANDOFF_2026-10-29.md](HANDOFF_2026-10-29.md) for exact remaining steps.
