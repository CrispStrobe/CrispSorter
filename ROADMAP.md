# Roadmap

## Done

- **LanceDB + Tantivy hybrid search** — persistent embedded library with dense ANN + BM25 full-text, RRF fusion
- **ONNX / CoreML backend** — run ONNX-format models via `ort` crate with CoreML execution provider for Apple Neural Engine acceleration
- **CrispEmbed GGUF backend** — feature-gated optional backend using libcrispembed for GGUF model inference (Metal/CUDA/Vulkan GPU acceleration)
- **Expanded model registry** — 24 ONNX model variants (BGE-M3, PIXIE-Rune, Snowflake Arctic, Jina v2/v3/v5, Qwen3-Embedding, Octen, MiniLM)
- **OrtPath backend** — handles ONNX models with external `.onnx_data` companion files and KV-cache decoder models
- **Cross-platform release workflow** — GitHub Actions builds for macOS ARM64/x86, Windows, Linux with llama-server sidecar
- **CrispEmbed CI integration** — sibling repo checkout + path rewrite so `cargo metadata` resolves on clean runners

## In Progress

- **Wire CrispEmbed sparse encoding into search pipeline** — BGE-M3/SPLADE sparse vectors via GGUF backend (C API ready, needs UI integration)
- **Add more GGUF-backed models to UI** — CrispEmbed supports 43+ models; CrispSorter currently exposes GGUF toggle for ~12 of them. Need ONNX `EmbedderModel` enum variants for the rest (all-MiniLM-L6-v2, bge-small/base/large, nomic-embed, mxbai, E5, etc.)
- **CrispEmbed reranking in search** — cross-encoder and bi-encoder reranking APIs are wired in `CrispEmbedBackend` but not yet used by the search pipeline

## Planned

1. **Custom output path template** — `{author}/{year} - {title}.{ext}` configurable in Settings
2. **BibTeX / Zotero export** — generate `.bib` / RIS from batch metadata
3. **Read PDF metadata** — pre-fill Title/Author/Year from XMP/DocInfo before LLM
4. **Folder Watcher** — auto-ingest new files from a watched directory
5. **Reranking pipeline stage** — after ANN+BM25 retrieval, rerank top-N with cross-encoder (CrispEmbed or ONNX reranker)
6. **Matryoshka dimension selection** — expose CrispEmbed `set_dim()` in Settings for smaller/faster embeddings
7. **Query/passage prefix selection** — auto-apply model-specific prefixes ("query: " / "search_query: ") for better retrieval
8. **PWA demo** — generate `.sh`/`.bat` sorting scripts or browser-based sorting via File System Access API

---

# Learnings

## CrispEmbed integration architecture

CrispSorter integrates CrispEmbed as an **optional Cargo path dependency**
feature-gated behind `crispembed` (plus `crispembed-vulkan`, `crispembed-metal`,
`crispembed-cuda`). The dependency points at `../../CrispEmbed/crispembed`
for local dev — a sibling-of-parent layout.

In CI/release, both repos live inside `$GITHUB_WORKSPACE`. A Python regex
rewrites the path dep from `../../CrispEmbed/crispembed` to
`../_sibling/CrispEmbed/crispembed` at build time. This is fragile — if the
path format changes in Cargo.toml, the regex silently fails and cargo metadata
breaks. Always verify the rewrite output in CI logs.

## Rust enum match arms and non-existent variants

**Critical pitfall**: In a `match` on a Rust enum, if you write a variant name
that doesn't exist in the enum, Rust treats it as a **variable binding** (catches
everything), not a compile error for a missing variant. Combined with `|` (or)
patterns, this produces E0408 "variable not bound in all patterns" — confusing
unless you know the root cause.

Example that fails:
```rust
fn gguf_registry_name(&self) -> Option<&'static str> {
    use EmbedderModel::*;
    Some(match self {
        // BgeSmallEnV15 doesn't exist in the enum — Rust binds it as a variable!
        BgeSmallEnV15 | BgeSmallEnV15Q => "bge-small-en-v1.5",
        _ => return None,
    })
}
```

**Rule**: Never add match arms for model variants that don't exist in the enum
yet. The wildcard `_ => return None` handles them. Add enum variants first,
then add match arms.

## GGUF_CAPABLE_MODELS must mirror EmbedderModel enum

