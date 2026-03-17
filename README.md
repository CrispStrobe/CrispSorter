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
- **Script export** — generate a `.bat` / `.sh` script to review moves before executing them
- **Customisable output** — author sub-folders on/off, save extracted `.txt` transcript alongside files
- **Editable grid** — column visibility, width, sort; inline field editing
- **Search index** — optional semantic + full-text search over all sorted documents (local or remote)

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

### Settings UI (Settings → Search Index)

| Setting | Description |
|---|---|
| **Enable search index** | Toggle indexing on/off globally |
| **Search mode** | `Text` (BM25 only), `Vector` (ANN only), or `Hybrid` (RRF) |
| **Backend** | `Local` (on-device LanceDB) or `Remote` (crisp-index-server) |
| **Remote URL** | Base URL of your crisp-index-server, e.g. `https://crisp.example.com` |
| **Remote API key** | Bearer token configured on the server (`CRISP_API_KEY`) |
| **Embedder model** | `BGE-M3` (1024-dim, multilingual, default), `E5-Large`, `E5-Base`, `MiniLM-L6`, `BGE-Small-EN` |
| **Device** | `Auto`, `CPU`, `Metal` (macOS), `CUDA` (Windows/Linux) |
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

# Build production installer and publish to GitHub
.\release.ps1
```

`download-llama-backends.ps1` downloads pre-built llama.cpp binaries for Windows.

### macOS — release script

```bash
# Build production app and publish .dmg to GitHub
./release.sh
```

Requires `gh` CLI authenticated (`gh auth login`).

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
| Embedding (local) | fastembed-rs (BGE-M3 / E5 / MiniLM) |
| Vector store (local) | LanceDB (embedded) |
| Full-text (local) | Tantivy |
| Search server | crisp-index-server (axum + LanceDB + Tantivy) |

---

## License

**AGPL-3.0** — see [LICENSE](LICENSE).

