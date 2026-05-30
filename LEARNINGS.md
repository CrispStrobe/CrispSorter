# CrispSorter — Learnings & Key Insights

Critical things we've learned that are easy to forget when returning to this codebase.

---

## Build & CI

### `notify` event handlers run on a non-tokio thread

`notify::recommended_watcher`'s callback runs on a notify-internal thread,
not a tokio worker. That means: no `await`, no tokio-mutex `lock().await`
directly. Two clean patterns:

1. Pass an `Arc<Mutex<…>>` into the closure and `tokio::spawn` an async
   block from inside the callback — the spawn returns immediately, the
   async work runs on the runtime, and emit-to-frontend stays well-formed.
2. Use a sync `std::sync::Mutex` instead of `tokio::sync::Mutex` for
   small bookkeeping inside the callback.

Pattern (1) is what `watcher::handle_event` uses for the dedup map +
`AppHandle::emit` call. Pattern (2) would be slightly faster but mixes
sync/async primitives in a way that's harder to reason about — every
async caller would need to use `blocking_lock` to coordinate with the
sync callback.

### `onMount` cleanup with async work needs a sync wrapper

Svelte's `onMount` accepts a sync function returning either nothing or
a sync cleanup. An `async () => …` returning a cleanup typechecks as
`Promise<() => void>`, which is *not* the same as the expected
`() => void`. Pattern that works in Svelte 5 + TypeScript:

```ts
onMount(() => {
    let cleanup = () => {};
    (async () => {
        // ... await work ...
        cleanup = () => unlisten();
    })();
    return () => cleanup();
});
```

The IIFE runs as a side-effect; the cleanup closure is captured by
reference and gets the real implementation once the async setup
finishes. If `onMount`'s return runs *before* the IIFE assigns
`cleanup`, the no-op default fires, which is harmless.

### Native TTS over stdin avoids argv-quoting nightmares

`say` (macOS), `espeak`/`spd-say` (Linux), and PowerShell SAPI (Windows)
all read text from stdin when invoked correctly. Piping the text instead
of argv-passing dodges the quoting fan-out that arbitrary chat content
would otherwise require — code blocks, smart quotes, embedded newlines,
single + double quotes side by side, etc. The Windows path uses a
PowerShell one-liner that calls `[Console]::OpenStandardInput().ReadToEnd()`
and feeds the result into the SAPI synthesizer; never embed user text in
the script string itself or you reintroduce the quoting problem.

`AppState.tts_process` holds the spawned `tokio::process::Child`. Both
`tts_speak` (when called while another utterance is in flight) and
`tts_stop` call `Child::kill().await` then `wait().await` to ensure the
synth process is reaped before returning — otherwise rapid speak/stop
cycles leave zombies. The frontend doesn't track utterance lifetime
precisely (the synth runs detached); `ttsSpeaking` flips back to
`false` on a 500ms timer after the spawn returns, which is good enough
to keep the Mute button visible during plausible speech without holding
it forever after a 1-word utterance finishes.

### Two sibling path deps now: CrispEmbed AND CrispASR

The `crispembed` and `crispasr` optional path-deps both live as siblings
of the CrispSorter checkout (`../../CrispEmbed/crispembed` and
`../../CrispASR/crispasr` from `src-tauri/`). Cargo metadata resolves
both at every build, so a missing checkout breaks even default-feature
builds. `release.yml` now checks out both via the `actions/checkout`
sibling pattern under `_sibling/CrispEmbed` and `_sibling/CrispASR`,
and a single Python rewrite step retargets both path deps to the
`_sibling/...` layout. Adding a third sibling dep in the future means
extending that rewrite — keep the regex anchors tight (`\.\./\.\./X/y`)
so unrelated `path = "..."` strings in Cargo.toml aren't accidentally
rewritten.

### `crispembed` optional path dep still needs to resolve
`src-tauri/Cargo.toml` has `crispembed` as an optional dep at path `../../CrispEmbed/crispembed`.
Cargo resolves ALL path deps (even optional ones) during `cargo metadata`, so if the sibling repo
doesn't have the Rust crate, the build fails even when the `crispembed` feature is not enabled.

**Local dev fix:** A minimal stub crate lives at `/Users/<user>/code/CrispEmbed/crispembed/`.
**CI fix:** The release workflow checks out `CrispStrobe/CrispEmbed` and rewrites the Cargo.toml path.

In CI/release, both repos live inside `$GITHUB_WORKSPACE`. A Python regex rewrites the path dep
from `../../CrispEmbed/crispembed` to `../_sibling/CrispEmbed/crispembed` at build time. This is
**fragile** — if the path format changes in Cargo.toml, the regex silently fails and `cargo metadata`
breaks. Always verify the rewrite output in CI logs.

### Release workflow: CrispEmbed checkout is load-bearing for `npm prebuild`
The `generate-licenses.js` prebuild hook runs `cargo-license --json`, which calls `cargo metadata`.
That resolves ALL path dependencies in Cargo.toml — including the optional `crispembed` dep.
Without the sibling checkout + path rewrite, the build fails at the **npm prebuild** step, not at
Rust compilation. The error message ("failed to resolve patches") doesn't obviously point to
CrispEmbed.

### llama-server sidecar binary naming
Tauri sidecar binaries must be named `{name}-{target_triple}[.exe]` and placed in `src-tauri/bin/`.
The release workflow downloads pre-built llama-server binaries from the ggml-org/llama.cpp
releases and renames them to match. On macOS/Linux, shared libraries (`.dylib`/`.so`) from the
tarball are also copied to `src-tauri/bin/` so they're bundled alongside the sidecar.

