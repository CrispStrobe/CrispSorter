# CrispSorter — Store Listing Copy

All text below is ready for copy-paste. Sections are labelled by store/field.
Character counts are noted where stores impose limits.

---

## APP METADATA

**App Name:** CrispSorter
**Developer / Publisher:** Christian Ströbele
**Website:** https://crispstro.be
**Support Email:** *(fill in)*
**Category:** Productivity / Utilities
**License:** AGPL-3.0
**Platforms:** Windows (x64), macOS (Apple Silicon + Intel)

---

## SHORT DESCRIPTION
*(~80 characters — used in search results, banners)*

> AI-powered document organiser. Rename & sort PDFs and ebooks — fully offline.

---

## MEDIUM DESCRIPTION
*(~250 characters — Microsoft Store "short description" field)*

> CrispSorter automatically extracts Title, Author, and Year from your PDFs, Word files, and ebooks using on-device AI, then renames and moves them into a clean folder structure — no cloud, no subscription, no data leaving your machine.

---

## FULL DESCRIPTION
*(~4 000 characters — Microsoft Store "description" field / App Store main description)*

### Stop drowning in a sea of unnamed PDFs.

CrispSorter is an offline-first, AI-powered document manager that takes a chaotic pile of PDFs, DOCX files, and ebooks and turns it into a neatly organised library — automatically.

Drop in a folder. Pick a local AI model. Let CrispSorter read each document, extract its Title, Author, and Year, and move it to a clean, consistent path like `Sorted / Ströbele / 2024 / My Paper.pdf`. Every step runs entirely on your device.

---

**Works with your files**
- PDF — digital and scanned (built-in OCR via Tesseract)
- DOCX / Word documents
- Plain text and Markdown
- EPUB ebooks

**Runs AI locally — your documents never leave your machine**
Choose from a rich menu of on-device AI engines:
- **Ollama** — the easiest local LLM server; CrispSorter can start it for you
- **mistral.rs** — blazing-fast native inference with CUDA (Windows) or Metal GPU (macOS)
- **llama.cpp** — battle-tested CPU/GPU inference via sidecar
- **MLX** — optimised for Apple Silicon Neural Engine and GPU (macOS only)
- **WebLLM** — runs compact models directly in the app window using WebGPU; no server needed
- **ONNX Runtime / Transformers.js** — browser-based inference with WebGPU or CPU fallback

Prefer cloud speed for non-sensitive documents? CrispSorter also supports Groq, OpenRouter, Mistral, OpenAI, and more — all opt-in, all with your own API key.

**Review before anything moves**
CrispSorter never touches your files without your approval. Every suggested rename appears in an editable grid. Fix a title, correct a year, skip a file — then accept the whole batch with one click. A script export mode lets you review the generated shell/batch commands before running them at all.

**Built for large libraries**
- Drag in an entire folder tree; CrispSorter recurses automatically
- Multi-select and bulk re-analyse rows with different models or settings
- Duplicate detection flags files with identical content
- Sessions auto-save so you can close the app and resume tomorrow
- Full session history lets you revisit any past sorting run

**Ask your documents anything**
A built-in AI chat lets you query across the documents in your current batch — summarise a paper, compare two authors, find a date buried in a scan. Uses whichever AI provider you already have configured.

**Customisable to your workflow**
- Choose your output folder and path template
- Toggle author-based sub-folders on or off
- Tune the LLM prompt for specialist document types (legal, medical, academic)
- Set per-provider models, context length, and temperature
- Export a `.txt` transcript of each document alongside the renamed file
- Full OCR language support: English, German, French, Spanish, Italian, and more

CrispSorter is free, open-source (AGPL-3.0), and built with Tauri + Rust for a small, fast, native binary with no Electron overhead.

---

