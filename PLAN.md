# CrispSorter — Development Plan

## Capabilities (shipped)

- **LanceDB + Tantivy hybrid search** — persistent embedded library with dense ANN + BM25 full-text, RRF fusion
- **ONNX / CoreML backend** — run ONNX-format models via `ort` crate with CoreML execution provider for Apple Neural Engine acceleration
- **CrispEmbed GGUF backend** — feature-gated optional backend using libcrispembed for GGUF model inference (Metal/CUDA/Vulkan GPU acceleration)
- **Expanded model registry** — 36 ONNX/GGUF model variants (BGE-M3, BGE en-v1.5 small/base/large, PIXIE-Rune, Snowflake Arctic L-v2, Jina v2/v3/v5, Qwen3-Embedding, Octen, MiniLM, Multilingual-E5 small/base/large, Nomic-Embed v1.5, Mxbai-Embed Large v1, all-MiniLM-L6-v2, EmbeddingGemma 300M, GTE base/large en-v1.5)
- **OrtPath backend** — handles ONNX models with external `.onnx_data` companion files and KV-cache decoder models
- **Cross-platform release workflow** — GitHub Actions builds for macOS ARM64/x86, Windows, Linux with llama-server sidecar
- **CrispEmbed CI integration** — sibling repo checkout + path rewrite so `cargo metadata` resolves on clean runners

## In Progress

- **Wire CrispEmbed sparse encoding into search pipeline** — BGE-M3/SPLADE sparse vectors via GGUF backend (C API ready, needs UI integration). Tracked under P2.
- **CrispEmbed reranking in search** — cross-encoder and bi-encoder reranking APIs are wired in `CrispEmbedBackend` but not yet used by the search pipeline. Tracked under P2.

---

## Open TODOs

### P2 — Search index / RAG

(All P2 items shipped — see Recent changes.)

### P3 — Voice chat (CrispASR integration)

- [ ] **CrispASR sidecar for voice input** — let the user dictate prompts /
  rename suggestions / chat messages. Mirrors the `crispembed` optional path
  dep pattern: `../../CrispASR/crispasr` + cargo features
  (`crispasr`, `crispasr-metal`, `crispasr-cuda`, `crispasr-vulkan`). One
  Rust thread holds the model; Tauri command takes an audio buffer or a
  recording session handle.
- [ ] **Push-to-talk in Chat UI** — mic button in `Chat.svelte`. WebAudio
  capture → Float32 PCM 16 kHz mono → `invoke('asr_transcribe', { pcm })`.
  Stop button must abort mid-recording and mid-decode.
- [ ] **TTS for LLM answers** — voice the analysis result / chat reply back.
  Decide: native macOS `say` / Windows SAPI for v1 (zero deps), or a small
  GGUF TTS (Piper / Kokoro) sidecar for cross-platform consistency.
  Settings: voice picker, rate, "auto-speak replies" toggle.
- [ ] **Hotword / wake word (optional)** — out of scope for v1, but the ASR
  thread should be designed so a separate small KWS model can gate full-ASR
  decoding when this lands.

### P4 — Code quality / maintenance

- [ ] Audit remaining hardcoded UI strings in `Settings.svelte` (model manager sections)
  and `LogPanel.svelte` and move them to `i18n.svelte.ts`.

### P5 — Future / planned

1. **Custom output path template** — `{author}/{year} - {title}.{ext}` configurable in Settings
2. **BibTeX / Zotero export** — generate `.bib` / RIS from batch metadata
3. **Read PDF metadata** — pre-fill Title/Author/Year from XMP/DocInfo before LLM
4. **Folder Watcher** — auto-ingest new files from a watched directory
5. **PWA demo** — generate `.sh`/`.bat` sorting scripts or browser-based sorting via File System Access API

---

## Recent changes