### serde `rename_all = "kebab-case"` and digit boundaries — pin with a test
Heck (the kebab-case algorithm serde uses) splits between letter↔digit, but the
exact split is not always intuitive. Examples produced by serde for our enum:
- `BgeSmallEnV15` → `bge-small-en-v15`  *(no dash before `15`; `V` and `15` stay together)*
- `AllMiniLmL6V2` → `all-mini-lm-l6-v2`  *(splits `Mini`+`Lm`, `L6`+`V2`)*
- `EmbeddingGemma300M` → `embedding-gemma300-m`  *(`Gemma300` is one token; trailing `M` splits off)*
- `MxbaiEmbedLargeV1` → `mxbai-embed-large-v1`

The frontend `Settings.svelte::indexEmbedderToRust` hand-writes these strings.
A mismatch silently falls back to `bge-m3` (the `?? 'bge-m3'` default). Always
add new variants to the `embedder_model_serde_strings` test in `embedder.rs`
to lock the wire format the frontend has to match.

### `src-tauri/target` is a symlink to `<external-volume>`
The Cargo build dir is symlinked to an external volume. If that volume is not mounted, `cargo` fails
with "Not a directory". Fix: `rm src-tauri/target && mkdir src-tauri/target`.

`scripts/build.sh` re-establishes the symlink automatically on every run when the volume is mounted
and the symlink is missing or pointing somewhere else. It also falls through to a local
`src-tauri/target/` when the volume isn't mounted (so disconnected-laptop builds Just Work, just
without the disk-saving aspect). Override the destination via the
`CRISPSORTER_TARGET_VOLUME` env var, or set it to `""` to skip the dance entirely.

### Debug binary launch (`cargo run`) needs vite running — white screen otherwise
Tauri 2 reads `devUrl: http://localhost:1420` from `tauri.conf.json` for **debug** builds and
loads the webview from there. Release builds use `frontendDist: "../build"` instead. So:

* **`npm run tauri dev`** is the right dev command — it spawns vite on :1420 and cargo together.
* **`cargo run` alone** points at `:1420` and gets nothing → white screen if vite isn't up.
* **`cargo build --release && ./target/release/tauri-app`** uses the static `build/` and runs
  standalone. Useful for testing the bundled-frontend path without the full `cargo tauri build` cycle.

`scripts/build.sh --release` does the static-frontend + release-binary combo in one shot.

### macOS 13 (Intel) GitHub runner is chronically slow to provision
macOS 13 runners are often queued for 1+ hours on GitHub-hosted Actions. The release workflow now
has a separate `publish` job with `if: always()` that publishes the draft as soon as all *other*
matrix jobs finish — macOS 13 can catch up later or be skipped without blocking the release.

### AppImage requires `APPIMAGE_EXTRACT_AND_RUN=1` on GitHub runners
Linux AppImage tools (`linuxdeploy`, `linuxdeploy-plugin-appimage`, `linuxdeploy-plugin-gtk`) are
themselves AppImages. GitHub Actions ubuntu-24.04 runners have no FUSE, so the AppImage bundling
step fails with the unhelpful error `failed to run linuxdeploy`. Set `APPIMAGE_EXTRACT_AND_RUN=1`
in the workflow to make AppImage tools extract to a temp dir instead of using FUSE. Also useful:
`NO_STRIP=true` to prevent `strip` from failing on unusual binaries. The `.deb` bundle is
unaffected — it doesn't use linuxdeploy.

### Bundling native wrappers (libcrispasr / libcrispembed) into the .app

Five-step recipe that finally got `cargo tauri build --features
crispasr-metal` to produce a self-contained `.app` (PLAN P3.5 phase 1):

1. **Sibling-repo source-of-truth.** `crispasr-sys` (and `crispembed-sys`)
   are direct optional cargo deps with `links = "crispasr"`. Their
   `build.rs` runs cmake on the parent repo (or finds a pre-built tree
   under `../<repo>/build*` — `build-flutter-bundle` matches what
   CrisperWeaver already produces) and emits `cargo:LIB_DIR=<absolute
   path>` so consuming crates' `build.rs` see `DEP_<NAME>_LIB_DIR`.
2. **Direct-dep is mandatory.** Cargo only forwards `links` metadata to
   *immediate* dependents — a transitive path drops the `cargo:LIB_DIR=`
   silently. So `crispasr-sys` has to be in `[dependencies.crispasr-sys]`
   alongside the safe `crispasr` wrapper, even though we don't import
   it from Rust.
3. **rpath has to be emitted from the consumer's build.rs.**
   `cargo:rustc-link-arg=-Wl,-rpath,…` from a transitive lib's build
   script is silently dropped. Read `DEP_CRISPASR_LIB_DIR` in the
   consumer's `build.rs` and emit four entries on macOS: the absolute
   build dir + `<build>/ggml/src` (so `cargo run` works out of the
   workspace), plus `@executable_path/../Frameworks` + `@loader_path/
   ../Frameworks` (so the bundled `.app` resolves them post-bundling).
