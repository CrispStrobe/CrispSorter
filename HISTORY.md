# CrispSorter — History of Done Work

> Anything in this file is shipped. New work goes in `PLAN.md`.

---

## Search index (RAG) — backend

- **LanceDB + Tantivy hybrid search** — persistent embedded library with dense ANN + BM25 full-text, RRF fusion
- **Rich query translator** — AND/OR/NOT, phrases, wildcards, fuzzy, `w/N`, `pre/N`, grouped proximity → Tantivy query tree (`fts_query.rs`, with tests)
- **`FileLocation` URI model** — `crisp+local`, `crisp+vps`, `crisp+internxt`, `crisp+internxt-zip` parsing/serialisation with retrieval-cost classification (`location.rs`, with tests)
- **Arrow schema with escape hatch** — `metadata_json` JSON field for forward-compatible additions; `chunk_index = -1` reserved for whole-document metadata rows
- **Heading-aligned chunking** — split at headings, sub-divide long sections into 512-token windows with 128-token stride
- **Search engine** — parallel FTS + ANN with RRF (k=60) merge, scalar pre-filtering by owner/language/year
- **IVF-PQ vector index build** — exposed via `index_build_ivf_pq` Tauri command
- **Owner / multi-user filter** — `owner_id` on every chunk, default-filtered in queries
- **`crisp-index-server` skeleton** — Axum VPS server with stub handlers (real LanceDB/Tantivy logic still TODO)

## 2026-05-05 session — UI restructure, L2 metadata, multi-catalog, GGUF expansion, CrispEmbed enable

- **Catalog UI consolidation** — `Index Suche` and `Index Ingest` merged into a single **Kataloge** top-level entry. Sub-views: Übersicht (browse + filter) / Suche / Hinzufügen / Quellen.
- **L2 metadata extraction** — new `index/l2_metadata.rs` module reads PDF Info dict, DOCX `docProps/core.xml`, EPUB OPF without full text extraction. Tauri command `index_promote_l2(doc_ids)` upgrades existing L1 rows. Catalog Übersicht has a "Auf L2 anheben" bulk action. 5/5 unit tests pass.
- **Multi-catalog management** — Settings → Search Index gains a Catalogs panel: create / rename / delete / select-active. Each catalog bundles dataDir + mode + backend + embedder + device. Active catalog auto-applies on selection. `applyIndexConfig` syncs back to the active catalog.
- **GGUF model registry expansion** — added `BgeLargeEnV15` / `MultilingualE5Large` / `MxbaiEmbedLargeV1` / `NomicEmbedTextV15` enum variants + dropdown options + i18n + GGUF capability flags.
- **`enable-crispembed.{ps1,sh}`** scripts — auto-clone CrispEmbed sibling repo, download prebuilt C++ tarball from CrispEmbed GH release, set `CRISPEMBED_SYS_LIB_DIR`, hand off to dev/build with `--features crispembed-{vulkan,cuda,metal,cpu}`. Tested all flag permutations including fresh download. README has a new "Optional: CrispEmbed (GGUF) backend" section.
- **`hf_prefetch.rs`** — workaround for `hf_hub 0.4.3`'s Windows symlink-or-rename bug (silent `create_dir_all().ok()` then rename fails with `os error 3`). Downloads HF files via reqwest into the on-disk cache layout `hf_hub::Cache::get` reads on cache hit.
- **Per-model download size + concurrent-init lock** — `EmbedderModel::approx_download_mb()` covers 25 models, surfaced via new `index_model_download_mb` command. `IndexState.initializing` flag rejects duplicate `index_init` calls so a double-click can't fire two parallel multi-GB downloads.
- **`index_capabilities` command** — returns `{ crispembed: cfg!(feature = "crispembed") }` so the UI can disable / annotate the GGUF toggle when the binary wasn't built with CrispEmbed.
- **Embedder logging** — `app_log!` calls on every model-fetch step (init / per-file start with size / progress per 10 % AND per 50 MB / cache-hit / failure). The previous silent "Failed to retrieve onnx/model.onnx" error path is gone.
- **Windows build fixes** — removed `+crt-static` from `src-tauri/.cargo/config.toml` (conflicted with prebuilt ORT dynamic-CRT imports); made `scripts/generate-licenses.js` permissive when `cargo-license` is missing (only fatal when `LICENSES_REQUIRE=1`).
- **Extractor coverage** — HTML / HTM (DOMParser, charset-aware), WebP / PNG / JPG / BMP / TIFF (Tesseract OCR via Blob URL), legacy DOC stub with friendly error.
- **About panel** — version pill (Vite-injected from `package.json`); license loading / error / empty states with explicit "run `npm run licenses:gen`" hint.
- **Tesseract refresh button** — probes IndexedDB + Cache Storage for cached `*.traineddata`.
- **Keyboard shortcuts** — Delete/Backspace removes selected entries in Stapel; Enter saves Metadaten edits, Esc reverts.
- **Backend selector** — toggle now labelled "FastEmbed (ONNX)" / "CrispEmbed (GGUF)" with engine-specific hint text + always-visible (disabled state with reason when GGUF unavailable).

## Search index — frontend

- **Settings → Search Index panel** — enable toggle, mode/backend/embedder/device selectors, remote URL+key, data-dir picker, Apply & Init, IVF-PQ build button, init progress stream
- **Index Ingest tab** — drag-drop, folder management, contents list, delete-from-index, stats bar
- **Per-doc location update on Sort** — `index_update_location` called after every move/copy

## Embedder backends

- **ONNX / CoreML backend** — run ONNX-format models via `ort` crate with CoreML execution provider for Apple Neural Engine acceleration
- **CrispEmbed GGUF backend** — feature-gated optional backend using libcrispembed for GGUF model inference (Metal/CUDA/Vulkan GPU acceleration)
- **Expanded model registry** — 24 ONNX model variants (BGE-M3, PIXIE-Rune, Snowflake Arctic, Jina v2/v3/v5, Qwen3-Embedding, Octen, MiniLM)
- **OrtPath backend** — handles ONNX models with external `.onnx_data` companion files and KV-cache decoder models

## Build / release

- **Cross-platform release workflow** — GitHub Actions builds for macOS ARM64/x86, Windows, Linux with llama-server sidecar
- **CrispEmbed CI integration** — sibling repo checkout + path rewrite so `cargo metadata` resolves on clean runners
- **AppImage `APPIMAGE_EXTRACT_AND_RUN=1` fix** — works around missing FUSE on ubuntu-24.04 GitHub runners
- **AppImage dropped from Linux release bundles** — only `.deb` ships; AppImage was unreliable
- **llama-server sidecar with dynamic backends** — Vulkan/CUDA DLLs loaded from `bin/`, `download-llama-backends.ps1` script

## App UX

- **Stop button + in-app log panel** — `LogPanel.svelte`, ring buffer relayed via `app-log` event
- **Session persistence + history** — auto-save, resume, full session history with import/export
- **Editable grid** — column visibility, width, sort; inline field editing
- **Duplicate detection** — content hashing with shallow / deep modes
- **Multi-select + batch operations** — re-extract / re-analyse / accept-all / reject-all on selection
- **Path template** — `{Author}/{Year}/{Title}` configurable, with live preview and presets
- **Execution report modal** — copy/move stats, locked-files warnings, choice of post-execute cleanup mode
- **WebLLM provider** — runs models entirely in WebView via WebGPU; cached in IndexedDB
- **ORT / Transformers.js provider** — ONNX Runtime in-browser via `@huggingface/transformers`