- [x] **Matryoshka dimension selection (May 2026, v0.1.28)** — new
  `IndexConfig.matryoshka_dim: Option<u32>` threads through
  `EmbedderConfig.with_matryoshka_dim` to `CrispEmbedBackend::set_dim` at
  load. `EmbedderConfig::effective_dim()` clamps to the model's nominal
  dim and treats `Some(0)` as `None` (model default). The LanceDB column
  width now uses the effective dim so the schema matches what the embedder
  emits — changing `matryoshka_dim` on an existing index requires
  re-ingestion (warned in the UI hint). UI: number-select (128/256/384/512/768)
  appears under "Inference Backend" only when GGUF is selected and the
  model has a GGUF spec — fastembed has no per-call truncation hook so
  ONNX paths ignore the field. Quality only holds for MRL-trained models
  (BGE-M3, Snowflake Arctic L v2, PIXIE-Rune); the hint flags this.
- [x] **Sparse retrieval + Octen auto-download (May 2026, v0.1.27)** — BGE-M3
  / SPLADE sparse vectors are now used at query time as a 3rd RRF channel
  alongside FTS + dense ANN. `LocalIndex::search_sparse_in_pool` scores the
  union of FTS+ANN candidates by sparse dot product (two-pointer merge for
  sorted indices, hash-join fallback otherwise) and `SearchEngine::maybe_sparse_search`
  fuses the result via the new generalized `rrf_merge_n`. Auto-on when the
  embedder has a sparse head (BGE-M3, BGE-small en-v1.5 with SPLADE++);
  silently skipped otherwise. Octen 0.6B variants (FP32, INT4, INT8-Full)
  switched from local-only `with_local_subdir` to fastembed-native
  auto-download via `cstr/Octen-Embedding-0.6B-ONNX*` HF repos. The
  matMul-only INT8 variant stays local-only (no fastembed equivalent —
  dropped in fastembed-rs 77cc2e45 due to platform-dependent checksums).
- [x] **Configurable model cache dir (May 2026, v0.1.25)** — new
  `IndexConfig.model_cache_dir: Option<String>` + `resolve_model_cache_dir`
  helper picks: `CRISPSORTER_MODEL_CACHE_DIR` env > UI override >
  `{data_dir}/models/`. Single dir is shared by fastembed (ONNX), hf-hub
  (external-data ONNX + GGUF embedder + GGUF reranker), so one setting
  controls every weight on disk. Settings.svelte adds a "Model cache
  directory" picker; an external volume like
  `<external-volume>/ai/crispsorter-models` lets the cache survive app
  re-installs and (partially) share with CrispEmbed CLI. Three unit tests
  pin the resolve precedence.
- [x] **Cross-encoder reranking pipeline (May 2026, v0.1.25)** — new
  `RerankerModel` enum (`BgeRerankerV2M3`, `BgeRerankerBase`,
  `JinaRerankerV2BaseMultilingual`) + `Reranker` wrapper around
  `crispembed::CrispEmbed::rerank` (cross-encoder only; bi-encoder skipped).
  `RerankerHandle` is a cheap-clonable lazy-load handle: GGUF download +
  model open happens on first `score_batch` call. `SearchEngine` now fetches
  `rerank_top_n` candidates (default 50) from FTS / ANN / RRF when a
  reranker is configured, scores each via `score_batch(query, snippets)`,
  and re-sorts; NaN scores fall back to RRF order. `IndexConfig` gains
  `reranker_model: Option<RerankerModel>` + `rerank_top_n: usize`. UI:
  Settings.svelte adds a "Reranker" section between Compute Device and Data
  Directory. GGUF-only — without the `crispembed` cargo feature, `Reranker::load`
  returns a clear error.
- [x] **Pre-existing FTS regression fixed (May 2026)** —
  `index::fts_index::tests::scenario_accent_folding` was failing on `main`
  before any of this branch's edits: query-side `fold_accents` was applied
  but the index used Tantivy's `default` tokenizer (lowercase only), so
  `München` was indexed as `münchen` and never matched the folded query
  `munchen`. Fixed by registering a custom `ascii_folding` tokenizer
  (SimpleTokenizer + RemoveLong + LowerCaser + AsciiFoldingFilter) on the
  index and using it for the title/headings/body fields. Existing FTS dirs
  need re-ingestion — see LEARNINGS.md for the migration note. Also cleaned
  up clippy: `wrong_self_convention` on `to_gguf_spec`/`to_model_spec`
  (`&self` → `self` since `EmbedderModel: Copy`), and explicit
  `#[allow(dead_code)]` on `CrispEmbedBackend` placeholders that future P2
  work will use.
