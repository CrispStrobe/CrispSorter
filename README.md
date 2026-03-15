# CrispSorter

CrispSorter is an offline-first document management tool designed to automatically organize messy libraries of PDFs, DOCX, and ebooks. Using local AI or cloud providers, it extracts metadata (Title, Author, Year) and renames/moves files into a clean, structured hierarchy. It prioritizes privacy and performance, supporting local LLM backends like `ollama`, `llama.cpp`, and `mistral.rs` directly on your machine.

Successor to BiblioForge and ZotBiblioForge — no Python, no cloud required.

## Core Workflow

### 1. Extraction Mode
Extract text from PDFs, DOCX, and ebooks without AI. Simply add files, set mode to **Text Only**, and start. Supports **OCR (Tesseract)** for scanned documents.

### 2. AI Sort Mode
Automatically organize files into a clean hierarchy:
- **Ingest**: Drag-drop files or folders (recursive).
- **Analyze**: LLM extracts `Title`, `Author`, and `Year` from document text.
- **Review**: Inline editing in the grid with split-pane text preview.
- **Sort**: Acceptance-based moving of files to `Sorted/Author/Year/Title.ext`.

## Key Features

- **Advanced Extraction**: Supports PDF (Native & JS), DOCX, EPUB, TXT, MD.
- **Visual OCR**: Integrated **Tesseract.js** manager for high-quality visual text recognition on scanned PDFs.
- **Local AI Power**:
    - **Ollama**: Built-in manager to pull and use models (Qwen 3.5, Llama 3.2, etc.).
    - **mistral.rs**: Native GPU acceleration via **CUDA** (Windows) or **Metal** (macOS).
    - **llama.cpp**: Sidecar support for GGUF models.
    - **MLX**: Support for Apple Silicon optimized models.
- **Persistent Chat**: A dedicated AI chat tab that preserves context across your document batch.
- **Organization**: Recursive import, deep duplicate detection (content hashing), and batch move/copy script generation.
- **Modern UI**: Svelte 5 (Runes) powered grid with customizable columns and real-time session saving.

## Development

### Prerequisites
1. **Node.js**: Latest LTS version.
2. **Rust**: Install via [rustup.rs](https://rustup.rs/).
3. **Windows GPU Support (Optional but Recommended)**:
    - **CUDA Toolkit**: Install [NVIDIA CUDA 12.x+](https://developer.nvidia.com/cuda-downloads) for GPU acceleration.
    - **Visual Studio Build Tools**: Install "Desktop development with C++" to enable CUDA kernel compilation.

### Setup & Run (Windows)
We provide optimized scripts to handle environment paths and bypass common toolchain issues:

1. **Configure Environment**:
   ```powershell
   .\paths.ps1
   ```
   *This dynamically detects your MSVC and Rust installations and cleans the PATH.*

2. **Run in Dev Mode**:
   ```powershell
   .\recompile.ps1
   ```
   *Use `.\recompile.ps1 --clean` to perform a fresh build if you change feature flags.*

### Standard Commands
```bash
npm install
npm run tauri dev   # Standard Tauri dev mode
npm run tauri build # Build production installer
```

## Architecture

- **Frontend**: Svelte 5 (Runes) + Lucide Icons + Deep Chat
- **Backend**: Tauri v2 (Rust)
- **AI Engine**: `mistralrs` (with CUDA/Metal), `tauri-plugin-shell` for sidecars.
- **Extractors**: `pdfjs-dist` (with OCR), `pdf-extract` (Native Rust), `mammoth.js`.
- **Persistence**: `tauri-plugin-store` for automatic session recovery.

## License

This project is licensed under the **AGPL-3.0 License**.

---
*Developed by Christian Ströbele. Visit [crispstro.be](https://crispstro.be) for more info.*