## KEYWORDS / TAGS
*(comma-separated — use for both stores' keyword fields)*

```
PDF organiser, document management, rename PDF, sort documents, local AI, offline AI, LLM, OCR, metadata extraction, ebook organiser, DOCX, file renaming, document sorter, privacy, on-device AI, Ollama, llama.cpp, academic papers, bibliography, file manager
```

---

## WHAT'S NEW / RELEASE NOTES
*(for first public release)*

> Initial public release of CrispSorter.
>
> - Offline AI document analysis via Ollama, mistral.rs, llama.cpp, MLX, WebLLM, and ONNX Runtime
> - PDF, DOCX, TXT, MD, and EPUB ingestion with optional OCR
> - Editable metadata grid with batch accept / reject
> - Built-in AI chat with document context
> - Session history and auto-save
> - Cloud provider support (Groq, OpenRouter, Mistral, OpenAI)

---

## PRIVACY POLICY
*(required by both stores — paste this at a public URL, e.g. https://crispstro.be/privacy)*

---

**Privacy Policy — CrispSorter**
*Last updated: 2026-03-15*

### 1. Summary
CrispSorter is designed to work entirely on your device. Under normal use with local AI providers, no document content, metadata, or personal data is transmitted to any server operated by the developer.

### 2. Data collected by the application
CrispSorter does not collect, store, or transmit any analytics, telemetry, crash reports, or usage data to the developer or any third party.

### 3. Your documents
Document text is processed locally by the AI engine you select. It is never sent to the developer's servers. If you configure a **cloud AI provider** (Groq, OpenRouter, Mistral, OpenAI, etc.), document excerpts are sent to that provider's API under their respective privacy policies. This is entirely opt-in and requires you to enter your own API key.

### 4. API keys
API keys you enter in Settings are stored locally on your device using the operating system's application data store (Tauri Store). They are never transmitted to the developer.

### 5. Network access
The application makes network requests only in these cases:
- When a cloud AI provider is configured and you trigger an analysis or chat request
- When you explicitly download a model (Ollama pull, GGUF download, MLX model cache, WebLLM model cache) — these requests go to HuggingFace Hub, the Ollama registry, or the MLC model registry

### 6. Local storage
CrispSorter stores the following data locally on your device:
- App settings and preferences
- Current and historical sorting sessions (file paths and extracted metadata)
- Downloaded AI models (in system cache directories)
- Extracted text transcripts (only if the "Save TXT" setting is enabled, written to your chosen export folder)

### 7. Children's privacy
CrispSorter is a productivity tool not directed at children. We do not knowingly collect data from children under 13 (or the applicable age in your jurisdiction).

### 8. Changes to this policy
Any material changes will be noted in the app's release notes and on this page with an updated date.

### 9. Contact
For privacy questions, contact: *(fill in email)*

---

## AGE RATING QUESTIONNAIRE
*(answers for both Microsoft Store and Apple App Store rating tools)*

| Question | Answer |
|---|---|
| Violence | None |
| Sexual content | None |
| Nudity | None |
| Language (profanity) | None |
| Alcohol / tobacco / drugs | None |
| Gambling | None |
| User-generated content shared online | No |
| In-app purchases | No |
| Ads | No |
| Location data | No |

**Resulting ratings:** PEGI 3 / ESRB Everyone / App Store 4+ / Microsoft Store "Everyone"

---

## MICROSOFT STORE — FIELD-BY-FIELD

| Field | Value |
|---|---|
| App name | CrispSorter |
| Short description (≤ 270 chars) | *use Medium Description above* |
| Description (≤ 10 000 chars) | *use Full Description above* |
| Keywords (≤ 7, each ≤ 45 chars) | PDF organiser, document management, local AI, offline AI, OCR, file renaming, ebook organiser |
| Category | Productivity |
| Sub-category | Document management |
| Privacy policy URL | https://crispstro.be/privacy |
| Support URL | https://github.com/CrispStrobe/CrispSorter/issues |
| Website | https://crispstro.be |
| Copyright | © 2024–2026 Christian Ströbele |
| Trademark | — |
| Age rating | Everyone |
| Pricing | Free |

### Screenshots required (Microsoft Store)
- Minimum 1, recommended 4–8
- Size: 1366×768 px minimum, 3840×2160 px maximum; 16:9 preferred
- Format: PNG or JPEG, ≤ 50 MB each
- Suggested shots:
  1. Main batch grid with several PDFs loaded
  2. Settings panel showing LLM provider selection
  3. Chat tab with a document conversation
  4. Session history panel

### Store logo / icon (Microsoft Store)
- 300×300 px (required)
- 150×150 px (required)
- PNG with transparent background

---

## APPLE APP STORE — FIELD-BY-FIELD

*(macOS App Store via App Store Connect)*

| Field | Value |
|---|---|
| App name (≤ 30 chars) | CrispSorter |
| Subtitle (≤ 30 chars) | AI document organiser |
| Description (≤ 4 000 chars) | *use Full Description above* |
| Promotional text (≤ 170 chars) | Sort your PDFs and ebooks instantly with on-device AI. No cloud. No subscription. Runs entirely on your Mac. |
| Keywords (≤ 100 chars total) | PDF,documents,AI,organiser,rename,OCR,ebook,sort,offline,llm,metadata |
| Category | Productivity |
| Secondary category | Utilities |
| Privacy policy URL | https://crispstro.be/privacy |
| Support URL | https://github.com/CrispStrobe/CrispSorter/issues |
| Marketing URL | https://crispstro.be |
| Copyright | © 2024–2026 Christian Ströbele |
| Age rating | 4+ |
| Pricing | Free |

### Screenshots required (macOS App Store)
- At least 1 per supported display size
- Sizes needed: 1280×800, 1440×900, 2560×1600, 2880×1800 (Retina)
- Format: PNG or JPEG
- Suggested shots: same four as Microsoft Store above

### App icon (macOS App Store)
- 1024×1024 px PNG, no alpha channel, no rounded corners (Apple applies the mask)

### Notarization (macOS — required before submission)
```bash
# After building:
xcrun notarytool submit CrispSorter.dmg \
  --apple-id "your@apple.id" \
  --team-id "XXXXXXXXXX" \
  --password "app-specific-password" \
  --wait
xcrun stapler staple CrispSorter.dmg
```
You need:
- Apple Developer Program membership ($99/year)
- An app-specific password from appleid.apple.com
- Your 10-character Team ID from developer.apple.com

---

## GITHUB RELEASES — DESCRIPTION TEMPLATE

```markdown
## CrispSorter vX.Y.Z

AI-powered document organiser — sort PDFs, DOCX files, and ebooks by Title / Author / Year using fully local LLMs.

### Downloads
| Platform | File |
|---|---|
| Windows (x64) installer | `CrispSorter_X.Y.Z_x64-setup.exe` |
| Windows (x64) portable | `CrispSorter.exe` |
| macOS (Apple Silicon) | `CrispSorter_X.Y.Z_aarch64.dmg` |

### What's new
- ...

### Requirements
- **Windows**: Windows 10 / 11, x64
- **macOS**: macOS 12 Monterey or later, Apple Silicon or Intel
- Optional: [Ollama](https://ollama.com) for easy local AI (auto-started by the app)

### First run
1. Install and open CrispSorter
2. Go to **Settings → AI Provider** and choose a backend
3. Drag a folder of PDFs onto the batch area
4. Click **Analyse** and review the suggestions
5. Click **Accept** — done

Full documentation: https://github.com/CrispStrobe/CrispSorter
```
