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

The repository also contains a historical `mobile_fs` module and a Kotlin SAF
helper, but they are not registered with the application and their Android JNI
and iOS security-scoped methods contain explicit stubs. Their presence does not
establish persistent provider access. The active picker/filesystem integration
must be checked on devices, including revoked permissions and app restart, before
claiming persistent folder access or wiring file moves through those stubs.
