# CrispSorter

A cross-platform, offline-first document sorting tool built with SvelteKit, Tauri v2, and TypeScript.
Successor to BiblioForge and ZotBiblioForge — no Python, no cloud required.

## Core Workflow

CrispSorter has two modes, togglable per batch:

### 1. Extraction Mode
Extract PDFs, DOCX, ebooks to `.txt`. No AI needed.
Add files → set mode to **Text Only** → Start.

### 2. AI Sort Mode
Automatically organize a messy library into a clean hierarchy.
- **Ingest**: Drag-drop files or folders (recursive), or use Add / Add Folder
- **Analyze**: LLM extracts `Title`, `Author`, `Year` from document text
- **Re-analyze**: Select rows → click Re-analyze ▾ to override provider/model/context/author-step per run
- **Review**: Inline editing in the grid; split-pane text preview
- **Sort**: Accept rows → Rocket button → Rust backend moves files to `Sorted/Author/Year - Title.ext`

## What's Implemented

| Feature | Status |
|---|---|
| PDF / DOCX / TXT / EPUB / MD extraction | ✅ |
| Rust-native PDF extraction fallback | ✅ |
| Tesseract.js OCR (WASM) | ✅ |
| Ollama / OpenAI / Groq / Mistral / Gemini providers | ✅ |
| mistral.rs in-process backend (GGUF) | ✅ |
| llama.cpp sidecar backend (Metal, HTTP) | ✅ |
| Recursive folder import + drag-drop | ✅ |
| Duplicate detection (size + optional SHA-256) | ✅ |
| Per-run re-analyze overrides (provider/model/ctx) | ✅ |
| Selective re-analyze (only selected rows) | ✅ |
| Stop/resume batch processing | ✅ |
| Auto author title-stripping (Dr./Prof./PhD) | ✅ |
| Robust JSON+XML parsing with auto-detection | ✅ |
| EN/DE prompt variants | ✅ |
| Session save/resume/export/import | ✅ |
| Column sorting, resizing, visibility | ✅ |
| i18n (EN/DE) | ✅ |

## Roadmap

1. **Custom output path template** — `{author}/{year} - {title}.{ext}` configurable in Settings
2. **BibTeX / Zotero export** — generate `.bib` / RIS from batch metadata
3. **Read PDF metadata** — pre-fill Title/Author/Year from XMP/DocInfo before LLM
4. **Folder Watcher** — auto-ingest new files from a watched directory
5. **LanceDB** — persistent embedded library with full-text search
6. **ONNX / CoreML backend** — run ONNX-format models via `ort` crate with CoreML execution provider for Apple Neural Engine acceleration
7. **PWA demo** — generate `.sh`/`.bat` sorting scripts or browser-based sorting via File System Access API

## LLM Backend Comparison (Apple Silicon)

| Backend | Format | Acceleration | Notes |
|---|---|---|---|
| **llama.cpp sidecar** | GGUF | Metal GPU | Best throughput today; already integrated |
| **mistral.rs** | GGUF | Metal (candle) | In-process, no HTTP overhead |
| **Ollama** | GGUF | Metal GPU | Easiest setup; HTTP overhead |
| **ONNX Runtime + CoreML** | ONNX | ANE / GPU / CPU | Best power efficiency; requires converted models |
| OpenAI / Groq / etc. | — | Cloud | No local hardware required |

For batch document sorting on Apple Silicon, **llama.cpp with Metal** currently gives the best tokens/sec. **CoreML/ANE** leads in power efficiency for sustained workloads.

A built-in benchmark (tokens/sec per provider for a standard prompt) is planned alongside the ONNX backend.

## Architecture

- **Frontend**: Svelte 5 (Runes) + Lucide Icons
- **Backend**: Tauri v2 (Rust) — file ops, sidecar management, PDF extraction
- **Extractors**: `pdfjs-dist`, `mammoth.js`, `epub-parser`, `Tesseract.js`
- **LLM clients**: llmClient abstraction over HTTP (Ollama/OpenAI-compat) + mistral.rs in-process
- **Persistence**: `tauri-plugin-store` with automatic session save/resume

## Development

```bash
npm install
npm run tauri dev
npm run tauri build   # produces .dmg / .exe / .deb
```
