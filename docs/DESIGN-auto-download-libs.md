# Design: Lite binaries + first-launch lib auto-download

## Status
**Draft, no code yet.** Sketch for review.

## Problem statement

Today the release binaries for CrispSorter co-bundle CrispEmbed +
CrispASR native libraries:

| Platform | Bundle | Size | DLLs/dylibs |
|---|---|---|---|
| macOS arm64 | `.app.tar.gz` / `.dmg` | ~280 MB | bundled in `.app/Contents/Frameworks/` |
| Linux x86_64 | `.deb` | ~290 MB | bundled in `/usr/lib/crispsorter/` |
| Windows x64 | portable `.zip` | ~310 MB | bundled next to `.exe` |

Three pain points:

1. **Bundle size**. ~290 MB is large for a desktop installer; ~80% of
   it is CrispEmbed + CrispASR's ggml + backend libs.

2. **Couples release cadence**. CrispSorter releases drag a frozen
   snapshot of CrispEmbed + CrispASR. A bugfix in `crispembed` v0.2.7
   needs a fresh CrispSorter release to reach users, even though
   nothing in CrispSorter itself changed.

3. **Windows installer is a dead end.** The NSIS / MSI installers
   Tauri produces put DLLs at `<install>/resources/bin/` which the
   Windows loader can't find. v0.2.0 ships a portable `.zip`
   instead. Users wanting a "proper" install (Start menu entry,
   uninstaller, signed installer) have no path.

## Proposed solution

A new **`lite`** Cargo feature that swaps the link-time linkage to
CrispEmbed + CrispASR for **runtime dynamic loading via `libloading`**.
The shipped binary becomes ~50 MB; on first launch (or when the user
enables a feature that needs the libs), CrispSorter downloads the
required dylibs/DLLs from each repo's GitHub release into the user's
cache dir.

### Architecture

```text
                ┌────────────────────────────────┐
                │ CrispSorter.exe (lite)         │
                │ - no link-time crispembed*     │
                │ - no link-time crispasr*       │
                │ - imports libloading           │
                └─────────────┬──────────────────┘
                              │ at startup
                              ▼
        ┌───────────────────────────────────────────────────┐
        │ libs/manager.rs                                   │
        │ - cache dir resolution (per OS)                   │
        │ - manifest of expected libs + version + sha256    │
        │ - check filesystem; download missing/wrong-hash   │
        │ - extract; place in cache dir                     │
        │ - dlopen / LoadLibrary; resolve symbols           │
        └───────────────────────────────────────────────────┘
                              │
                ┌─────────────┴──────────────┐
                ▼                            ▼
   ~/.cache/crispsorter/         %LOCALAPPDATA%/crispsorter/
     libs/                         libs/
       libcrispembed.0.2.6.dylib     crispembed-v0.2.6/
       libcrispasr.0.5.7.dylib         crispembed.dll
       libggml-*.dylib                 ggml-*.dll
                                     crispasr-v0.5.7/
                                       crispasr.dll
                                       ...
```

### Cache layout

Per-OS resolution via the `dirs` crate:

| OS | Path |
|---|---|
| macOS | `~/Library/Caches/com.<user>.crispsorter/libs/` |
| Linux | `~/.cache/crispsorter/libs/` |
| Windows | `%LOCALAPPDATA%\crispsorter\libs\` |

Inside, one subdirectory per upstream release tag:

```text
libs/
├── crispembed-v0.2.6/
│   ├── libcrispembed.0.2.6.dylib  (or .so / .dll)
│   ├── libggml-base.dylib
│   ├── libggml-cpu.dylib
│   ├── libggml-metal.dylib  (macOS)
│   ├── libggml-vulkan.so    (Linux)
│   ├── ggml-vulkan.dll      (Windows)
│   └── SHA256SUMS
├── crispasr-v0.5.7/
│   └── ...
└── manifest.json
```

`manifest.json` records which versions the running binary expects:

```json
{
  "crispembed": { "version": "v0.2.6", "arch": "macos-arm64", "verified_at": "2026-05-20T11:30:00Z" },
  "crispasr":   { "version": "v0.5.7", "arch": "macos-arm64", "verified_at": "2026-05-20T11:30:00Z" }
}
```

A schema-version field lets us migrate the cache on breaking changes.

### Download flow

1. **Resolve required versions.** Versions are hard-coded in the
   binary at build time (from `Cargo.toml`'s `crispembed-sys`
   version constraint). Released CrispSorter always knows what
   versions it needs.

2. **Check cache.** Read `manifest.json`, verify SHA256SUMS, confirm
   the libs are present.

3. **If anything missing/mismatched**, fall into the **download
   flow**:
   - GET the release asset URL from GitHub:
     `https://github.com/CrispStrobe/CrispEmbed/releases/download/v0.2.6/crispembed-macos-arm64.tar.gz`
   - Verify the asset's published SHA256 against a hash we baked
     into the binary at build time (prevents MITM / asset
     substitution).
   - Download with progress reporting (Tauri command emits
     `libs://download/progress` events to a frontend modal).
   - Verify the downloaded file's SHA256.
   - Extract to the cache dir.
   - Update `manifest.json`.