4. **The .app must be patched after `tauri build` finishes.** Tauri 2
   has no hook between "create .app" and "create .dmg", so post-process:
   copy `lib*.dylib` from `<build>/src/` and recursively from `<build>/
   ggml/src/` (per-backend subdirs `ggml-metal/`, `ggml-blas/`, …) into
   `Contents/Frameworks/` with the SOVERSION-1 symlink alias
   (`libcrispasr.1.dylib → libcrispasr.dylib`); recursively bundle every
   `/opt/homebrew/...` / `/usr/local/...` transitive (espeak-ng pulls
   pcaudiolib on kokoro builds) with their `LC_ID_DYLIB` rewritten to
   `@rpath/<basename>`; **delete every absolute `LC_RPATH` entry from
   libcrispasr** (cmake bakes `/opt/homebrew/Cellar/espeak-ng/...` and
   the build-tree path in, both leak the dev machine and crowd out the
   bundled `@loader_path` lookup) and add `@loader_path/.` as the only
   rpath; ad-hoc codesign; finally `hdiutil create` a fresh .dmg from
   the patched .app and `gh release upload --clobber` the new .dmg
   *and* `.app.tar.gz` over what `tauri-action` already pushed.
5. **Verify with a clean-machine simulation.** `mv build-flutter-bundle
   build-flutter-bundle.HIDDEN; DYLD_PRINT_LIBRARIES=1 …app/Contents/
   MacOS/<bin>` should show *every* dylib loading from `…app/Contents/
   Frameworks/…`. If you see `/opt/homebrew/...` paths in the trace,
   you missed an `LC_RPATH` cleanup or a transitive walk.

The `links` metadata channel is the keystone — without it you have no
clean way to discover the cmake build dir at the consumer's build time,
and `cargo:rustc-link-arg` from the wrong place is a silent no-op. The
debugging path goes: `cargo build -vv | grep "tauri_app .*--crate-type
bin"` → look for `-C link-arg=-Wl,-rpath,...` in the final rustc
invocation, then `otool -l <bin> | grep -A2 LC_RPATH` on the result.

### CrispASR ships per-platform `libcrispasr-{arch}.tar.gz` bundles

Mirroring `llama.cpp`'s release-asset layout: CrispASR's `release.yml`
publishes a tarball per (platform, GGML-backend) combo containing
`libcrispasr.{dylib,so,dll}` + `libggml*.{dylib,so,dll}` + headers under
the same directory shape that `crispasr-sys`'s `try_existing_build`
walks (`<bundle>/src/lib*` + `<bundle>/ggml/src/libggml*`). CrispSorter's
release.yml downloads + extracts the matching tarball into
`_sibling/CrispASR/build-flutter-bundle/` and `crispasr-sys`'s build
script then bypasses its cmake fallback. Saves ~10 min per CrispSorter
release per platform vs. building from source in our own CI; reusable
for CrisperWeaver / Python wrapper / future Node-Go bindings (one
upstream lib build feeds many consumer apps).

---

## Frontend (Svelte 5 + Tauri)

### Svelte 5 rune mode: `$state` inside class fields
`BatchManager` uses `$state<BatchItem[]>([])` as class fields.  This works, but mutations must
happen through direct assignment (`this.items = [...]`) or array mutations (`this.items.push(...)`)
— reactive updates propagate correctly in both cases in Svelte 5.

### `@tauri-apps/plugin-http` fetch supports `AbortSignal`
The Tauri HTTP plugin's `fetch(input, init)` extends `RequestInit`, so `signal: AbortController.signal`
works as expected.  This is NOT documented prominently — it just falls through to the underlying
`reqwest` cancel token.

### `invoke()` calls cannot be cancelled mid-execution
Tauri's `invoke()` is a one-shot IPC call.  Once the Rust handler starts, there is no way to cancel
it from JS.  Use `Promise.race([invokePromise, timeoutPromise, abortPromise])` to make the **JS side**
responsive while the Rust handler runs to completion in the background.

### Stop button only works between async boundaries
`BatchManager.stopRequested` is checked at the top of each loop iteration and after each `await`.
If a single `await` takes a long time (LLM query, Rust extraction), the stop button has no effect
until that `await` resolves.  The fix is to pass `AbortSignal` into the awaited call so it can
terminate early.

### Svelte store subscriptions in rune-mode components
In a `$state`-based (rune-mode) component, subscribe to a Svelte writable store using `$effect`:
```svelte
$effect(() => {
    const unsub = myStore.subscribe(v => { localState = v; });
    return unsub; // cleanup
});
```
Or use `import { get } from 'svelte/store'` for one-shot reads.

---

## Search / FTS

### Model cache directory is configurable, shared across all download paths

Five different code paths download model weights into the same dir:
fastembed native (ONNX), fastembed UserDefined (ONNX), OrtPath (external
ONNX data), CrispEmbed GGUF embedder, CrispEmbed GGUF reranker. The single
`resolve_model_cache_dir(config, data_dir)` helper picks the path in this
order:

1. `CRISPSORTER_MODEL_CACHE_DIR` env var (machine-wide override; useful
   for CI runners and shared multi-user installs).
2. `IndexConfig.model_cache_dir` (Settings UI override; persisted via
   the standard `saveSetting` flow as `indexModelCacheDir`).
3. `{data_dir}/models/` (default).

The directory is created eagerly on resolve so hf-hub / fastembed don't
fail on the very first download. Pointing this at an external volume
(e.g. `<external-volume>/ai/crispsorter-models`) lets the cache survive app
re-installs and be shared with `CrispEmbed` CLI when both use the same
hf-hub layout (`models--<repo>--<sha>/...`). Note: fastembed-rs's own
`fastembed_cache/` layout is *not* the same as hf-hub's, so the cache is
only fully shareable with hf-hub-using tools — fastembed will re-download
into its own subtree under the chosen dir.

