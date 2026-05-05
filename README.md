# CrispSorter

**AI-powered document organiser.** Drop in a folder of PDFs, DOCX files, or ebooks — CrispSorter extracts Title, Author, and Year from each one using a local LLM and moves them into a clean, consistent hierarchy like `Sorted/Author/Year/Title.pdf`. Every step runs on your device; nothing leaves your machine unless you explicitly configure a cloud provider.

Successor to BiblioForge and ZotBiblioForge — no Python, no cloud required.

---

## How it works

1. **Ingest** — drag in files or an entire folder tree
2. **Analyse** — a local (or cloud) LLM reads each document and suggests Title, Author, Year
3. **Review** — edit any field inline in the grid; preview extracted text alongside
4. **Sort** — accept suggestions and files are moved to `Sorted/{Author}/{Year}/{Title}.{ext}`

---

## Supported file types

| Format | Extraction method |
|---|---|
| PDF (digital) | pdfjs-dist (JS) or pdf-extract (native Rust) |
| PDF (scanned) | Tesseract.js OCR — multi-language |
| DOCX / Word | mammoth.js |
| EPUB | @lingo-reader/epub-parser |
| TXT / Markdown | direct UTF-8 |

---

## AI backends

### Local / offline (no API key needed)

| Backend | Notes |
|---|---|
| **Ollama** | Easiest option — CrispSorter can start the server for you and pull models |
| **mistral.rs** | Native binary, CUDA on Windows, Metal on macOS |
| **llama.cpp** | GGUF sidecar, configurable GPU offload layers |
| **MLX** | Apple Silicon Neural Engine + GPU (macOS only) |
| **WebLLM** | Runs compact models in-app via WebGPU; no server, no install |
| **ONNX Runtime** | Transformers.js with WebGPU or WASM/CPU fallback |

### Cloud (opt-in, bring your own key)

Groq · OpenRouter · Mistral · OpenAI · Nebius · Scaleway

---

## Features

- **OCR** — Tesseract with English, German, French, Spanish, Italian and more; force-OCR per file
- **Batch operations** — multi-select, bulk re-analyse with different models, bulk accept/reject
- **Duplicate detection** — content hashing identifies near-identical files across a batch
- **Session persistence** — auto-save and resume; full session history
- **Built-in AI chat** — query across the documents in your current batch using any configured provider
- **Voice chat (push-to-talk + auto-speak)** — mic button transcribes speech via on-device CrispASR; replies are read back through the platform's native synth (macOS `say`, Windows SAPI, Linux espeak/spd-say). All offline; opt-in.
- **Folder watcher** — watch one or more folders; new files dropped in get auto-added to the batch (no auto-move — you still review and press Start)
- **PDF metadata pre-fill** — read Title / Author / Year from a PDF's `/Info` dict and XMP packet before the LLM runs; useful fallback when you skip the LLM or it fails
- **BibTeX export** — generate a `.bib` file from sorted batch metadata; LaTeX-escaped, deduplicated citation keys
- **Script export** — generate a `.bat` / `.sh` script to review moves before executing them
- **Customisable output** — `{Author}/{Year}/{Title}` template configurable in Settings, save extracted `.txt` transcript alongside files
- **Editable grid** — column visibility, width, sort; inline field editing
- **Search index** — optional semantic + full-text search over all sorted documents (local or remote), with optional cross-encoder reranking, sparse retrieval (BGE-M3/SPLADE), and Matryoshka dim truncation

---

## Search index

CrispSorter can build a searchable index of your sorted documents — combining BM25 full-text search (Tantivy) with dense vector search (LanceDB) fused via Reciprocal Rank Fusion (RRF). This lets you ask natural-language questions across your entire library.

### Two backends

#### Local backend (default)

Everything runs on your machine.

