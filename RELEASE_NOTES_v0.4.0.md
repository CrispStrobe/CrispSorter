# CrispSorter v0.4.0

## Highlights

**CrispSorter runs on Android.** The full app — LanceDB, Tantivy, fastembed,
OCR, PDF extraction, cloud-backup sync, batch sort — cross-compiles for
`aarch64-linux-android` with 100% feature parity. No features removed, no
thin-client compromises. Sort your Downloads folder on your phone the same
way you sort documents on your desktop.

**Mobile-first UI.** A responsive bottom tab bar replaces the sidebar on
phones (<768px). All core components (Stapel, Chat, Übersicht, Translate,
Settings) adapt to touch-sized targets and stacked layouts. Tablets get an
icon-only collapsed sidebar.

---

## Android support

- `tauri android init` generates the Gradle project under `src-tauri/gen/android/`
- Full Rust workspace cross-compiles for `aarch64-linux-android` (arm64)
- Vendored OpenSSL (`openssl = { features = ["vendored"] }`) — no system libssl needed
- Patched `lance-linalg` for Android aarch64 NEON f16 kernels
- Platform-scoped Tauri capabilities: `default.json` (desktop: shell + process) vs `mobile.json` (no shell/process, scoped filesystem)
- CI: `release-android` job in `release.yml` — JDK 17, Android SDK/NDK 26.3, builds + uploads APK to GitHub release

## `desktop` feature flag

Desktop-only code is now gated behind `--features desktop`:

| Gated behind `desktop` | Why |
|---|---|
| `tauri-plugin-shell` / `tauri-plugin-process` | No subprocess spawning on Android/iOS |
| `mistralrs` (local LLM inference) | Sidecar model — spawns `llama-server` |
| `notify` (folder watcher) | No inotify on Android |
| Sidecar commands (llama.cpp, MLX, Ollama) | OS-level process spawning |
| TTS module (`tts/mod.rs`) | Spawns `say`/`espeak`/PowerShell |

**Desktop CI/release builds must add `--features desktop`.** Mobile builds omit it. All batch sort, indexing, search, sync, and cloud LLM features work on both platforms without the flag.

## Responsive UI

- **Phone (<768px):** Sidebar hidden, bottom tab bar with 5 tabs (Stapel, Chat, Übersicht, Translate, Settings). `safe-area-inset-bottom` for iOS notch.
- **Tablet (768–1024px):** Sidebar collapses to icon-only (64px).
- **Desktop (>1024px):** Unchanged.
- `BatchReview`: stacked layout on mobile (detail pane below table instead of beside), touch-sized buttons.
- `Settings`: horizontal scrollable tab bar on phone instead of vertical sidebar.
- `Chat`: sidebar hidden on phone, compact header.
- `IndexIngest` / `Translate`: scrollable tabs, tighter padding.

## Build environment

To build Android locally:

```bash
export JAVA_HOME=/path/to/jdk-17
export ANDROID_HOME=/path/to/android-sdk
export NDK_HOME=$ANDROID_HOME/ndk/26.3.11579264
export PROTOC=/path/to/protoc
NDK_BIN=$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
export CC_aarch64_linux_android=$NDK_BIN/aarch64-linux-android24-clang
export CXX_aarch64_linux_android=$NDK_BIN/aarch64-linux-android24-clang++
export AR_aarch64_linux_android=$NDK_BIN/llvm-ar
export RANLIB_aarch64_linux_android=$NDK_BIN/llvm-ranlib
# Symlink NDK tools for vendored OpenSSL
for tool in ranlib ar strip nm objdump readelf; do
  ln -sf "$NDK_BIN/llvm-$tool" "$NDK_BIN/aarch64-linux-android-$tool"
done
export PATH="$JAVA_HOME/bin:$NDK_BIN:$PATH"

npx tauri android init   # first time only
npx tauri android build --target aarch64
```

## Files changed

| File | Change |
|---|---|
| `Cargo.toml` (workspace) | `[patch.crates-io]` for lance-linalg Android fix |
| `src-tauri/Cargo.toml` | `desktop` feature flag, vendored OpenSSL, optional deps |
| `src-tauri/build.rs` | Skip rpath on Android/iOS |
| `src-tauri/src/lib.rs` | `#[cfg(feature = "desktop")]` on 12+ commands, dual handler |
| `src-tauri/src/bg_ingest/mod.rs` | Watcher references gated |
| `src-tauri/capabilities/default.json` | Scoped to desktop platforms |
| `src-tauri/capabilities/mobile.json` | New — mobile permissions |
| `src-tauri/tauri.conf.json` | Removed fixed window size, added iOS config |
| `src/routes/+page.svelte` | Mobile bottom tab bar + responsive CSS |
| 4 component .svelte files | Mobile responsive breakpoints |
| `.github/workflows/release.yml` | `release-android` CI job |
| `PLAN.md` | Phase 3 un-deferred |