**The default eats your boot drive on macOS.** `<data_dir>/models/`
resolves to `~/Library/Application Support/com.<user>.crispsorter/models/`
which lives on the boot volume. Five embedder model variants run ~5-10 GB;
add a few rerankers + ASR models and you're routinely over 15 GB. Users
with tight boot drives should set the cache dir to an external volume on
first run via Settings → Search Index → "Model cache directory". The
canonical user HF cache (`~/.cache/huggingface/hub/` on Linux,
`~/Library/Caches/huggingface/hub/` on macOS — though many users override
to a backup drive via `HF_HOME`) is also a reasonable target if you want
free reuse with Python `transformers` / `huggingface_hub` / CLI tools.

**Symlink trick when downloads already happened in the wrong dir.** The
HF hub layout is content-addressed (`snapshots/<sha>/<file>` referencing
`blobs/<hash>` via hardlink), so an `rsync -a --ignore-existing src/ dst/
&& rm -rf src && ln -s dst src` on each `models--*` subdir is safe — the
rsync is a no-op when both sides have the same blobs (the common case
when both came from the same `hf_hub` download flow), and a merge when
one side is missing blobs (e.g. fresh CrispSorter cache vs years-old
shared user HF cache). Today's session reclaimed ~6.9 GB by symlinking 7
recently-downloaded model dirs to the canonical
`<external-volume>/ai/huggingface-hub/` mirror.

### Matryoshka truncation is GGUF-only and pinned by index schema

`CrispEmbed::set_dim(N)` truncates the model output vector to N dims;
`fastembed-rs` exposes no equivalent hook. So `EmbedderConfig.matryoshka_dim`
is honored only when the active backend is `EmbedderBackend::Gguf` — ONNX
paths silently fall back to the model's nominal dim regardless of the
config. The Settings UI gate (`indexEmbedderBackend === 'gguf'`) keeps
the field invisible elsewhere so users don't set it expecting an ONNX
effect.

The LanceDB `embedding` column is `FixedSizeList<Float32>[N]` — a single
fixed width per index. `EmbedderConfig::effective_dim()` is the source of
truth: it clamps `matryoshka_dim` to `model.dims()` (so a too-large value
is silently corrected) and treats `Some(0)` as `None` (model default,
common UI sentinel). `tauri_commands::init_index` reads it once and
passes to both `Embedder::new` *and* `LocalIndex::open_or_create` so the
two stay in lockstep.

Changing `matryoshka_dim` on an existing index is a schema-incompatible
migration: LanceDB cannot resize a `FixedSizeList` column in place. The
UI hint warns about re-ingestion; the runtime would fail at write time
with a column-width mismatch otherwise. Quality also depends on the
model — only MRL-trained models (BGE-M3, Snowflake Arctic L v2,
PIXIE-Rune) preserve relative similarity at smaller dims; non-MRL models
will silently produce poorer embeddings.

### Sparse retrieval is a 3rd RRF channel, not a primary modality

LanceDB has no native sparse-vector ANN, and an inverted index over
`embedding_sparse` would duplicate work Tantivy already does for term
matching. Instead `LocalIndex::search_sparse_in_pool` scores the union of
FTS+ANN candidates by sparse dot product against the query's sparse vector,
and `SearchEngine::search_hybrid` fuses the result as a third RRF channel
via the generalized `rrf_merge_n`. Trade-offs:

- **No corpus-wide sparse retrieval** — chunks that didn't show up in
  either FTS or dense ANN can't be promoted by sparse alone. For an
  academic-doc corpus where dense and sparse usually agree on candidate
  selection, this is a feature: it stops sparse from amplifying lexical
  noise. For a true SPLADE-as-primary-modality use case, a separate
  inverted index is needed.
- **Two-pointer merge when both inputs are sorted** (the common case for
  BGE-M3 / SPLADE outputs) — O(|a|+|b|), zero allocation. Hash-join
  fallback for unsorted inputs. `is_sorted_ascending` is the gate.
- **Sparse ingestion path was already wired** before this — every chunk
  has an `embedding_sparse` JSON column when the active embedder has a
  sparse head. The new query-side retrieval just *reads* that column.

### Cross-encoder reranking: lazy-load handle + NaN-safe fallback
Rerankers double the search-side memory (separate `crispembed::CrispEmbed`
instance with its own GGUF) and add ~100–500ms of first-query latency for
the model load + download. Two patterns in `index/reranker.rs` keep this
manageable:

1. **`RerankerHandle`** is the only thing `SearchEngine` carries — a
   `Clone`-able struct with `(model, cache_dir, Arc<Mutex<Option<Reranker>>>)`.
   First `score_batch` call performs the load (HF download via `hf-hub`,
   GGUF open via `crispembed::CrispEmbed::new`, `is_reranker()` sanity
   check); subsequent calls reuse the cached `Reranker`. Users who never
   issue a query don't pay the load cost.
2. **NaN-safe fallback**: `RerankerHandle::score_batch` returns
   `vec![f32::NAN; docs.len()]` on load failure, and `Reranker::score`
   returns `f32::NAN` if the underlying CrispEmbed call fails. The
   `SearchEngine::maybe_rerank` sort treats NaN entries as "stay in original
   RRF order at the back of the list" — a misconfigured reranker degrades
   to plain RRF ranking instead of erroring the entire query.