The `GGUF_CAPABLE_MODELS` set in `Settings.svelte` gates the ONNX/GGUF backend
toggle in the UI. Entries in this set that don't correspond to actual `<option>`
values in the model dropdown are dead weight — they'll never match. Keep the set
in sync with models that have both:
1. An `EmbedderModel` enum variant (Rust side)
2. An `<option>` in the Settings dropdown (Svelte side)

## Release workflow: CrispEmbed checkout is load-bearing

The `generate-licenses.js` prebuild hook runs `cargo-license --json`, which
calls `cargo metadata`. This resolves ALL path dependencies in Cargo.toml —
including the optional CrispEmbed dep. Without the sibling checkout +
path rewrite, the build fails at the **npm prebuild** step, not at Rust
compilation. The error message ("failed to resolve patches") doesn't obviously
point to CrispEmbed.

## llama-server sidecar binary naming

Tauri sidecar binaries must be named `{name}-{target_triple}[.exe]` and placed
in `src-tauri/bin/`. The release workflow downloads pre-built llama-server
binaries from the ggml-org/llama.cpp releases and renames them to match. On
macOS/Linux, shared libraries (`.dylib`/`.so`) from the tarball are also copied
to `src-tauri/bin/` so they're bundled alongside the sidecar.

## OrtPath vs Fastembed backend selection

Two conditions force the OrtPath backend (bypassing fastembed's `UserDefined`):
1. **External ONNX data** — `.onnx_data` companion files that ORT must resolve
   by relative path (loading from bytes breaks this)
2. **No config.json** — fastembed's `UserDefinedEmbeddingModel` requires a
   config.json; repos like Octen don't have one

Models with KV-cache (Qwen3-Embedding) also use OrtPath because they need
custom input tensor construction (empty `past_key_values` tensors).

## Decoder model pooling strategies

Different decoder-based embedding models use different pooling:
- **Qwen3-Embedding (KV-cache ONNX)**: last-token pooling on the EOS position.
  Empty KV-cache tensors `[batch, kv_heads, 0, head_dim]` are passed — ndarray
  supports zero-sized dims but ort's raw-data path does not, so use
  `ndarray::Array4::zeros()`.
- **Octen-0.6B (no KV-cache ONNX)**: also last-token pooling, but the ONNX
  export has no `past_key_values` inputs — set `force_last_token_pool()`.
- **electroglyph uint8 export**: pre-pooled uint8 output requires dequantization:
  `f32 = (u8 - zero_point) * scale`.

## Adding a new model to CrispSorter

1. Add variant to `EmbedderModel` enum in `embedder.rs`
2. Add `display_name()`, `dims()`, `max_tokens()` match arms
3. Add `to_model_spec()` with HF repo + ONNX filename + any special config
4. If GGUF equivalent exists: add match arm to `gguf_registry_name()`
5. Add `<option value="...">` to the model dropdown in `Settings.svelte`
6. If GGUF-capable: add the option value to `GGUF_CAPABLE_MODELS` set
7. Add i18n label in `src/lib/i18n/en.ts` and `de.ts`

## AppImage bundling on GitHub Actions (ubuntu-24.04)

`linuxdeploy` and its plugins (`linuxdeploy-plugin-appimage`,
`linuxdeploy-plugin-gtk`) are themselves AppImages. They need FUSE to
self-extract. GitHub Actions ubuntu-24.04 runners don't have FUSE, so the
AppImage bundling step fails with the unhelpful error `failed to run linuxdeploy`.

**Fix**: Set `APPIMAGE_EXTRACT_AND_RUN=1` as an environment variable in the
workflow. This tells AppImage tools to extract to a temp dir instead of using
FUSE. Also useful: `NO_STRIP=true` to prevent strip from failing on unusual
binaries.

The .deb bundle is unaffected — it doesn't use linuxdeploy.

## Snowflake Arctic model variants

Snowflake Arctic Embed L v2.0 has 7 ONNX variants on HuggingFace, all in the
same repo under `onnx/`:
- `model_quantized.onnx` (INT8, default — smallest practical)
- `model_int8.onnx` (INT8 via different quantization)
- `model_fp16.onnx`, `model_q4.onnx`, `model_q4f16.onnx`, `model_O4.onnx`
- `model.onnx` + `model.onnx_data` (FP32 reference, ~1.7 GB, needs OrtPath)

Only the FP32 variant has external data — the quantized ones are self-contained
and can use the fastembed UserDefined path.
