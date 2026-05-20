# Design: Lite + Full installer variants

## Status
**Draft, no code yet.** Sketch for review.

Supersedes an earlier draft that proposed runtime `libloading` of the
native libs. That approach was rejected — see "Alternatives considered"
below.

## Problem statement

Today the release binaries co-bundle CrispEmbed + CrispASR native
libraries:

| Platform | Bundle | Size | DLLs/dylibs |
|---|---|---|---|
| macOS arm64 | `.app.tar.gz` / `.dmg` | ~280 MB | bundled in `.app/Contents/Frameworks/` |
| Linux x86_64 | `.deb` | ~290 MB | bundled in `/usr/lib/crispsorter/` |
| Windows x64 | portable `.zip` | ~310 MB | bundled next to `.exe` |

Two pain points:

1. **Bundle size.** ~290 MB is large for a desktop installer; ~80% of
   it is CrispEmbed + CrispASR's ggml + backend libs.
2. **Many users only want the cloud-LLM path** (translation /
   summarisation via OpenAI / Anthropic / etc.) and never touch
   offline NMT, alignment, or local embeddings. They pay the full
   bundle cost for nothing.

Out of scope here: the Windows MSI/NSIS DLL-placement bug. That is a
separate WiX fix tracked elsewhere; v0.2.0's portable `.zip` is the
tactical workaround in the meantime.

## Proposed solution

Ship **two installer variants** per platform, picked at download time:

| Variant | Size | Features available | GPU backend |
|---|---|---|---|
| `crispsorter-full-*` | ~290 MB | everything (cloud LLMs + offline embed/ASR/NMT/alignment) | one per platform — see "GPU backend axis" |
| `crispsorter-lite-*` | ~50 MB  | cloud LLMs only | n/a |

Lite is the same source tree built with a Cargo feature flag that
**compiles out** the embed / ASR / alignment code paths. No runtime
dynamic loading, no FFI surgery — the lite binary simply doesn't link
or bundle the heavy libs, and the UI code that would call them is
gated behind `#[cfg(feature = "native_ml")]` / equivalent JS feature
checks.

### GPU backend axis

`crispembed-sys` and `crispasr-sys` already expose `vulkan`, `metal`,
and `cuda` Cargo features. Today's `release.yml` picks one per
platform:

| Platform | Backend shipped today |
|---|---|
| macOS arm64 | Metal |
| Windows x64 | Vulkan |
| Linux x64   | Vulkan |

Vulkan covers NVIDIA + AMD + Intel + most integrated GPUs on
Linux/Windows; Metal covers everything on Apple Silicon. **The full
variant keeps this 1-backend-per-platform choice.** It is *not* a
fat bundle of CUDA + Vulkan + CPU — that would push the macOS-less
platforms past 1 GB (CUDA runtime alone is ~700 MB with cuBLAS) and
re-create the bundle-size problem this design is trying to fix.

Trade-off accepted: NVIDIA users on Linux/Windows run Vulkan, not
CUDA. For CrispSorter's workloads (text embeddings, small Whisper
models, sentence-level NMT) Vulkan is within ~10-20% of CUDA on
consumer cards — fine. Power users who want CUDA can build from
source; if demand surfaces, we add a third variant per affected
platform:

```
crispsorter-full-{macos-arm64, windows-x64, linux-x64}            ← today
crispsorter-full-cuda-{windows-x64, linux-x64}                    ← future, if needed
crispsorter-lite-{macos-arm64, windows-x64, linux-x64}            ← today
```

Each backend variant is its own Cargo feature combo, its own CI
matrix row, and its own signed installer. No runtime probing, no
lazy backend download. The whisper.cpp / llama.cpp release pages
follow this same "many variants, user picks" pattern.

ROCm (AMD on Linux) and SYCL (Intel) are not on the roadmap. AMD
users get Vulkan, which works on RDNA cards.

### What the user sees

- Download page offers both: "Full (290 MB) — includes offline
  translation and audio" / "Lite (50 MB) — cloud LLMs only".
- A lite user who later wants offline features clicks **Settings →
  Switch to full version**, which opens the full installer's download
  URL. The Tauri auto-updater handles the swap; settings, caches,
  history persist across the upgrade because the install ID and
  config dir don't change.
- A full user who wants to slim down can downgrade the same way
  (less common, but supported).

### Architecture

```text
            ┌─────────────────────────────────────────┐
            │ Cargo workspace                         │
            │                                         │
            │  ┌────────────┐    ┌────────────────┐  │
            │  │ crispembed │    │ crispasr-sys   │  │
            │  │   -sys     │    │                │  │
            │  └─────┬──────┘    └────────┬───────┘  │
            │        │ optional dep        │ optional │
            │        ▼                     ▼          │
            │  ┌─────────────────────────────────┐   │
            │  │  src-tauri (#[cfg] gated)       │   │
            │  │  feature = "native_ml"          │   │
            │  └─────────────────────────────────┘   │
            └─────────────────────────────────────────┘
                     │                    │
              build with                build with
              --features native_ml      --no-default-features
                     │                    │
                     ▼                    ▼
              crispsorter-full       crispsorter-lite
              (~290 MB)              (~50 MB)
```