The cross-encoder scores are *raw logits* (not probabilities), so absolute
score values aren't comparable across reranker models. Only the relative
ordering matters — don't expose the score in UI as a confidence percentage.

Bi-encoder reranking (`CrispEmbed::rerank_biencoder`) is intentionally not
wired in: it's just better dense retrieval over the candidate set, which the
existing dense ANN already does. Cross-encoders are the only path that adds
real signal beyond what we already have.

### `rerank_top_n` interacts with the inner `*2` RRF over-fetch
`search_hybrid` already over-fetches by 2x on each side (`limit*2`) so RRF
has slack. When reranking, the over-fetch is `inner_limit*2` where
`inner_limit = max(rerank_top_n, limit)`. With default `top_n=50`, that's
100 candidates per side hitting Tantivy/Lance — fine for the typical
academic corpus, but worth profiling before raising `top_n` over 100.

### Tantivy ASCII-folding must happen on both sides
Query-side folding alone (via `fts_query::fold_accents`) is not enough — the
indexed terms also have to be folded, otherwise `München` (indexed as
`münchen`, since the `default` tokenizer only lowercases) never matches a
folded query `munchen`. The fix is a custom tokenizer
`ascii_folding = SimpleTokenizer + RemoveLong(40) + LowerCaser + AsciiFoldingFilter`
registered on the index in `register_tokenizers()` and used by the schema for
`title`, `headings`, `body`. The query-side `fold_accents` (`deunicode +
to_lowercase`) covers substitutions Tantivy's `AsciiFoldingFilter` does not
(e.g. `ø` → `o`), so keep both.

**Breaking change for existing FTS dirs**: indexes written before this fix
have `münchen`-flavored terms that won't match queries passing through the
new tokenizer. Re-ingestion is required — there is no in-place migration
because Tantivy stores tokens, not raw text. Document this when bumping the
release that ships the fix.

---

## Embedder / Model registry

### Rust enum match arms and non-existent variants

In a `match` on a Rust enum, if you write a variant name that doesn't exist in the enum, Rust treats it as a **variable binding** (catches everything), not a compile error for a missing variant. Combined with `|` (or) patterns this produces E0408 "variable not bound in all patterns" — confusing unless you know the root cause.

Example that fails:
```rust
fn gguf_registry_name(&self) -> Option<&'static str> {
    use EmbedderModel::*;
    Some(match self {
        // BgeSmallEnV15 doesn't exist in the enum yet — Rust binds it as a variable!
        BgeSmallEnV15 | BgeSmallEnV15Q => "bge-small-en-v1.5",
        _ => return None,
    })
}
```

**Rule**: Never add match arms for model variants that don't exist in the enum yet. The wildcard `_ => return None` handles them. Add enum variants first, then add match arms.

### `GGUF_CAPABLE_MODELS` must mirror the `EmbedderModel` enum

The `GGUF_CAPABLE_MODELS` set in `Settings.svelte` gates the ONNX/GGUF backend toggle in the UI. Entries that don't correspond to actual `<option>` values in the model dropdown are dead weight — they'll never match. Keep the set in sync with models that have both:
1. An `EmbedderModel` enum variant (Rust side)
2. An `<option>` in the Settings dropdown (Svelte side)

### OrtPath vs Fastembed backend selection