4. **Load.** Use `libloading::Library::new(&cached_path)` to
   `dlopen` / `LoadLibrary` each lib. Pass the handle into the
   existing `crispembed-sys` / `crispasr-sys` shims (refactored
   to take a Library handle instead of statically linking).

5. **Symbol resolution.** Each `-sys` crate gains a
   `LibraryFromHandle` constructor that pulls `crispembed_init`,
   `crispembed_encode`, etc. out of the loaded library and stashes
   them in a struct. Function call sites go from `unsafe { crispembed_init(...) }`
   to `unsafe { (handle.encode)(...) }`.

### Library version pinning

CrispSorter's Cargo.toml pins both `crispembed-sys` and `crispasr-sys`
to exact versions; the runtime check refuses to load mismatched
libraries. This prevents "user upgraded their cached lib, CrispSorter
crashes because the ABI moved" failure modes.

When CrispSorter bumps to a newer sibling-lib version, the next
launch sees the manifest mismatch and re-downloads.

### UI: first-launch experience

A modal appears on first launch (or whenever a feature is first
activated that needs the libs):

```text
┌─────────────────────────────────────────────────────┐
│ Downloading native libraries (one-time)             │
│                                                     │
│ CrispSorter uses two optional native runtimes for   │
│ embeddings and speech / translation. Downloading    │
│ them now keeps the installer small.                 │
│                                                     │
│ [████████░░░░░░░░░░] crispembed v0.2.6  78 MB / 90 MB │
│ [░░░░░░░░░░░░░░░░░░] crispasr   v0.5.7   0 MB / 145 MB │
│                                                     │
│ [Skip — translate via cloud LLMs only]    [Cancel]  │
└─────────────────────────────────────────────────────┘
```

`Skip` is non-destructive — the user can use cloud LLM providers,
just no offline NMT / alignment / embeddings. Re-trigger the
download from Settings → Native libraries.

### Implementation tasks (rough scope)

| Task | Where | Effort |
|---|---|---|
| Add `lite` Cargo feature; gate the existing link-time path | `src-tauri/Cargo.toml` | ~1h |
| `libs/manager.rs` — cache dir, manifest, sha256 check | new module | ~6h |
| `libs/downloader.rs` — http + progress + extraction | new module | ~4h |
| Refactor `crispembed-sys` to optionally use `libloading` | sibling crate | ~12h |
| Refactor `crispasr-sys` to optionally use `libloading` | sibling crate | ~12h |
| `libs_download_*` Tauri commands + progress events | `src-tauri/src/libs/` | ~3h |
| `Settings → Native libraries` UI section | `src/lib/components/Settings.svelte` | ~4h |
| First-launch modal | new component | ~3h |
| Bake hash table into the binary at build time | `build.rs` | ~2h |
| `release.yml`: build the `lite` variant alongside the bundled one | CI | ~3h |
| Migration: existing v0.2.0 users' settings.json picks up the lite path on next launch | settings migration | ~2h |
| **Total** | | **~52 hours / ~1.5 weeks** |

### Open questions

1. **Should `lite` be the default?** Pro: smaller installer, faster
   download for users who only want cloud LLMs. Con: every first-launch
   needs network access to be useful. *Recommendation: ship both
   variants, default = bundled (status quo), lite is a separate
   download for users who want it.*

2. **What about users behind corporate proxies / air-gapped?**
   The bundled variant remains available. Lite explicitly states
   "needs internet for first launch."

3. **Signed binaries?** If we eventually ship a code-signed Windows
   installer with the lite binary, we'd still need to download
   unsigned DLLs at runtime — which Windows SmartScreen / corporate
   security will object to. Mitigation: pin DLL SHA256s in the
   binary and refuse to load mismatches.

4. **Library version drift between CrispSorter and the sibling
   repos.** If a user uses CrispSorter at v0.2.0 but updates the
   cache by hand, they might trigger an ABI mismatch. The manifest
   check refuses. *Don't auto-update the cache silently.*

5. **Should we publish the bundled-variant Windows installer once
   the WiX fix lands?** Yes — bundled remains the user-friendly
   default once the DLL placement is resolved (separate work).

### Status

This is a Phase-2 design. Phase 1 (the v0.2.0 Windows portable .zip)
is a tactical fix that gives Windows users a working binary today.
Lite + auto-download replaces both the bundled variant and the
portable variant once it lands.

If we proceed: spec lives in `docs/DESIGN-auto-download-libs.md`,
implementation phase opens its own session (~1.5 weeks, single
developer).
