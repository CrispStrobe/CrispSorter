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
| PDF (scanned) | OCR — three tiers: PaddleOCR, ocrs (pure Rust), Tesseract |
| DOCX / Word | mammoth.js |
| EPUB | @lingo-reader/epub-parser (DRM detection via META-INF/encryption.xml) |
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

Groq · OpenRouter · Together · Cerebras · Mistral · OpenAI · Anthropic · Nebius · Scaleway · Poe · Google (Gemini)

API keys you enter in the Settings tab are stored in the **OS keychain** (macOS Keychain / Windows Credential Manager / Linux Secret Service) — never in plaintext on disk. See the [Translate tab](#translate-tab) section below for the migration story.

---

## Features

- **Three-tier OCR** (in quality order):
  - **Tier 3 — PaddleOCR** (`--features paddle-ocr`): multilingual incl. CJK, ONNXRuntime via the existing ort dep, ~60 MB models auto-download; CJK/Latin model selection per document
  - **Tier 2 — ocrs**: pure Rust, zero system install, Latin-script (EN/DE/FR/…)
  - **Tier 1 — Tesseract**: shell-out for users with the system install
- **Batch operations** — multi-select, bulk re-analyse with different models, bulk accept/reject, **content-confirmed duplicate detection** (size → SHA-256), **book-chapter grouping** (ISBN-13 prefix detection — De Gruyter, Brill, Mohr Siebeck, etc.; only the representative file goes through the LLM, metadata propagates to siblings), edited-volume toggle
- **Robust ingest at scale**: 300 s extraction timeout, L2 fallback row when extraction fails (title/author still searchable), automatic DRM detection (EPUB ADEPT/FairPlay), classified failure reasons (timeout / DRM / corrupt / unsupported / password) with retryable-vs-permanent semantics, N-worker parallel pool, retry-on-fail UI button
- **Session persistence** — auto-save and resume; full session history; durable SQLite job queue
- **Built-in AI chat** — query across the documents in your current batch using any configured provider
- **Voice chat (push-to-talk + auto-speak)** — mic button transcribes speech via on-device CrispASR; replies are read back through the platform's native synth (macOS `say`, Windows SAPI, Linux espeak/spd-say). All offline; opt-in.
- **Audio + video transcription** (P13.5, complete) — drop audio or video files into a watched folder and they're indexed as first-class searchable documents, exactly like PDFs.  Supports 22 extensions: WAV / MP3 / M4A / FLAC / OGG / OPUS / AAC for pure audio, MP4 / MOV / MKV / WebM / M4V for video (audio-stream demux only — no video decode), and AVI / WMV / FLV / TS / AMR / RA via an automatic ffmpeg shell-out fallback for the long tail.  Decoder is pure-Rust [`symphonia`](https://github.com/pdeljanov/Symphonia) for tier 1; resampling to the canonical 16 kHz mono Float32 happens via linear interpolation (same approach `whisper.cpp` uses internally) before handoff to CrispASR.
- **CLI ASR + TTS** — `crispsorter chat transcribe <file>` runs any of **24 ASR backends** (`whisper`, `parakeet`, `distil-whisper`, `omniasr`, `qwen3`, `granite`, `voxtral`, …) over any input the audio module can decode.  `crispsorter chat tts "Hello world" --output out.wav` synthesises via **5 TTS backends** (`kokoro`, `qwen3-tts`, `vibevoice-tts`, `orpheus`, `chatterbox`).  All backends are addressable by string + opt-in via `--features crispasr-metal / -cuda / -vulkan`.
- **Audio language identification + backend routing** — `--language auto` detects the input's language via LID (Whisper encoder / Silero / Ecapa / Firered) and routes per `--policy as-configured | strict | auto`.  Example: `--backend parakeet --policy auto --fallback whisper --language ja` transcribes Japanese audio via `whisper` instead of parakeet (which is 25 EU languages only).  Curated per-backend capability table in `asr/lang.rs` mirrors the CrispASR README feature matrix so the routing decision is informed.
- **Index-time text-LID** — every extracted document (PDF, DOCX, EPUB, TXT, audio transcript) optionally goes through a text-LID pass via CrispASR's CLD3 / GlotLID-V3 / LID-176 fastText backends.  Detected ISO 639-1 code lands in the LanceDB `language` column for search-time filtering, faceting, and per-language reranker routing.  Opt-in via `ExtractOptions::text_lid_model`.
- **Cross-document translation** — two surfaces, both wired end-to-end through the GUI:
  - **On-demand**: search-results panel ships a per-row "Translate to …" button + target-language dropdown in the filter row.  Clicking calls the `translate_text` Tauri command; result renders inline below the original snippet with cached / backend badges.  SHA-256-keyed SQLite `translation_cache` makes repeated clicks on the same chunk instant.  Bosnian PDF found via vector search → click "Translate to en" → m2m100 inline.
  - **Index-time batch**: flipped on from `Settings → Search Index → Index-time translation` (dropdown with en/de/fr/es/it/ja/zh) → persisted to `<data_dir>/index_config.json` → next `bg_ingest` pass auto-resolves CLD3 text-LID + runs MT after each extraction, writing into dedicated `text_translated` + `text_translated_lang` LanceDB columns alongside the original `full_text`.  Useful for an English-only corpus that wants foreign-language documents pre-translated and searchable by English keywords without per-query MT overhead.
  - **Search-side query rewrite**: `SearchFilters::prefer_translated_lang = Some("en")` restricts results to rows whose `text_translated_lang` matches, AND swaps the displayed snippet from the original to the translated text — the search-results UI's preview shows the English text that matches the English query, not the source-language original.
  - **4 MT backends**: `m2m100` (default, 100 langs any-to-any), `m2m100-wmt21` (EN↔{zh,de,fr,ja,ru,is,ha} direction-specific), `madlad` (419 langs via target-language prefix), `gemma4-e2b` (dual ASR+MT).
- **Multimodal Settings + L1/L2/L3 ingest depth** (P13.6 + P13.7, complete) — `Settings → Search Index → Multimodal processing` exposes (a) a master switch + ASR backend dropdown (whisper / whisper-large-v3 / whisper-small / whisper-medium / parakeet / qwen3-omni) + LID method + audio ingest depth (L1=filesystem only / L2=symphonia probe / L3=full transcription, default L3); (b) the parallel image controls — master switch + image ingest depth (L1 / L2=EXIF / L3=EXIF+OCR); (c) opt-in CrispLens image-push.  Audio L2 metadata (`duration / codec / sample_rate / channels / bitrate_kbps`) and image L2 EXIF (`camera_make / camera_model / lens_model / taken_at_unix / iso`) land in dedicated LanceDB columns via schema migrations v101 + v102.  Per-row promotion via "Transcribe" (audio) / "Re-OCR" (image) buttons on search results that re-ingest a specific row through the L3 pipeline regardless of the global level.  Drop zones in both Stapel + Kataloge accept all 22 audio/video extensions and surface a "Transcribing" status badge while whisper runs.
- **CLI search with cloud-backup-parity filters** (P13.7) — `crispsorter index search "query"` accepts `--ext pdf,docx --hash a1b2c3 --folder-prefix /path --lang de --translated-to en --year-min 2020 --year-max 2025 --min-size 100KB --max-size 50MB --after 2023-01-01 --before 2025-06-01 --audio-duration-min 60 --audio-duration-max 1800 --image-camera-make Apple --image-camera-model 'iPhone 15 Pro' --limit 50 -f table|json`.  Pushes ext / hash / folder / language / year / audio-duration / image-camera filters into LanceDB scalar SQL; size + date filters post-hoc on `metadata_json.fs_size` / `fs_mtime` (promote to scalar columns is a tracked follow-up).  Mirrors `../cloud-backup`'s `search.py` flag set so users moving between the two tools don't relearn the surface.
- **CrispLens image push** (P13.7, opt-in) — `images_crisplens_image_push(path, visibility?)` Tauri command + Settings → Multimodal toggle.  Two-phase: GET `/api/images/by-hash/{sha256}` for dedup, multipart POST `/api/ingest/upload-local` on miss.  Server runs face detection + ArcFace embeddings + (optional) VLM description, stores in its own SQLite + `uploads/` tree.  Privacy-aware default: off until you opt in.
- **Schema-migration framework** — versioned `Migration` async trait with SQLite ledger at `<data-dir>/.crispsorter_migrations.db`.  Gap detection, duplicate-version rejection, downgrade guard (ledger says vN applied but no matching migration registered → refuse to proceed), failure isolation (mid-run failure leaves the ledger consistent for resume).  Three real consumers landed: `AddTextTranslatedColumns` (v100, P13.5), `AddAudioMetadataColumns` (v101, P13.6), `AddImageMetadataColumns` (v102, P13.7).
- **Folder watcher** — watch one or more folders; new files dropped in get auto-added to the batch (no auto-move — you still review and press Start)
- **PDF metadata pre-fill** — read Title / Author / Year from a PDF's `/Info` dict and XMP packet before the LLM runs
- **BibTeX export** — generate a `.bib` file from sorted batch metadata; LaTeX-escaped, deduplicated citation keys
- **Script export** — generate a `.bat` / `.sh` script to review moves before executing them
- **JSON sort plans** — `batch process` → `batch apply` pipeline produces a structured plan you can audit before applying
- **Customisable output** — `{Author}/{Year}/{Title}` template configurable in Settings, save extracted `.txt` transcript alongside files
- **Editable grid** — column visibility, width, sort; inline field editing; metadata edits immediately update the sort destination path
- **Search index** — optional semantic + full-text search over all sorted documents (local, remote, or hybrid), with optional cross-encoder reranking, sparse retrieval (BGE-M3/SPLADE), and Matryoshka dim truncation
- **Mountable archive index (`.cidx`)** — export a per-volume slice of the search index as a portable directory (LanceDB + optional Tantivy FTS companion). Ship the archive drive + `.cidx` in the same backup snapshot; CrispSorter mounts it as a read-only "Archiv" tab and full-text search works offline.
- **cloud-backup integration** ([`../cloud-backup`](https://github.com/CrispStrobe/cloud-backup)): import 482k+ files as L1 metadata in seconds (`index_ingest_cb_manifest`), promote individual files to L3 on demand via `retrieve.py`, reverse-lookup tier availability (Lokal/VPS) in the preview pane, opt-in VPS-side indexing trigger
- **cloud-backup HTTP sync** (P13.7 Step 5, complete) — incremental manifest sync over HTTP against the cloud-backup VPS without rsync'ing the SQLite DB.  Adds a small FastAPI module (`../cloud-backup/api/app.py`, deployed via the new `cb-api.service` systemd unit) that exposes `POST /api/manifest/push`, `GET /api/manifest/pull`, `POST /api/index/push-embeddings`, `GET /api/index/by-embedding` against the same `<catalog-db>` that `vps_worker.py` writes to today.  Bearer auth via a new `api_keys` table (bcrypt-hashed; mint via `python -m api.admin mint <NAME>` on the VPS).  Default per-owner scoping (`CRISP_CB_SHARED_OWNERS=1` env flips into shared-catalog mode).  Client side: `crispsorter sync cloud-backup {status,push-manifest,pull,login,logout}` CLI + Settings → Cloud-backup sync panel.  Token stored in OS keychain under `CrispSorter.CloudBackup`, never in `index_config.json`.  Live-verified against the production VPS (3 env-gated `cb_sync_live_*` tests pass; default test run skips them when `CB_SYNC_TEST_URL`/`CB_SYNC_TEST_API_KEY` env are absent).
- **cloud-backup TB-scale tier split** (cb-api Stage W, 2026-05-28) — the remote `cb-api` backend now keeps the SQLite catalog at metadata-only size and routes body text (`full_text`) into per-shard LanceDB on attached CIFS storage.  Toggled per-deployment via `CB_BODY_BACKEND=lance` (default `sqlite` for back-compat).  The CrispSorter wire client (`cloud_backup.rs::ManifestRow` / `ManifestPullResponse` / `SearchHit`) works against both backends with **zero protocol changes** — verified end-to-end against the production VPS via `crispsorter sync cloud-backup pull --include-full-text` returning the same body bytes that lived in SQLite before the migration.  Production proof point: the 50 753-row wallabag corpus migration shrank the catalog from 1.1 GB to 349 MB; extrapolated to 5 TB of PDFs the catalog stays around 3 GB on the Hetzner block volume.
- **Cloud drives** — register **WebDAV** (Nextcloud / ownCloud / mailbox.org / Synology / `filen webdav-start` / `internxt webdav-enable`), **Filen**, **Internxt**, or any local/OS-mounted path as a drive (Quellen → Cloud-Ordner → Anlegen). Manifest-only L1 ingest of any subtree (no bandwidth cost beyond directory listings); per-row "Promote to L3" downloads + indexes a file's contents on demand via the existing extract+embed pipeline. `crisp+drive://<id>/<remote-path>` URI scheme keeps the same row resolvable across drive renames/edits.
- **Document translation (Translate tab)** — end-to-end `.docx` → `.docx` translation via the [`crisp-docx`](https://github.com/CrispStrobe/crisp-docx) workspace. Pick an input docx, a target language, and an LLM provider; the Tauri command streams paragraph-by-paragraph translation progress to the UI. Paragraph styles, sections, bookmarks, and footnote references are preserved in v0.1; intra-paragraph bold/italic span preservation (v0.2) is gated behind a `--features translate-align` build that pulls in CrispEmbed for SimAlign-driven word alignment. **Offline NMT** (CrispASR's m2m100 / wmt21 / madlad / gemma4-e2b GGUF models) is supported as a `Nmt` provider option for zero-network translation. **OS-keychain credential storage**: when settings.json holds a plain-text apiKey from earlier versions, a one-time migration moves it into the OS-native credential vault under the service `CrispSorter.LLM` and replaces the JSON entry with a `@keyring/llm-provider:<id>` sentinel. New keys typed into the form migrate the same way on save.
- **Photos / Bilder vertical** (P13, complete) — dedicated "Bilder" tab in Übersicht with image-row filtering (jpg/jpeg/png/webp/heic/heif/tiff/bmp), lazy-loaded thumbnails via IntersectionObserver, click-to-open preview pane with a curated EXIF metadata table (camera, lens, aperture/ISO/exposure, GPS, taken-at), SHA-256 byte-identical dup grouping, and perceptual-hash near-duplicate grouping for resize / re-encode catches. **Optional CrispLens Tier 2** connector (`Settings → CrispLens`) — backend dropdown + URL + login, with the session cookie stored in the OS-native keychain (Keychain on macOS, secret-service on Linux, Credential Manager on Windows; never in the JSON settings file). Live health-monitor banner (offline / session-expired / warming-up / ok), open-in-CrispLens deep-link from the preview pane, watchfolder cross-reference hint when an image's folder is also watched server-side, People view listing face-recognition clusters, per-image faces query, and remote text search (`/api/search` — filename / person-name substring; true semantic search depends on a future CrispLens upstream endpoint). Full CLI parity: `crispsorter images {extensions,count,list,thumbnail,exif,duplicates,near-duplicates,crisplens}` with `crisplens` covering the full Tier 2 surface.

---

## Headless CLI mode

The same binary doubles as a CLI tool. Detection is on the first argument — running `crispsorter` with no args (the typical GUI launch) bypasses clap entirely.

```bash
crispsorter version
crispsorter doctor                                     # OCR engines, embedder cache, etc.
crispsorter index init --model bge-m3 --device metal   # download embedder weights
crispsorter index ingest /path/to/docs                 # full extraction + embedding pipeline
crispsorter index stats                                # docs / chunks / fts-docs counts
crispsorter index search "karl barth"                  # BM25 FTS

# Richer search (P13.7) — cloud-backup parity filter set
crispsorter index search "klimaschutz" \
    --ext pdf,docx --lang de --year-min 2020 \
    --folder-prefix /Users/foo/papers \
    --min-size 100KB --max-size 50MB \
    --after 2023-01-01 \
    --limit 30 -f table
crispsorter index search "podcast" \
    --ext mp3,wav,m4a --audio-duration-min 600 --audio-duration-max 3600 \
    --lang en --limit 20
crispsorter index search "berlin" \
    --ext jpg,png --image-camera-make Apple --after 2024-01-01 -f json
crispsorter index list-failed --retryable-only
crispsorter index retry-failed [--dry-run]
crispsorter index export-cidx my-archive.cidx --include-fts
crispsorter index inspect-cidx my-archive.cidx
crispsorter index ingest-cb-manifest cloud-backup.db   # bulk import 482 k file metadata

crispsorter batch add ~/Downloads/papers/              # enqueue for the GUI
crispsorter batch list
crispsorter batch process --llm-url http://localhost:11434/v1 --llm-model llama3 \
                          --path-template '{Author}/{Year}/{Title}' \
                          --out-plan plan.json         # → JSON sort plan
crispsorter batch apply plan.json                      # execute the plan

crispsorter chat query "Was ist die Hauptthese?" --context-files paper.pdf

# Audio + video transcription (P13.5 — needs --features crispasr-metal / -cuda / -vulkan)
crispsorter chat transcribe interview.mp3                         # whisper, plain-text to stdout
crispsorter chat transcribe interview.mp3 -f json -o out.json     # JSON envelope with decode metadata
crispsorter chat transcribe long-recording.wav --stream            # partials to stderr as Whisper commits
                                                                   # rolling windows (step=3000ms /
                                                                   # length=10000ms / keep=200ms);
                                                                   # final transcript still routes to -o.
crispsorter chat transcribe ja-podcast.m4a \
    --backend parakeet --policy auto --fallback whisper \
    --language auto --lid-model ~/models/cld3-f16.gguf
# → LID detects ja, parakeet doesn't speak Japanese, routes to whisper automatically.
# JSON output carries: detected_language, confidence, decision, used_backend.

crispsorter chat transcribe bosnian-interview.wav \
    --language bs --translate-to en --translate-backend m2m100
# → transcribe via whisper (Bosnian is in whisper's 99 langs), then translate
#   transcript bs → en via m2m100.  JSON output carries the original + translation.

# TTS — synthesise to WAV via CrispASR TTS backends
crispsorter chat tts "Hello, world." --output /tmp/hello.wav
crispsorter chat tts "Hallo Welt." --backend orpheus --speaker Anton --output /tmp/de.wav
crispsorter chat tts "..." --backend qwen3-tts --voice ~/voices/sample.wav \
    --voice-ref-text "..." --output /tmp/cloned.wav

crispsorter catalog scan ~/Volumes/Backup --hash sha256 --out backup.caf
crispsorter catalog find-dupes Backup1.caf Backup2.caf --strategy hash:sha256

# Photos / Bilder vertical (P13)
crispsorter images extensions                          # print canonical IMAGE_EXTS
crispsorter images count                               # image-row count in the local index
crispsorter images list --limit 20                     # newest-first photo rows
crispsorter images thumbnail /tmp/x.jpg --size 256 --out /tmp/x.png
crispsorter images exif /tmp/x.jpg                     # curated EXIF (json/text)
crispsorter images duplicates                          # SHA-256 dup clusters
crispsorter images near-duplicates --threshold 8       # pHash near-dup clusters

# Tier 2 — CrispLens connector
crispsorter images crisplens set-url https://crisplens.example.com --enable
CRISPLENS_PASSWORD=… crispsorter images crisplens login --user alice
crispsorter images crisplens session-status            # boolean — never leaks the cookie
crispsorter images crisplens status                    # health + auth (4-state machine)
crispsorter images crisplens logout                    # POSTs /api/auth/logout + wipes keychain
crispsorter images crisplens watchfolders              # list folders the server is watching
crispsorter images crisplens people                    # face-recognition person clusters
crispsorter images crisplens image-faces 201           # face crops on one image
crispsorter images crisplens search 'Christian'        # filename / person-name text search

crispsorter completion zsh > ~/.zsh/completions/_crispsorter
crispsorter manpage --out /usr/share/man/man1/
```

The catalog primitives also ship as a tiny standalone binary (`crispcat`) — `cargo install --path crates/crispcat-cli`. No Tauri, no LanceDB, no embedder; just `.caf` I/O, parallel scanner, duplicate engine.

---

## Runtime modes

CrispSorter has three runtime modes (Settings → Search Index → Backend):

| Mode | Reads | Writes | Use case |
|---|---|---|---|
| **Standalone** (`local`) | local LanceDB + Tantivy | local | single machine, fully offline (default) |
| **Server** (`remote`) | self-hosted `crisp-index-server` | remote via HTTP | index lives on a VPS or GPU box |
| **Hybrid** (`hybrid`) | local-first cache | local + mirror to remote outbox | laptop ↔ VPS, offline-capable |

In Hybrid mode, writes go to the local cache and queue to a SQLite **sync outbox** (`sync_outbox.db`). A background worker drains the outbox to the remote server when it's reachable. The nav sidebar shows a `⇅ N` chip indicating pending count + online state; clicking it triggers an immediate push.

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

### Build artifacts location (optional, recommended for low-disk macOS / Linux)

A full Tauri build of this workspace can grow `target/` to **20–25 GB**.
On a 460 GB MacBook Pro that fills the boot disk fast, especially with
sibling repos (`fastembed-rs`, `CrispEmbed`, …) all building locally.

The recommended setup keeps every Rust project's build artifacts on an
external volume, isolated per-repo, via a tiny zsh wrapper around
`cargo`.  Add to `~/.zshenv` (so it's picked up by both interactive
shells and scripts):

```sh
cargo() {
  local root
  root="$(git rev-parse --show-toplevel 2>/dev/null)"
  if [[ -n "$root" && -z "$CARGO_TARGET_DIR" && -d <external-volume> ]]; then
    CARGO_TARGET_DIR="<external-volume>/code/cargo-target/$(basename "$root")" \
      command cargo "$@"
  else
    command cargo "$@"
  fi
}
```

Each repo's compiled artifacts land at
`<external-volume>/code/cargo-target/<reponame>/`.  The wrapper falls back
to the default `./target/` when not in a git repo, when
`CARGO_TARGET_DIR` is already set (one-off overrides win), or when the
external volume isn't mounted — so it's safe to leave on for any machine.

Per-repo subdirs (instead of one shared `target-dir`) keep `cargo clean`
scoped to the current repo and avoid feature-flag thrash between projects.

Adapt the path to your own external volume, or drop the `-d <external-volume>`
guard if you'd rather always redirect.

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
| Speech-to-text | [CrispASR](https://github.com/CrispStrobe/CrispASR) — optional sibling crate; 24 ASR backends (Whisper/Parakeet/Qwen3/Granite/Voxtral/Canary/Cohere/OmniASR/…) addressable by string |
| Text-to-speech (CLI / index) | [CrispASR](https://github.com/CrispStrobe/CrispASR) — 5 TTS backends (Kokoro/Qwen3-TTS/VibeVoice-TTS/Orpheus/Chatterbox), 24 kHz mono Float32 output via the `audio::writer` |
| Text-to-speech (GUI auto-speak) | Native platform synth — `say` (macOS), SAPI (Windows), `spd-say`/`espeak` (Linux) |
| Audio + video decode | [`symphonia`](https://github.com/pdeljanov/Symphonia) tier 1 (WAV/MP3/M4A/FLAC/OGG/OPUS/AAC, MP4/MOV/MKV/WebM/M4V demux), `ffmpeg` shell-out tier 2 for the AVI/WMV/FLV/TS/AMR long tail |
| Language ID | [CrispASR](https://github.com/CrispStrobe/CrispASR) — audio LID (Whisper encoder / Silero / Ecapa / Firered) and text LID (CLD3 / GlotLID-V3 / LID-176 fastText, routed by GGUF `general.architecture`) |
| Translation | [CrispASR](https://github.com/CrispStrobe/CrispASR) — 4 MT backends (M2M-100 / WMT21-dense / MADLAD-400 / Gemma4-E2B); on-demand Tauri command + index-time batch column |
| Schema migrations | In-tree `crate::migrations` framework — async `Migration` trait, SQLite version ledger at `<data-dir>/.crispsorter_migrations.db` |
| Vector store (local) | LanceDB (embedded) |
| Full-text (local) | Tantivy (with ASCII-folding for German umlaut search) |
| Folder watcher | `notify` (FSEvents/inotify/ReadDirectoryChangesW) |
| PDF metadata | `lopdf` (/Info dict) + `quick-xml` (XMP packet) |
| Search server | crisp-index-server (axum + LanceDB + Tantivy) |
| Catalog primitives | `crispcat` workspace crate (extracted from `src-tauri/src/catalog/`); standalone CLI in `crates/crispcat-cli` |
| OCR Tier 3 | PaddleOCR DB + SVTR via [`usls`](https://crates.io/crates/usls) (ONNXRuntime); CJK + Latin recognition models |
| Wire types | `crisp-index-protocol` workspace crate — single source of truth for `IngestChunk` / `SearchRequest` / `SearchHit` shapes |

---

## Testing

```bash
# Fast unit tests (no network, no model download)
cargo test -p crispsorter --lib                    # ~200 unit tests across the desktop app
cargo test -p crispcat                           # ~20 unit tests in the catalog library

# Standalone CLI integration tests (compile + spawn `crispcat` binary)
cargo test -p crispcat-cli                       # 8 e2e tests on real .caf files

# Full Tauri-binary smoke tests (require ~30 GB free disk for the build).
# Each test spawns the actual `crispsorter` binary and exercises a real subcommand.
cargo test -p crispsorter --test cli_smoke -- --ignored

# Heavy: full ingest → search → delete e2e.
# Downloads ~90 MB of all-MiniLM-L6-v2 ONNX weights from HuggingFace on first run.
cargo test -p crispsorter --test cli_e2e_embedder -- --ignored
```

The unit-test sweep (`cargo test --workspace`) covers cross-cutting components:
URI round-trips (incl. `crisp+cb-archive://`), failure-reason classification,
EPUB DRM detection (real zip fixtures), background-ingest state machine, OCR
tier dispatch, runtime-mode serde, sync outbox lifecycle, drive registry,
FTS query parser, embedder backend selection, full `.caf` v6/v7/v8 round-trip,
and CrispEmbed GGUF metadata.

The integration tests (`--ignored`) use real files: the `crispcat-cli`
suite scans real folder trees and validates SHA-256 deduplication; the
`cli_smoke` suite exercises `version` / `doctor` / `catalog scan|browse|find-dupes` /
`batch add|list|apply` / `index list-failed|stats` / `completion` / `manpage`;
the `cli_e2e_embedder` suite downloads a small embedder, ingests three text
files, runs BM25 search, exports a `.cidx` archive with the FTS companion,
and verifies `inspect-cidx` reports the right counts.

> **Tip for laptops with tight boot disks:** point Cargo at an external
> volume to keep the build artifacts off `/`:
> ```bash
> CARGO_TARGET_DIR=/Volumes/External/cargo-target/crispsorter \
> cargo test --workspace
> ```

---

## License

**AGPL-3.0** — see [LICENSE](LICENSE).