```
Documents
  └─► Extract text / markdown (PDF, DOCX, TXT, MD)
  └─► Chunk text (sliding window, configurable size)
  └─► Embed locally (fastembed — BGE-M3, E5-Large, MiniLM, …)
  └─► Write to local LanceDB + Tantivy
  └─► Search via hybrid RRF
```

Best for: privacy-first use, laptops with enough RAM, small-to-medium libraries.

#### Remote backend (crisp-index-server)

Embedding happens locally; storage and search happen on your self-hosted server.

```
Documents
  └─► Extract text / markdown  (same as local)
  └─► Chunk + embed locally    (fastembed — required even in remote mode)
  └─► POST /v1/ingest          ──► crisp-index-server VPS
                                       ├── LanceDB (ANN)
                                       └── Tantivy (BM25)
  └─► POST /v1/search          ──► server runs hybrid RRF
                                       └─► results returned to app
```

Best for: shared team libraries, very large corpora, keeping client storage small.

No GPU is needed on the server — all neural embedding is done by the client.

---

### GPU acceleration

The local embedder uses ONNX Runtime with automatic execution-provider selection:

| Setting | Backend used |
|---|---|
| `Auto` (default) | CoreML + Metal on macOS · CUDA on Windows/Linux · CPU fallback |
| `Metal` | Apple CoreML / Metal / Neural Engine (macOS only) |
| `CUDA` | NVIDIA CUDA (Windows/Linux) |
| `CPU` | Force CPU — lower memory pressure, no GPU required |

On an M-series Mac with BGE-M3, expect ~2–3 GB RAM (ONNX arena + model weights) and ~1–3 s per document for embedding.

---

### Search query syntax

The full-text component of every search mode supports the following syntax:

| Pattern | Meaning | Example |
|---|---|---|
| `word` | Exact term (case-insensitive) | `barth` |
| `word1 word2` | Implicit AND — both terms required | `karl barth` |
| `word1 AND word2` | Explicit AND | `grace AND theology` |
| `word1 OR word2` | Either term | `rahner OR barth` |
| `NOT word` | Exclude term | `NOT nietzsche` |
| `"phrase"` | Exact phrase | `"grace alone"` |
| `word*` | Prefix wildcard | `theolog*` matches *theologisch*, *theology*, … |
| `wor?` | Single-character wildcard | `grac?` |
| `word~2` | Fuzzy match (edit distance) | `barth~1` also matches *Bart* |
| `a w/10 b` | *a* within 10 words of *b* (either order) | `grace w/5 faith` |
| `a pre/5 b` | *a* appears before *b* within 5 words | `sola pre/3 fide` |
| `(a OR b) w/N c` | Grouped proximity | `(faith OR grace) w/20 works` |

> **Hybrid mode** runs full-text and vector (semantic) search in parallel and fuses them with Reciprocal Rank Fusion. You get both keyword precision and semantic recall.

---

### Supported document formats for indexing

| Format | Plain text | Markdown / headings |
|---|---|---|
| PDF | pdfjs-dist text layer | heuristic heading detection |
| DOCX | mammoth plain-text | `mammoth.convertToMarkdown` |
| TXT | direct | — |
| MD / Markdown | direct | `#`/`##`/`###` headings parsed |
| EPUB | epub-parser text | — |

Headings extracted from DOCX/MD/PDF are stored in the index and boost search relevance.

---

### Embedding models

CrispSorter ships with a carefully benchmarked set of embedding models. All run entirely on-device via ONNX Runtime with optional CoreML / CUDA acceleration.

#### Benchmark results

Measured on Apple M-series (CPU-only, batch=32, 3 documents, hybrid search).
`ch/s` = document-chunks embedded per second · `Acc` = top-1 retrieval accuracy (0–1) · `RSS` = resident memory while the model is loaded.

