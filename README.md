# CrispSorter

CrispSorter is an offline-first document management tool designed to automatically organize messy libraries of PDFs, DOCX, and ebooks. Using local AI or cloud providers, it extracts metadata (Title, Author, Year) and renames/moves files into a clean, structured hierarchy. It prioritizes privacy and performance, supporting local LLM backends like llama.cpp and mistral.rs directly on your machine.

Successor to BiblioForge and ZotBiblioForge — no Python, no cloud required.

## Core Workflow

### 1. Extraction Mode
Extract text from PDFs, DOCX, and ebooks without AI. Simply add files, set mode to **Text Only**, and start.

### 2. AI Sort Mode
Automatically organize files into a clean hierarchy:
- **Ingest**: Drag-drop files or folders (recursive).
- **Analyze**: LLM extracts `Title`, `Author`, and `Year` from document text.
- **Review**: Inline editing in the grid with split-pane text preview.
- **Sort**: Acceptance-based moving of files to `Sorted/Author/Year - Title.ext`.

## What's Implemented

- **File Support**: PDF, DOCX, EPUB, TXT, MD extraction; native PDF fallback; Tesseract.js OCR.
- **LLM Integration**: Ollama, OpenAI, Groq, Mistral, Gemini; in-process `mistral.rs`; `llama.cpp` sidecar (Metal).
- **Organization**: Recursive import, duplicate detection (size/SHA-256), batch move/rename.
- **UI/UX**: Svelte 5 grid, inline editing, EN/DE support, session save/resume, column customization.

## Roadmap

See [ROADMAP.md](ROADMAP.md) for planned features.

## Architecture

- **Frontend**: Svelte 5 (Runes) + Lucide Icons
- **Backend**: Tauri v2 (Rust) — file ops, sidecar management, PDF extraction
- **Extractors**: `pdfjs-dist`, `mammoth.js`, `epub-parser`, `Tesseract.js`
- **LLM clients**: Unified abstraction supporting HTTP & local backends
- **Persistence**: `tauri-plugin-store` with automatic session save/resume

## License

This project is licensed under the **AGPL-3.0 License**.

## Development

```bash
npm install
npm run tauri dev
```

### Building for Windows (.exe / .msi)
To build a fully bundled Windows installer with the `llama-server` sidecar:

1. **Install Rust**: Ensure you have the latest stable Rust (MSVC) installed via `rustup`.
2. **Setup Sidecar**:
   - Create `src-tauri/bin/` directory.
   - Download the `llama-server.exe` binary from [llama.cpp releases](https://github.com/ggml-org/llama.cpp/releases).
   - Rename it to `llama-server-x86_64-pc-windows-msvc.exe`.
   - Place all required DLLs (e.g., `llama.dll`, `ggml.dll`, `libomp140.x86_64.dll`, etc.) in `src-tauri/bin/`.
3. **Configure Tauri**:
   - Ensure `tauri.conf.json` includes `bin/*.dll` in the `bundle > resources` section.
4. **Build**:
   ```bash
   npm run tauri build
   ```
   *Artifacts*: `src-tauri/target/release/bundle/nsis/CrispSorter_x.x.x_x64-setup.exe` and `.msi`.

### Sidecar Dependencies (Windows)
The `llama-server` sidecar requires several DLLs from the `llama.cpp` distribution to run correctly within the Tauri bundle. Ensure these are present in `src-tauri/bin/` so they are bundled as resources.

### Building for macOS (.dmg)
```bash
npm run tauri build
# Artifact: src-tauri/target/release/bundle/dmg/CrispSorter_0.1.0_aarch64.dmg
```

#### macOS "Unverified Developer" or "Damaged" Fix
Because this app is currently unsigned (requires a $99/year Apple Developer account), macOS Gatekeeper will flag it. 

1. **"Apple cannot verify..."**: Instead of double-clicking to open for the first time, **Right-Click (Control-Click) the App → Open**. A dialog will appear with an "Open" button; click it to authorize the app.
2. **"Damaged" Error**: If the error persists, you can manually remove the quarantine flag in your terminal:
   ```bash
   sudo xattr -rd com.apple.quarantine /Applications/CrispSorter.app
   ```

### Building for Windows (.exe) on macOS
To cross-compile for Windows from a macOS host, you'll need the MSVC target and `cargo-xwin`.

1. **Install requirements:**
   ```bash
   rustup target add x86_64-pc-windows-msvc
   cargo install cargo-xwin
   ```
2. **Build binary:**
   ```bash
   # Build the frontend first
   npm run build
   # Compile the Windows binary
   cd src-tauri && cargo xwin build --release --target x86_64-pc-windows-msvc
   # Artifact: src-tauri/target/x86_64-pc-windows-msvc/release/tauri-app.exe
   ```
   *Note: Full bundling (MSI/NSIS) is not supported directly on macOS via Tauri CLI; the .exe is produced as a standalone binary.*

#### Build Notes
- **mistralrs**: The `metal` feature is only enabled for macOS targets.
- **Sidecars**: Ensure appropriate architecture-specific binaries for `llama-server` are present in `src-tauri/bin/` if needed for bundling.
