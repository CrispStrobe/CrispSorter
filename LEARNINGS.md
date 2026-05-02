# CrispSorter — Learnings & Key Insights

Critical things we've learned that are easy to forget when returning to this codebase.

---

## Build & CI

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