| Model | Dims | ch/s | Acc | RSS MB | Notes |
|---|---|---|---|---|---|
| Jina-v2 Small EN | 512 | 8.56 | 1.00 | 2421 | Fast encoder, English |
| Multilingual MiniLM | 384 | 6.10 | 1.00 | 2505 | Fastest multilingual; lower quality |
| Qwen3-Emb uint8 (calibrated) | 1024 | 6.01 | 1.00 | 1407 | Compact, calibrated quant |
| **Octen-0.6B INT8** (default) | 1024 | 6.09 | 1.00 | 1348 | ✅ Best balance; recommended |
| Octen-0.6B INT8 Full | 1024 | 6.35 | 1.00 | 1207 | Smallest RAM (~1.2 GB); embedding table also quantized; 570 MB file |
| Qwen3-Emb INT8 | 1024 | 5.78 | 0.50 | 1857 | Lower accuracy on hybrid test |
| Jina-v2 Base EN | 768 | 6.85 | 1.00 | 2843 | Solid English encoder |
| Snowflake Arctic-L v2 | 1024 | 5.77 | 1.00 | 2479 | |
| BGE-M3 | 1024 | 2.39 | 1.00 | 3266 | Also produces sparse vectors for hybrid BM25+dense fusion |
| **Octen-0.6B INT4** | 1024 | 2.62 | 1.00 | 1151 | 🔋 Lowest RAM; good for constrained machines |
| PIXIE-Rune-v1.0 | 1024 | 4.04 | 1.00 | 3489 | 74 languages |
| Octen-0.6B FP32 | 1024 | 3.89 | 1.00 | 2590 | Reference; no accuracy gain over INT8 |
| Jina-v5 Nano | 768 | 1.98 | 1.00 | 2051 | 32k context |
| Jina-v3 | 1024 | 0.16 | 1.00 | 5153 | Multilingual, very slow on CPU |

#### About the Octen models

**Octen-Embedding-0.6B** is a Qwen3-0.6B fine-tune trained specifically for semantic search and retrieval. The FP32, INT8, and INT4 ONNX files are produced by our own `export_octen_onnx.py` / `quantize_octen_int8.py` / `quantize_octen_int4.py` scripts from the original `Octen/Octen-Embedding-0.6B` safetensors — no third-party ONNX conversions.

| Variant | File size | Quantisation method | RAM (RSS) |
|---|---|---|---|
| FP32 | 2.38 GB | none (reference) | ~2.6 GB |
| INT8 | 1.06 GB | ORT dynamic, MatMul-only, per-tensor | ~1.3 GB |
| INT8 Full | 0.57 GB | ORT dynamic, MatMul + Gather (embedding table) | ~1.4 GB |
| INT4 | 0.90 GB | ORT `MatMulNBits`, block_size=32, symmetric | ~1.2 GB |

The embedding layer (token lookup table, ~600 MB) is intentionally left in FP32 in the INT8 and INT4 variants — quantising it saves memory but measurably degrades multilingual quality. The INT8 Full variant does quantise the embedding table, saving ~450 MB vs INT8.

All four variants maintain **1.00 retrieval accuracy** on the benchmark suite (top-1 hybrid search). INT4 is ~15% smaller than INT8 but runs at roughly half the throughput on CPU due to `MatMulNBits` dequantisation overhead. Choose INT8 for speed, INT4 if you need to minimise resident memory.

#### Quantization quality metrics

Measured on Apple M-series (CPU, batch=1, 8 texts across 3 language-topic pairs).
`Cosine drift` = mean cosine similarity between quantized and FP32 embeddings (1.0 = identical) · `Min drift` = worst-case per-vector cosine · `Triplet margin` = mean (sim(anchor,positive) − sim(anchor,negative)) · `Anisotropy` = avg pairwise cosine over 8 diverse texts (lower = more uniform embedding space).

| Variant | Cosine drift (mean) | Cosine drift (min) | Ordering (3/3) | Triplet margin | Anisotropy | Unit-norm |
|---|---|---|---|---|---|---|
| INT8 (MatMul-only) | 0.8301 | 0.6737 | ✅ 3/3 | 0.2398 | 0.2358 | ✅ |
| INT8 Full (+ Gather) | 0.8382 | 0.6975 | ✅ 3/3 | 0.2604 | 0.2245 | ✅ |
| INT4 (MatMulNBits) | **0.9451** | **0.9303** | ✅ 3/3 | 0.2412 | 0.2333 | ✅ |

