# Mobile qualification

The desktop browser suite uses simulated Tauri responses. A narrow viewport is
not an Android or iOS build, and a successful archive/signature inspection does
not establish that an app has launched on a device. The mobile build's native
CrispEmbed capability is currently absent. Real on-device performance and OCR
parity remain unqualified until the native runtime, model files and app bindings
have been built and exercised on the target.

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
  --artifact CrispSorter_0.13.0_android-aarch64-dev-signed.apk --output android-smoke
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
provider access must produce a visible error. The iOS batch picker and sorting
are disabled until original identities and security-scoped access can be
preserved; other mobile picker entry points are not covered by this flow.

`.github/workflows/mobile-provider-check.yml` builds the Android plugin and
exercises its reader on an emulator against a test ContentResolver provider.
The instrumentation covers nested folders, case-sensitive document IDs, reads,
unavailable access, size limits and folder cycles. It does not exercise the
system picker, a real provider's persistent grants, the complete APK, or an
iOS app. The workflow has been added; its result and the device checks below
must be recorded before qualifying this integration.

For qualification, use a dedicated device or emulator and disposable local and
cloud-provider fixtures. Record the APK build and OS/device, and verify picker
cancellation, individual and nested folder selections, Unicode and case-sensitive
names, unsupported and oversized files, extraction, app restart, and provider
access after revocation. Confirm that provider items cannot enter any move,
copy, delete or script operation and that selecting a new grant after revocation
restores access. Inspect errors and confirm no local copy was presented as the
original. The historical `mobile_fs` module and `android-src/SAFBridge.kt` are
not registered and still contain stubs; they are not the implementation used by
this flow.

## Remaining native integration blockers

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
instead. Full Android sorting needs provider-aware destination selection and
operations, with explicit handling for providers that do not support moves.
iOS needs a directory picker, security-scoped bookmarks captured while access is
valid and restored on launch, and coordinated file operations. Both platforms
need restart, revocation and provider-backed move tests on actual devices.
