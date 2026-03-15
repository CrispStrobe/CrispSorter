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

---

## License

**AGPL-3.0** — see [LICENSE](LICENSE).

*Developed by Christian Ströbele · [crispstro.be](https://crispstro.be)*
