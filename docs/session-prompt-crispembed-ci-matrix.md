# Session prompt — bring CrispEmbed CI to CrispASR parity

Use this as the opening prompt for a fresh session. The work is **all
in the `CrispEmbed` repo** (sibling of `CrispSorter`); you only touch
`CrispSorter` at the end to drop the "GPU prebuilt is CPU-only"
warning once the matrix lands.

## Repos

- `C:\Users\stc\Downloads\code\CrispEmbed` — primary
- `C:\Users\stc\Downloads\code\CrispASR` — reference for the matrix
- `C:\Users\stc\Downloads\code\CrispSorter` — touched lightly at the end

## Goal in one sentence

Make `CrispEmbed`'s GitHub release publish the same per-target
prebuilt-library matrix that `CrispASR` already does, so CrispSorter's
`enable-crispembed.{ps1,sh}` can pull a CUDA / Vulkan / Metal-enabled
`crispembed.dll` (or `libcrispembed.dylib` / `.so`) without making
users build from source.

## What CrispASR ships today (target shape)

```
crispasr-windows-x86_64-cpu.zip                ~2 MB     CLI, CPU
crispasr-windows-x86_64-cpu-legacy.zip                   CLI, SSE4.2 fallback
crispasr-windows-x86_64-vulkan.zip            ~22 MB     CLI + Vulkan ggml DLLs
crispasr-windows-x86_64-cuda.zip             ~650 MB     CLI + CUDA ggml + cudart
crispasr-linux-x86_64.tar.gz                             CLI bundle
crispasr-macos.tar.gz                                    CLI bundle
crispasr-python-linux-x86_64.tar.gz                      Python wheel + lib
libcrispasr-windows-x86_64.tar.gz                        static lib + headers, CPU
libcrispasr-windows-x86_64-cpu-legacy.tar.gz             SSE4.2
libcrispasr-windows-x86_64-vulkan.tar.gz      ~41 MB     + Vulkan ggml
libcrispasr-windows-x86_64-cuda.tar.gz       ~745 MB     + CUDA ggml + cudart
libcrispasr-linux-x86_64.tar.gz                          Linux CPU
libcrispasr-linux-x86_64-cuda.tar.gz                     Linux CUDA
libcrispasr-macos-arm64.tar.gz                           macOS Metal
```

The `lib*.tar.gz` rows are the load-bearing ones for downstream Rust
apps. Each has the standardised tree that `crispasr-sys/build.rs`
probes:

```
<bundle>/
  src/
    Release/
      crispasr.lib
  ggml/
    src/
      Release/
        ggml.lib
        ggml-cuda.lib   (or -vulkan.lib / -metal.dylib)
  include/
    crispasr.h
  ggml/
    include/
      ggml.h
      ...
```

## What CrispEmbed ships today (current state)

```
crispembed-windows-x86_64.zip       1.17 MB   CLI only, CPU
crispembed-linux-x86_64.tar.gz      1.52 MB   CLI only, CPU
crispembed-macos-arm64.tar.gz       1.41 MB   CLI only, CPU
crispembed-linux-arm64.tar.gz       1.45 MB
crispembed-android-{arm64,armeabi}.tar.gz
crispembed-ios-arm64.tar.gz
```

i.e. **no library tarballs at all**, **no GPU variants**.
`crispembed-sys/build.rs` already has env-var + in-tree-build fallback
logic, but with no published lib bundle the only options downstream
are: clone the repo + run `cmake` (~15 min) or pull the CPU-only CLI
zip (which embeds the .lib but lacks the GPU ggml libs).

## Concrete delta (file-by-file)

### 1. `CrispEmbed/.github/workflows/release.yml`

Mirror `CrispASR/.github/workflows/release.yml`. Specifically copy the
job IDs and adapt:

- `build-libs-windows-x86_64`            (CPU lib bundle)
- `build-libs-windows-x86_64-cpu-legacy` (SSE4.2 fallback)
- `build-libs-windows-x86_64-vulkan`     (`-DGGML_VULKAN=ON`)
- `build-libs-windows-x86_64-cuda`       (`-DGGML_CUDA=ON`, sets up CUDA Toolkit action)
- `build-libs-linux-x86_64`              (CPU + OpenBLAS)
- `build-libs-linux-x86_64-cuda`         (CUDA-Linux)
- `build-libs-macos-arm64`               (Metal)

Each job's package step builds the standardised tree:

```yaml
mkdir -p "$OUT/src" "$OUT/src/Release" "$OUT/ggml/src" "$OUT/ggml/src/Release" "$OUT/include" "$OUT/ggml/include"
cp build-vulkan/src/Release/crispembed.lib   "$OUT/src/Release/"
cp build-vulkan/ggml/src/Release/ggml*.lib   "$OUT/ggml/src/Release/"
cp build-vulkan/ggml/src/Release/ggml*.dll   "$OUT/ggml/src/Release/"
cp build-vulkan/src/Release/crispembed.dll   "$OUT/src/Release/"
cp -r src/include/*  "$OUT/include/"
cp -r ggml/include/* "$OUT/ggml/include/"
tar -czf libcrispembed-windows-x86_64-vulkan.tar.gz -C "$OUT" .
```

Reference: `CrispASR/.github/workflows/release.yml:177-308`.

### 2. `CrispEmbed/crispembed-sys/build.rs`

`has_prebuilt()` (lines 20–25) currently probes only top-level + `Release/`.
Update it to look at the standardised subdirs:

```rust
fn has_prebuilt(dir: &Path) -> bool {
    let candidates = [
        dir.to_path_buf(),
        dir.join("Release"),
        dir.join("src"),
        dir.join("src/Release"),
    ];
    candidates.iter().any(|p|
        p.join("crispembed.lib").exists()
        || p.join("libcrispembed.so").exists()
        || p.join("libcrispembed.dylib").exists()
    )
}
```

Add `emit_runtime_rpath()` mirroring `crispasr-sys/build.rs:79-102` so
downstream apps don't need to chain `DEP_CRISPEMBED_LIB_DIR` rpath
flags themselves.

### 3. `CrispEmbed/crispembed-sys/Cargo.toml`

Currently has no `description` / `license` / `repository` / `include`,
so it's git-only. Add those (template: `CrispASR/crispasr-sys/Cargo.toml:1-22`)
and `cargo publish`. Then downstream consumers can use a versioned
crates.io dep instead of a path dep.

### 4. `CrispSorter/enable-crispembed.{ps1,sh}` — drop the warning

After the matrix lands, the "NOTE: requested -Backend cuda but the
upstream prebuilt is CPU-only" warning becomes obsolete. Delete that
block and update the asset name the script downloads:

- before: `crispembed-windows-x86_64.zip` (CLI, CPU)
- after, when `-Backend cuda`: `libcrispembed-windows-x86_64-cuda.tar.gz`
- after, when `-Backend vulkan`: `libcrispembed-windows-x86_64-vulkan.tar.gz`
- after, when `-Backend cpu`: `libcrispembed-windows-x86_64.tar.gz`

Update README's "Optional: CrispEmbed (GGUF) backend" section to
match.

## Acceptance criteria

A full release run of CrispEmbed produces at minimum these new assets
(in addition to the CLI zips that already work):

- `libcrispembed-windows-x86_64.tar.gz`
- `libcrispembed-windows-x86_64-vulkan.tar.gz`
- `libcrispembed-windows-x86_64-cuda.tar.gz`
- `libcrispembed-linux-x86_64.tar.gz`
- `libcrispembed-linux-x86_64-cuda.tar.gz`
- `libcrispembed-macos-arm64.tar.gz`

Smoke test downstream: in `CrispSorter`, point `CRISPEMBED_SYS_LIB_DIR`
at an extracted tarball and run

```powershell
.\enable-crispembed.ps1 -Backend vulkan -LibDir <extracted-dir>\src\Release
```

CrispSorter dev should boot with the GGUF backend live, and
`Settings → Search Index` should show "CrispEmbed (GGUF) — backend:
vulkan" in the engine hint.

## Out of scope for this session

- New CrispEmbed model variants in CrispSorter — already on
  CrispSorter's PLAN.md, requires both ONNX and GGUF support.
- Publishing the high-level `crispembed` Rust crate to crates.io —
  separate concern; needs API stabilisation first.
- CI for the Python wheel matrix.

## Helpful commands

```bash
# Inspect current CrispEmbed release assets
gh release view --repo CrispStrobe/CrispEmbed --json assets --jq '.assets[].name'

# Inspect current CrispASR matrix (the goal shape)
gh release view --repo CrispStrobe/CrispASR --json assets --jq '.assets[].name'

# Local build smoke test for the Vulkan variant before pushing
cd /c/Users/stc/Downloads/code/CrispEmbed
cmake -S . -B build-vulkan -DCRISPEMBED_BUILD_SHARED=ON -DGGML_VULKAN=ON -DCMAKE_BUILD_TYPE=Release
cmake --build build-vulkan --config Release
```