- [x] **Query/passage prefix selection (May 2026)** — auto-apply model-specific
  prefixes via `EmbedderModel::prefix(EmbedRole)`. E5 (`query:` / `passage:`),
  Nomic v1.5 (`search_query:` / `search_document:`), BGE en-v1.5 + Mxbai
  (BGE-style query-only), Jina v5 (`Query:` / `Document:`), EmbeddingGemma
  (task templates). All other models pass through unprefixed. CrispEmbed path
  uses native `set_prefix`; fastembed/OrtPath paths prepend in Rust. Sparse
  encoders (BGE-M3, SPLADE++) untouched — trained without prefixes.
- [x] **CrispEmbed/fastembed-rs registry sync (May 2026)** — added 12 new
  `EmbedderModel` variants (`MultilingualE5{Small,Base,Large}`, `Bge{Small,Base,Large}EnV15`,
  `NomicEmbedTextV15`, `MxbaiEmbedLargeV1`, `AllMiniLmL6V2`, `EmbeddingGemma300M`,
  `Gte{Base,Large}EnV15`). Each wired through both ONNX (native fastembed-rs
  via `CrispStrobe/fastembed-rs@feat/new-model-entries`) and GGUF (CrispEmbed
  `cstr/*-GGUF` registry). `BgeSmallEnV15` paired with `SparseModel::SPLADEPPV1`
  per `rag_plan.md` §2 rationale. Serde kebab-case test pins frontend mapper.
- [x] Stop button — wires `AbortController` through extraction and LLM queries (v0.1.22)
- [x] Per-request LLM timeout — 3 min local / 60 s remote via `Promise.race` (v0.1.22)
- [x] Extraction hang timeout — 5 min auto-abort on `extractionAbort` controller (v0.1.22)
- [x] Frontend log panel — `flog()` store, merged with Rust `app-log` events in LogPanel (v0.1.22)
- [x] Live processing stats in footer — N/total done · extracting X · analyzing Y (v0.1.22)
- [x] Release workflow — auto-publish draft after matrix even if one platform runner is slow (v0.1.22)
- [x] macOS 13 / `crispembed` stub — created minimal stub so CI/dev builds resolve the optional dep
- [x] Stuck items on resume — `resumeLastSession()` resets extracting/analyzing → unfinished (v0.1.23)
- [x] Per-page extraction watchdog — 30 s no-progress timeout replaces flat 5-min timeout (v0.1.23)
- [x] Two-phase batch processing — extract-all then analyze-all; LLM stall never blocks extraction (v0.1.23)
- [x] `unfinished` status — amber badge, filter option, footer counter, resetStuckItems handles it (v0.1.23)
- [x] i18n status strings — all BatchStatus values translated EN + DE; Chat/BatchReview use them (v0.1.23)
- [x] Chat context title/author — shows suggestedTitle + suggestedAuthor for analyzed docs (v0.1.23)
- [x] Stop button during rate-limit wait — `abortableSleep()` makes 429 backoff honour AbortSignal (v0.1.23)
- [x] Rate-limit Retry-After cap — capped at 90 s to prevent 10-min dead waits (v0.1.23)
- [x] Provider round-robin fallback — processAll phase 2 cycles through fallback providers on failure (v0.1.23)
- [x] Round-robin Settings UI — ordered checklist in LLM Options with up/down reorder (v0.1.23)
- [x] Index location update on move — `index_update_location_by_path` Rust command + TS call (v0.1.23)
- [x] i18n audit: Chat.svelte — "Docs:", "Chat:", "Clear Messages" use i18n keys (v0.1.23)