Two conditions force the OrtPath backend (bypassing fastembed's `UserDefined`):
1. **External ONNX data** — `.onnx_data` companion files that ORT must resolve by relative path (loading from bytes breaks this).
2. **No config.json** — fastembed's `UserDefinedEmbeddingModel` requires a `config.json`; repos like Octen don't have one.

Models with KV-cache (Qwen3-Embedding) also use OrtPath because they need custom input tensor construction (empty `past_key_values` tensors).

### Decoder model pooling strategies

Different decoder-based embedding models use different pooling:
- **Qwen3-Embedding (KV-cache ONNX)**: last-token pooling on the EOS position. Empty KV-cache tensors `[batch, kv_heads, 0, head_dim]` are passed — ndarray supports zero-sized dims but ort's raw-data path does not, so use `ndarray::Array4::zeros()`.
- **Octen-0.6B (no KV-cache ONNX)**: also last-token pooling, but the ONNX export has no `past_key_values` inputs — set `force_last_token_pool()`.
- **electroglyph uint8 export**: pre-pooled uint8 output requires dequantization: `f32 = (u8 - zero_point) * scale`.

### Snowflake Arctic L v2.0 ONNX variants

Same `Snowflake/snowflake-arctic-embed-l-v2.0` HF repo, all under `onnx/`:
- `model_quantized.onnx` (INT8, default — smallest practical)
- `model_int8.onnx` (INT8 via different quantization)
- `model_fp16.onnx`, `model_q4.onnx`, `model_q4f16.onnx`, `model_O4.onnx`
- `model.onnx` + `model.onnx_data` (FP32 reference, ~1.7 GB, needs OrtPath)

Only the FP32 variant has external data — the quantized ones are self-contained and can use the fastembed UserDefined path.

### Asymmetric retrieval prefixes are model-specific and silent on mistakes

Different model families train with different (or no) prefixes. The mapping
lives in `EmbedderModel::prefix(EmbedRole)`:

| Model family | Query prefix | Passage prefix |
|---|---|---|
| E5 (multilingual + English) | `query: ` | `passage: ` |
| Nomic v1.5 | `search_query: ` | `search_document: ` |
| BGE en-v1.5 + Mxbai | `Represent this sentence for searching relevant passages: ` | (none) |
| Jina v5 (Small + Nano) | `Query: ` | `Document: ` |
| EmbeddingGemma 300M | `task: search result \| query: ` | `title: none \| text: ` |
| BGE-M3, Qwen3, Octen, PIXIE-Rune, Snowflake Arctic-L v2, GTE en-v1.5, Jina v2/v3, MiniLM, BERT bases | (none) | (none) |

A wrong prefix degrades retrieval quality silently — vectors stay valid but
score against the wrong manifold. When in doubt, default to no prefix; it's
a smaller hit than picking the wrong one. The `prefix_table` test in
`embedder.rs` pins the mapping so changes are explicit.

The CrispEmbed (GGUF) backend has a native `set_prefix` that applies the
prefix inside tokenization (no max-token competition with chunk text). The
fastembed and OrtPath backends prepend in Rust — so a long prefix (BGE's is
~14 tokens) does eat into the chunker's `max_tokens` budget; truncation
silently drops the chunk tail. Acceptable for typical 256–512-token chunks
but worth re-checking for very small windows.

Sparse models (BGE-M3, SPLADE++) are trained without prefixes — `embed_sparse`
passes texts through unprefixed.

### Adding a new model to CrispSorter — checklist

1. Add variant to `EmbedderModel` enum in `embedder.rs`.
2. Add `display_name()`, `dims()`, `max_tokens()` match arms.
3. Add `to_model_spec()` with HF repo + ONNX filename + any special config — or return `None` if it's a native fastembed model.
4. Update `is_native()` (native fastembed) and `to_fastembed_dense()` / `to_fastembed_sparse()` mappings as applicable.
5. If GGUF equivalent exists: add match arm to `gguf_registry_name()`. The decoder vs encoder branch in `to_gguf_spec()` already picks the right `<name>-q8_0.gguf` vs `<name>.gguf` filename.
6. Add the new variant to the `embedder_model_serde_strings` test so the kebab-case wire format is locked.
7. Add `<option value="...">` to the model dropdown in `Settings.svelte`. If GGUF-capable, add the option value to `GGUF_CAPABLE_MODELS`. Add the entry to `indexEmbedderToRust` matching the kebab-case from the test.
8. Add i18n labels (`model_*`) in `src/lib/i18n.svelte.ts` for both `en` and `de` blocks.

---

## Processing Pipeline

### Session resume leaves items in mid-flight statuses
When the app closes mid-batch and resumes, items can be stuck in `extracting` or `analyzing`.
These statuses mean "actively running" to the UI, which is wrong after a restart.
**Pattern:** Always sanitize loaded session items: reset `extracting`/`analyzing` to a safe
"interrupted" status in `resumeLastSession()`.

### Extract-then-analyze is the right two-phase approach
Running extraction and LLM analysis interleaved (per item) means a rate-limited LLM also blocks
all extraction. Better: extract ALL items in pass 1, then analyze ALL in pass 2. Both passes
respect `stopRequested` and can be run independently.

### Per-page watchdog is better than flat timeout for extraction
A 5-minute flat timeout is too generous for small files and too tight for large scanned PDFs.
The right signal is "no page progress in N seconds" — wire the `onProgress` callback to a
watchdog timer that resets on every page event and fires after 30 s of silence.

---

## Rate Limits

### Remote provider 429s can cascade
Rate limits on Groq, OpenRouter, etc. cause the retry loop to eat into `MAX_RL_RETRIES` (currently 6)
before giving up.  During a large batch, this can mean many minutes of dead time.
**Better:** Detect 429 early and switch to a fallback provider (round-robin) rather than retrying
the same one.  Reset the provider index at the start of each `processAll`.

---

## Release

### Version must be bumped in three places
- `package.json` → `"version"`
- `src-tauri/Cargo.toml` → `version = "..."`
- `src-tauri/tauri.conf.json` → `"version"`

All three must match or the Tauri build will fail / produce inconsistent binaries.

### `releaseDraft: true` + separate publish job is the right pattern
`tauri-apps/tauri-action` uploads artifacts to the draft release as each platform job completes.
A separate `publish` job with `needs: [release]` and `if: always()` converts the draft to live
once all matrix jobs have settled — regardless of individual failures.

## Catalog (.caf)

### `.caf` is per-file metadata + dir tree + volume header — not just filenames

Easy to under-describe `.caf` because the user-visible payoff is "filename
search across drives." But the on-disk format (Cathy 1.x → Catfish v8)
carries materially more, and our reader parses every field. What we *use*
is a different question.

What `.caf` actually contains (per `src-tauri/src/catalog/caf.rs::read_file`):

* **Volume header** — device path, volume label, alias, serial number,
  comment (v ≥ 4), free space (v ≥ 1), archive flag (v ≥ 6), creation date.
* **Directory tree** — per-directory aggregates `(file_count, total_size)`
  for v ≥ 3, names, parent pointers.
* **Per-file ELM entries** — `(mtime, size, parent_id, name)` with version-
  dependent struct widths:
  * v ≤ 6: size as `<i32>`, parent_id as `<u16>`
  * v = 7:  size as `<i64>`, parent_id as `<u16>`
  * v = 8:  size as `<i64>`, parent_id as `<u32>` (current Catfish writes this)
* `size < 0` encodes a directory: dir ID = `-size` (v > 6) or 1-based
  positional index (v ≤ 6 quirk).
* **Hashes are NOT stored** — Cathy/Catfish design choice; we recompute
  on demand for dedup.

What we *keep* (per `catalog/index.rs::FileEntry` + `catalog/lance.rs::build_schema`):

| Field | Parsed | Surfaced to `FileIndex` | Lance `catalog_entries` row |
|---|---|---|---|
| path (reconstructed from dir tree) | ✓ | ✓ | ✓ as `entry_path` |
| size (per-file bytes) | ✓ | ✓ | ✓ as `Int64` |
| mtime (unix seconds) | ✓ | ✓ | ✓ as `Int64` |
| filename | denormalized | implicit | ✓ |
| hash (computed locally, not from .caf) | n/a | ✓ | ✓ nullable |
| device / root path | ✓ | ✓ as `root_path` | implicit via `catalog_path` |
| is_windows_path heuristic | ✓ | ✓ | — |
| volume label / alias / serial / comment | ✓ in `CafMetadata` | ✓ since v0.1.36 follow-up | — (still UI-only) |
| free space / archive flag / creation date | ✓ in `CafMetadata` | ✓ since v0.1.36 follow-up | — (still UI-only) |
| dir aggregates (file_count / total_size per dir) | ✗ skipped (`skip(12)`) | ✗ | — |

The dir aggregates are pre-computed values that would let us render a
folder tree without iterating all entries. Skipping them is a deliberate
simplification; revisit if the Catalog UI ever wants per-folder size
breakdowns without a full scan.

### Round-trip is semantic, not byte-identical

The `round_trip_v8_preserves_files` test asserts `(file_name, size, mtime)`
tuples match across a load → save → load cycle. It does NOT assert
byte-for-byte equality of the `.caf` output, and earlier docs that
implied "bit-identical round-trip" were too strong. Byte equality
would fail because:

1. We always **write v8**, even when reading v1–v7. Deliberate — bumps
   format and avoids version-specific writer code paths.
2. Volume label / alias / serial / comment / freesize / archive flag
   are parsed into `CafMetadata` but (until the v0.1.36 follow-up) were
   discarded by `FileIndex`, so a load → save → load cycle dropped them.
3. Hashes never round-trip (the format never carried them).

Practically, this means: re-saving a Cathy v6 catalog from 2008 emits a
v8 file readable by Catfish (Cathy's Python successor) but not by the
original Cathy.exe binary that produced it. Acceptable trade — Catfish
is our canonical reference.

### v ≤ 6 zero-size clamp matches Catfish behaviour

`caf.rs:421-429` substitutes `1024` bytes for any v ≤ 6 entry whose
`<i32>` size field is exactly 0, and clamps v > 6 zero-size entries to
`1` byte. Both clamps exist so the `size_index` bucket lookup works (it
keys on size). The v ≤ 6 fallback of 1024 specifically matches what
Catfish does — older Cathy.exe versions wrote 0 for indeterminate sizes
on Win9x. The downside: a genuine zero-byte file in a v ≤ 6 catalog
will report as 1024 bytes here. Real zero-byte files in v ≥ 7 catalogs
report as 1 byte instead of 0. Both are documented quirks, not bugs.

---

## Cloud drives (P11 Pillar 5)

### WebDAV servers post-check `exists()` after DELETE — your provider must invalidate caches

wsgidav's `do_DELETE` calls `child_res.delete()`, then immediately
re-asks `provider.exists(path, environ)`.  If the answer is still
`True`, wsgidav raises `DAVError(HTTP_INTERNAL_ERROR,
"Resource could not be deleted.")` — even though your underlying
storage call succeeded.  This bit filen-python (10-minute folder/file
listing cache wasn't cleared on `trash_item`/`delete_permanent`) and
will bite any provider whose `exists()` reads through a TTL cache that
mutations don't invalidate.  Whenever you write a custom WebDAV
provider on top of an API with cached listings: invalidate the parent
folder's cache (or the path-resolution cache) on every mutation, not
just the entry being deleted.

Symptom to watch for during e2e: `curl -X DELETE` returns 500 with the
exact string "Resource could not be deleted" and the file remains
visible in `PROPFIND` immediately after.  The fix is always at the
provider's `exists()`/`get_resource_inst()` cache, not the `delete()`
implementation.

### Internxt `cli.py` and Filen `cli.py` need `--json` patches to be useful from Rust

Both Python CLIs default to emoji-decorated text output.  Scraping that
from Rust is brittle (kaomojis, table widths, decorations vary across
versions).  The right move is to upstream a `--json` flag on the
read-side commands (`whoami`, `ls`/`list-path`, `resolve`, `trash`)
and parse with `serde_json`.  Patches live in:

* `internxt-python/995a543` — `whoami` / `list-path` / `resolve`
* `filen-python/1162aa0`    — `whoami` / `ls` / `resolve` / `trash`
                              (also added the missing `handle_trash`)

Both CLIs use argparse-style flag-before-subcommand vs flag-after-
subcommand semantics; the patches expose `--json` *both* on the parent
parser and on the patched subparsers so either invocation form works.

### `unittest.mock.patch` is not thread-safe

If two threads each enter their own `with patch(...)` block,
`__enter__` saves and `__exit__` restores the original attribute with
no locking.  Races between t1's `__exit__` and t2's `__enter__` (or
vice versa) leave the real attribute callable for a brief window —
which on CI without credentials triggers exceptions in unrelated
threads, leaving asserted dict slots empty and the failure message
surfacing as a generic `KeyError` instead of the real cause.

When testing thread-locality of state, hoist all `patch()` calls
*outside* the threaded function so a single mock surface wraps the
whole join window.  The threads never see an unpatched value.  Pattern
in `tests/test_webdav_misc.py:test_isolated_session_separate_threads…`
(internxt-python/`9128c3d`).

### `# type: ignore[<code>, unused-ignore]` for cross-platform mypy ignores

Linux's `os.stat_result` stub doesn't expose `st_birthtime` (macOS-only).
Bare `# type: ignore[attr-defined]` is needed on Linux but flagged
"Unused" on macOS.  Mypy's own `unused-ignore` code handles the
meta-warning: `# type: ignore[attr-defined, unused-ignore]` works on
both platforms.  Same trick for libraries with conditional stub
availability (e.g. `waitress`, `cheroot`):
`# type: ignore[import-untyped, unused-ignore]`.  Avoids platform-
specific `if sys.platform == ...:` guards in source code and avoids a
Linux-only mypy CI lane.

---

## Local index & search

### `chunk_index = 0` is required for local search visibility

The local LanceDB query helpers in
`src-tauri/src/index/local_index.rs` filter their representative-row
SELECT with `chunk_index = 0` — see `fetch_by_doc_ids` and
`fetch_by_doc_ids_filtered`.  The comment block at the top of
`schema.rs` says "L1 metadata + L3 representative rows use
`chunk_index <= 0`", which is *almost* right — but the actual
filter is `= 0`, not `<= 0`.  So a row ingested with
`chunk_index = -1` lives in LanceDB, gets indexed by Tantivy,
returns hits at the FTS layer — and then vanishes in the
metadata-join phase of `crispsorter index search`, producing the
mysterious "0 result(s)" with a non-empty FTS hit set.

**Rule:** any DocumentChunk that needs to be findable via
`crispsorter index search` (or browseable in Übersicht) must use
`chunk_index: 0, chunk_total: 1`.  Reserve `-1` only for rows that
are *intentionally* invisible to local search (none today; the
original use case turned out to be a foot-gun).

Got bit by this during the v107 follow-on work for L1-aware local
search: pulled cb-api rows ingested with `chunk_index = -1` (matching
the "manifest-only" convention).  Tantivy reported `num_docs = 30`;
`crispsorter index search` returned 0 of them.  Switched the pull
path to `chunk_index = 0, chunk_total = 1` and search worked
immediately.  See `cli/mod.rs` around the
`CloudBackupCmd::Pull` handler for the live example.

### `cargo build` on this workspace takes 3–9 min cold, 1–3 min warm

The default Rust dev build of `crispsorter` pulls in a non-trivial
chunk of the Tauri + LanceDB + Tantivy + Arrow stack.  Plan around
it when iterating on something that requires a recompile to test
manually — e.g. don't do six small edits and rebuild between each;
batch the edits, rebuild once.  Use `cargo check --package
crispsorter` (~30 s warm) to catch type errors mid-edit; only build
the binary when you actually need to exercise behavior.

### `crispsorter sync cloud-backup pull` writes BOTH LanceDB and Tantivy

The Pull command's chunk-ingest path is two-write: each pulled L1
chunk goes into the local LanceDB documents table via
`local.ingest_batch`, AND each chunk with `full_text` populated
gets added to the local Tantivy FTS via a delete-then-add by doc_id.
Soft-fails on the Tantivy side so a write-protected `fts/` dir
doesn't break the pull.  Emits `[sync] indexed N L1 row(s) into
local Tantivy` on success.

Without the Tantivy write, pulled rows would live in LanceDB but
not be findable via `crispsorter index search`, defeating the
offline-search story.  See `cli/mod.rs` around the
`CloudBackupCmd::Pull` arm.

### `#[serde(default = "…")]` does NOT feed `#[derive(Default)]`

`PageSpec` carries `#[serde(default = "default_page_limit")]` (→ 200)
on its `limit` field, but `PageSpec::default()` still yields
`limit: 0` — the serde attribute only supplies a value when a field is
*absent during deserialization*; Rust's derived `Default` ignores it
entirely and uses `u32::default()`.  `query_documents` then clamps
`limit` to `[1, 1000]`, so a `PageSpec::default()` page returns exactly
**one** row.

This burned ~two 19-minute build cycles chasing a phantom
"`array_has` drops rows from the tag-filtered browse" bug: `count_rows`
(no limit) reported the correct count while the scanner page returned a
single row.  `array_has` + the `lance::Scanner` filter/order/limit
pipeline were fine all along — the test built its page via
`PageSpec::default()`.  The real frontend always sends
`page: { limit: 200, … }`, so the app path was never affected.

**Rule:** in tests/back-end callers, construct `PageSpec { limit: N,
cursor: None }` explicitly; never trust `PageSpec::default()` to carry
the serde default.  (Same trap applies to any struct that mixes
`#[serde(default = "…")]` with `#[derive(Default)]`.)  If a single-row
result smells wrong, check the page limit before suspecting the query
engine.

### The crispsorter target dir can live on a slow external volume

On this machine the Cargo target dir resolves to
`<external-volume>/code/crispsorter-target` — a cold `cargo test`
build of the `crispsorter` lib took **~19 min**.  Budget for it: batch
all edits, run ONE build, and make probe tests *comprehensive* (test
every hypothesis in a single binary) rather than iterating one
spelling/stage per build.  `cargo check` is still the fast
inner-loop for type errors.