**Notable finding:** INT4 has *higher* cosine fidelity to FP32 than INT8, because `MatMulNBits` uses fine-grained block-wise quantisation (block_size=32) while dynamic INT8 uses coarser per-tensor calibration. All three quantised variants correctly rank semantically related pairs above unrelated ones across English and German texts.

### Settings UI (Settings → Search Index)

| Setting | Description |
|---|---|
| **Enable search index** | Toggle indexing on/off globally |
| **Search mode** | `Text` (BM25 only), `Vector` (ANN only), or `Hybrid` (RRF + optional sparse) |
| **Backend** | `Local` (on-device LanceDB) or `Remote` (crisp-index-server) |
| **Remote URL** | Base URL of your crisp-index-server, e.g. `https://crisp.example.com` |
| **Remote API key** | Bearer token configured on the server (`CRISP_API_KEY`) |
| **Embedder model** | 36 variants spanning BGE / E5 / MiniLM / Nomic / Mxbai / Snowflake / PIXIE / Qwen3 / Octen / Jina / GTE / EmbeddingGemma. Asymmetric query/passage prefixes auto-applied per model. |
| **Inference Backend** | `ONNX` (fastembed/ORT) or `GGUF` (CrispEmbed — Metal/Vulkan/CUDA via llama.cpp); only shown for models with both backends |
| **Reranker** | Optional cross-encoder rerank pass over the top-N hybrid hits (BGE-Reranker v2-m3 / base, Jina-Reranker v2 multilingual). GGUF only. |
| **Matryoshka dim** | Truncate embeddings to a smaller dim (128/256/384/512/768) — only meaningful for MRL-trained models (BGE-M3, Snowflake Arctic L v2, PIXIE-Rune). GGUF only. |
| **Device** | `Auto`, `CPU`, `Metal` (macOS), `CUDA` (Windows/Linux) |
| **Model cache directory** | Where downloaded weights live (ONNX + GGUF + reranker). External-volume override survives app re-installs. Honours `CRISPSORTER_MODEL_CACHE_DIR` env var. |
| **Data directory** | Where local LanceDB + Tantivy files are stored |
| **Apply & Init** | Apply settings and (re)initialise the index |
| **Build IVF-PQ** | Build approximate nearest-neighbour index after bulk ingest (≥ 10 000 rows) |

> The embedder model and dimension **must match** between client and server. Change `EMBED_DIMS` on the server when switching models.

---

### Location tracking

When a file is moved during a sort operation, CrispSorter updates its stored `location_uri` in the index so search results always point to the current file path. URIs follow the scheme:

```
crisp+local://<machine-uuid>/<user-uuid>/absolute/path/to/file.pdf
```

Remote backend: the update is sent as `POST /v1/docs/:doc_id/location`.

---

### Building the ANN index (IVF-PQ)

LanceDB performs a flat brute-force scan on small datasets. Once you have indexed ≥ 10 000 chunks, click **Build IVF-PQ** in Settings (or call `POST /v1/admin/build-ivf-pq` on the server) to build an approximate nearest-neighbour index. Vector search becomes ~10–100× faster on large libraries.

---

## Development

### Prerequisites