### Cargo feature wiring

`src-tauri/Cargo.toml`:

```toml
[features]
default = ["native_ml"]
native_ml = ["dep:crispembed-sys", "dep:crispasr-sys", "dep:crispalign"]
# lite = no features; cloud LLMs always compiled in
```

Call sites that touch the native libs are gated:

```rust
#[cfg(feature = "native_ml")]
mod embed;
#[cfg(feature = "native_ml")]
mod asr;

#[tauri::command]
fn supports_offline_translate() -> bool {
    cfg!(feature = "native_ml")
}
```

Frontend hides offline-only UI when `supports_offline_translate()`
returns false. The cloud-LLM tabs render identically in both builds.

### CI

`release.yml` matrix gains a `variant: [full, lite]` axis. Each
existing platform target builds twice; assets are named
`crispsorter-{full,lite}-{platform}.{ext}`. Both variants are signed
and notarized through the same pipeline; nothing in the signing path
changes because each artifact is a self-contained signed unit.

### "Switch to full" flow

A Settings button in the lite build:

```text
┌─────────────────────────────────────────────────────────┐
│ This is CrispSorter Lite (50 MB)                        │
│                                                         │
│ Offline translation, speech transcription, and          │
│ embeddings are not available. Switch to the full        │
│ version (290 MB) to enable them.                        │
│                                                         │
│            [Download full version]                      │
└─────────────────────────────────────────────────────────┘
```

The button opens the platform-appropriate full installer URL via the
Tauri shell-open API. Tauri's built-in updater handles the cross-
variant install if both share a bundle identifier (they do).
Per-user data lives in the OS config dir under that bundle ID, so it
survives the switch.

### Implementation tasks

| Task | Where | Effort |
|---|---|---|
| Add `native_ml` Cargo feature; gate existing imports | `src-tauri/Cargo.toml` + Rust modules | ~3h |
| `#[cfg]`-gate Tauri commands; expose `supports_offline_translate` | `src-tauri/src/lib.rs` | ~2h |
| Frontend feature-flag plumbing + hide offline UI | `src/lib/` | ~3h |
| "Switch to full version" Settings panel | `src/lib/components/Settings.svelte` | ~2h |
| `release.yml`: add `variant` matrix axis | `.github/workflows/release.yml` | ~3h |
| Download-page README update with two-variant explanation | `README.md` | ~1h |
| Smoke-test both variants end-to-end on macOS/Linux/Windows | manual | ~3h |
| **Total** | | **~17 hours / ~2 days** |

### Open questions

1. **Default download on the GitHub release page.** Recommendation:
   list both, with Full as the recommended option. Lite gets a "for
   cloud-only users" subtitle. Don't auto-detect — users are bad at
   knowing in advance whether they'll want offline.
2. **Linux packaging.** The `.deb` already declares its bundled libs;
   the lite `.deb` simply omits them and shrinks its install size.
   Same package name, different version metadata? Or two distinct
   packages (`crispsorter` vs `crispsorter-lite`)? Recommendation:
   two packages — apt doesn't gracefully handle "same name, different
   contents".
3. **Telemetry on which variant users pick.** None for v0.3.0; revisit
   if the split is uneven enough that we should rethink the default.

## Alternatives considered

**Runtime `libloading` + first-launch download of dylibs/DLLs.** Was
the original draft of this doc. Rejected because (a) the proposed
exact-version pinning made the "decoupled release cadence" benefit
illusory — a sibling-lib bugfix still needs a CrispSorter rebuild to
bump the pin, (b) macOS Library Validation refuses to `dlopen` dylibs
not signed by the same Team ID, and disabling it weakens the app's
hardened-runtime posture, (c) the `-sys` crate refactor to dual-mode
(static link *or* `libloading` handle) is the long pole and doubles
test surface for marginal benefit.

**Sidecar processes** (CrispEmbed + CrispASR as standalone HTTP/stdio
servers, spawned as Tauri sidecars, downloaded lazily). Architecturally
cleaner — clean process isolation, libs upgrade independently, matches
Ollama / language-server patterns — but ~1 week of work to refactor
the existing FFI call sites into JSON-over-socket clients, and it
postpones the installer-size win behind that refactor. Worth
revisiting if we later want hot-swappable model backends or want
CrispEmbed/CrispASR to be reusable outside CrispSorter; not the right
trade today.

**Status quo + WiX fix only.** Cheapest option (~3h for the Windows
installer fix, nothing else). Doesn't address bundle size or the
cloud-only user's pain. Acceptable as a stopgap but not a destination.

## Status

Phase 2 of the installer story. Phase 1 (v0.2.0 portable `.zip`) is
shipped. This phase replaces both bundled and portable Windows
artifacts with the full/lite split once the WiX MSI fix lands in
parallel.

If we proceed: ~2 days of work, single developer, single PR.
