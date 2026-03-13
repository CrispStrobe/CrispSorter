# CrispSorter

A cross-platform, robust file content extraction and sorting tool built with SvelteKit, Tauri, and TypeScript.
This app is the JS-first successor to BiblioForge and ZotBiblioForge, designed to be completely free of Python dependencies.

## 🚀 The Core Workflow

CrispSorter operates in two distinct modes, togglable per batch:

### 1. Simple Extraction Mode (.txt)
- **Goal**: Quickly turn PDFs, DOCX, and Ebooks into searchable text.
- **Workflow**: 
  - Add files -> Toggle "Metadata Extraction" to **OFF** -> Start.
  - The app extracts the text locally using JS-native libraries.
  - Results are ready for manual review or export to `.txt`.

### 2. AI Sorting Mode (Metadata + Rename)
- **Goal**: Automatically organize a messy library into a clean hierarchy.
- **Workflow**:
  - **Configure**: Enter your LLM keys (Ollama, Groq, OpenAI, etc.) in Settings.
  - **Ingest**: Drag and drop files into the Batch Review grid.
  - **AI Analysis**: Toggle "Metadata Extraction" to **ON**. The app extracts text, sends a sample to the LLM, and suggests `Title`, `Author`, and `Year`.
  - **Review**: Use the **Total Commander style grid** to verify suggestions. Click any row to see a split-pane preview of the raw text vs. suggestions.
  - **Sort**: Bulk-accept verified rows and hit the **Rocket button**. The Rust backend renames and moves files into a clean `Sorted/Author/Year - Title.ext` structure.

## 👁️ OCR Strategy (Handling Scanned Docs)

CrispSorter stays lean by default but offers two paths for OCR:

1.  **Visual OCR (VLM Path)**: 
    - If you use a Vision-capable LLM (like GPT-4o, Claude 3.5 Sonnet, or local Ollama with LLaVA), CrispSorter can send document snapshots directly for "Visual Extraction." No local OCR binaries needed.
2.  **Local OCR (WASM Path)**:
    - We utilize **Tesseract.js (WASM)** for basic local OCR. This runs entirely in the JS engine and downloads the 50MB language models *only when first needed*, keeping the initial app bundle tiny.

## 🗺️ Future Roadmap (Python-Free Expansion)

### Phase 3: Robust Library Management (LanceDB)
- Move from JSON state to **LanceDB** (embedded Rust-native DB).
- Store all extracted text and metadata permanently.
- Enable high-performance local Full-Text Search (FTS) across your entire sorted library.

### Phase 4: RAG & NAS Integration
- **Hybrid Storage**: Support connecting to a **Remote LanceDB/SurrealDB** instance on your NAS.
- **Cross-Device Sync**: Sync sorting plans and library metadata between your Desktop and NAS.
- **RAG Tool**: Extension into a "Chat with your Documents" interface using local vectors (via Ollama or Transformers.js WASM).

### Phase 5: Local LLM Integration (mistral.rs / llama.cpp)
- **Zero Configuration**: Bundle **mistral.rs** or a pre-compiled `llama.cpp` sidecar.
- **Offline UX**: Automatically download small quantized models (like **Ministral in Q4_K_M**) to allow AI sorting without Ollama or Cloud API keys.
- **Model Management**: Dedicated UI for downloading and switching between bundled GGUF models.

## 🛠️ Architecture

- **Frontend**: Svelte 5 (Runes) + Lucide Icons.
- **Backend**: Tauri v2 (Rust) for safe file system operations and CORS-free API calls.
- **Extractors**: `pdfjs-dist` (Legacy Build), `mammoth.js`, `epub-parser`, `Tesseract.js` (OCR).
- **Persistence**: Automatic session saving and resume support.

## 🏗️ Development

```bash
# Install dependencies
npm install

# Run in development mode
npm run tauri dev

# Build for production (.dmg, .exe, .deb)
npm run tauri build
```