- **Node.js** (LTS)
- **Rust** via [rustup.rs](https://rustup.rs/)
- **Windows GPU** (optional): CUDA 12.x + Visual Studio Build Tools with "Desktop development with C++"

### Quick start

```bash
npm install
npm run tauri dev
npm run tauri build
```

### Windows — optimised scripts

```powershell
# Set up MSVC / Rust environment paths
.\paths.ps1

# Dev mode (add --clean for a fresh build after feature-flag changes)
.\recompile.ps1

# Production .exe (add --clean for a fresh full rebuild)
.\recompile-exe.ps1

# Build production installer and publish to GitHub
.\release.ps1
```

`recompile.ps1` and `recompile-exe.ps1` automatically pick up CrispEmbed
when the sibling repo (`..\CrispEmbed`) and a staged prebuilt
(`src-tauri\crispembed-prebuilt\crispembed.lib`) are both present — they
delegate to `enable-crispembed.ps1` with the matching `-Mode` and pass
`--clean` through. Pass `--no-crispembed` to opt out for a single run
without removing the staged prebuilt.

`download-llama-backends.ps1` downloads pre-built llama.cpp binaries for Windows.

### Optional: CrispEmbed (GGUF) backend

CrispSorter ships with two embedding backends: **FastEmbed (ONNX)** by default,
and **CrispEmbed (GGUF)** as an opt-in. CrispEmbed reuses the llama.cpp GPU
stack (Vulkan / CUDA / Metal), gives smaller model files via GGUF
quantisation, and is significantly faster on supported models (≈ 9× faster
than FastEmbed on MiniLM-L6 per the upstream benchmarks).

**It is feature-gated at compile time.** Default builds (`recompile.ps1`,
`npm run tauri dev`) deliberately *do not* link CrispEmbed in, because:

- The high-level `crispembed` Rust crate lives in the sibling repo
  [CrispStrobe/CrispEmbed](https://github.com/CrispStrobe/CrispEmbed)
  (Cargo path dep at `../../CrispEmbed/crispembed`).
- The native C++ library can either be built from source via CMake
  (~15 minutes) or downloaded as a prebuilt tarball from CrispEmbed's
  GitHub release.

To enable it, use the **`enable-crispembed`** helper instead of `recompile`:

```powershell
# Windows (dev)
.\enable-crispembed.ps1

# Windows (production .exe)
.\enable-crispembed.ps1 -Mode build

# Force a specific GPU backend (default: vulkan on Win/Linux, metal on macOS)
.\enable-crispembed.ps1 -Backend cuda
.\enable-crispembed.ps1 -Backend cpu

# Skip the prebuilt download (reuse already-extracted libs)
.\enable-crispembed.ps1 -SkipDownload
```

```bash
# macOS / Linux
./enable-crispembed.sh
./enable-crispembed.sh build               # production
./enable-crispembed.sh dev --backend cuda
./enable-crispembed.sh dev --skip-download
```

The script:

1. Ensures the `CrispEmbed` source repo is checked out at `..\CrispEmbed`
   (`gh repo clone` if missing).
2. Downloads the OS-matching prebuilt C++ library tarball from CrispEmbed's
   latest GitHub release into `src-tauri\crispembed-prebuilt\`.
3. Sets `CRISPEMBED_SYS_LIB_DIR` so `crispembed-sys` links the prebuilt
   instead of running its own CMake build.
4. Copies `crispembed.dll` + `ggml*.dll` into:
   - `src-tauri\target\debug\` and `src-tauri\target\release\` so the dev
     and production .exe can find them at runtime,
   - `src-tauri\bin\` so the Tauri bundler picks them up for the installer
     (per `tauri.conf.json` → `resources: ["bin/*.dll"]`).
5. Hands off to `npm run tauri dev` or `npm run tauri build` with the
   matching Cargo feature flag (`crispembed-vulkan` / `crispembed-metal` /
   `crispembed-cuda` / `crispembed`).

Once it succeeds, the **CrispEmbed (GGUF)** option in *Settings → Search Index*
is no longer greyed out for models that have a verified GGUF equivalent
(PIXIE-Rune, Snowflake Arctic-L v2, Octen-0.6B, Jina v5, Qwen3-Embedding,
BGE-large-EN-v1.5, multilingual-E5-large, mxbai-embed-large-v1,
nomic-embed-text-v1.5).

#### GPU acceleration with CrispEmbed

The upstream **prebuilt CrispEmbed tarballs are CPU-only** (no
`ggml-cuda.dll` / `ggml-vulkan.dll` / `ggml-metal.dylib`). If you pass
`-Backend cuda` / `vulkan` / `metal`, the script still runs and the app
still launches, but inference falls back to CPU. The script prints a
warning when this happens.

For real GPU acceleration, build CrispEmbed from source and point the
script at the resulting library directory:

```powershell
# 1. Build CrispEmbed with the GPU backend you want
cd ..\CrispEmbed
.\build-cuda.bat            # or .\build-vulkan.bat
cd ..\CrispSorter

# 2. Tell the enable script to use that build instead of the GH release tarball
.\enable-crispembed.ps1 -Backend cuda -LibDir ..\CrispEmbed\build-cuda\src\Release
```

(The `-LibDir` argument also wins over `CRISPEMBED_SYS_LIB_DIR` in the
environment, so if you've already set that you can simply re-run the
script with `-SkipDownload`.)

Mirroring CrispASR's per-target tarball matrix (CUDA / Vulkan / Metal
variants in upstream CI) is on the roadmap — see
[`PLAN.md`](./PLAN.md) → "CrispEmbed CI: per-target lib tarballs".

### macOS — release script

```bash
# Build production app and publish .dmg to GitHub
./release.sh
```

Requires `gh` CLI authenticated (`gh auth login`) and `create-dmg` (`brew install create-dmg`).

---

## Troubleshooting

### Missing CLI Logs
By default, Tauri 2 does not pipe frontend `console.log` to the terminal. To see these:
1. **Developer Tools**: Right-click in the app and select **Inspect Element** (or `Cmd+Opt+I` on macOS) to open the WebView console.
2. **Rust Logs**: For backend/sidecar logs, run with:
   ```bash
   RUST_LOG=debug npm run tauri dev
   ```

### EPUB Extraction / "process is not defined"
If EPUB extraction fails with a reference to the Node.js `process` global, ensure the global shim in `src/app.html` is present. CrispSorter includes a built-in shim for `process.env`, `process.version`, and `process.cwd()` to support browser-incompatible libraries.

---

## Architecture

| Layer | Technology |
|---|---|
| Frontend | Svelte 5 (Runes) + SvelteKit + Lucide Icons |
| Chat UI | Deep Chat |
| Desktop shell | Tauri v2 (Rust) |
| Native inference | mistral.rs (CUDA / Metal) |
| In-app inference | WebLLM (`@mlc-ai/web-llm`), ONNX Runtime (`@huggingface/transformers`) |
| PDF extraction | pdfjs-dist + pdf-extract (Rust) |
| OCR | Tesseract.js |
| DOCX | mammoth.js |
| Persistence | tauri-plugin-store |
| Embedding (local) | fastembed-rs (ONNX) — fork at `CrispStrobe/fastembed-rs` `feat/new-model-entries` |
| Embedding (GGUF) | [CrispEmbed](https://github.com/CrispStrobe/CrispEmbed) — optional sibling crate; Metal/Vulkan/CUDA via llama.cpp |
| Speech-to-text | [CrispASR](https://github.com/CrispStrobe/CrispASR) — optional sibling crate (Whisper/Qwen3-ASR/FastConformer) |
| Text-to-speech | Native platform synth — `say` (macOS), SAPI (Windows), `spd-say`/`espeak` (Linux) |
| Vector store (local) | LanceDB (embedded) |
| Full-text (local) | Tantivy (with ASCII-folding for German umlaut search) |
| Folder watcher | `notify` (FSEvents/inotify/ReadDirectoryChangesW) |
| PDF metadata | `lopdf` (/Info dict) + `quick-xml` (XMP packet) |
| Search server | crisp-index-server (axum + LanceDB + Tantivy) |

---

## License

**AGPL-3.0** — see [LICENSE](LICENSE).

