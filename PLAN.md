# CrispSorter — Development Plan

> **Full specs for completed phases** → [HISTORY.md](HISTORY.md)
> **Technical patterns / pitfalls** → [LEARNINGS.md](LEARNINGS.md)
> **In-flight integration designs** → [docs/](docs/)

---

## Capabilities (shipped)

- LanceDB + Tantivy hybrid search, RRF fusion, sparse BGE-M3/SPLADE channel
- ONNX/CoreML + CrispEmbed GGUF backends, 36-model registry
- Batch AI sort (Stapel): extraction → LLM metadata → sort-path → move/copy/script
- P6 Catalog: `.caf` I/O, parallel scanner, duplicate engine, Übersicht columnar browse
- P7 Desktop search parity: folder tree, million-row pagination, preview pane, bg ingest
- P8 CLI: `version / doctor / catalog / index / batch / chat / completion / manpage`
- P9 Übersicht scale: DB-side ORDER BY (lance::Scanner), scalar indexes, volume filter
- P10 Robust ingest: TaskFailureReason, 300 s timeout, L2 fallback, DRM detection, skip-failed CLI
- P11 Remote server: `crisp-index-server` (Axum + LanceDB + Tantivy), durable job queue, server-side embedding
- P11 Cloud drives: `LocalDrive` + `InternxtDrive` + `FilenDrive` + `WebDavDrive` (live-verified against both Filen + Internxt local WebDAV servers); registry with create/edit/delete UI; `crisp+drive://` URIs; manifest-only L1 ingest + on-demand L3 promote
- P11 SyncManager: pull-apply loop closed (writes pulled rows as L1 metadata in local LanceDB)
- P12 cloud-backup: L1 manifest import (`source_files` → LanceDB), L3 via `retrieve.py`, reverse lookup, VPS-trigger indexing
- P15 Batch pre-processing: content-dedup (SHA-256), book-chapter grouping (ISBN-13)
- OCR: Tier 1 Tesseract, Tier 2 ocrs, Tier 3 PaddleOCR (`--features paddle-ocr`)
- `.cidx` offline archives: LanceDB + Tantivy FTS export/mount, Archiv tab in Übersicht, background-promote per row
- `crisp+cb-archive://` URI scheme for cloud-backup archived files
- `crisp+drive://` URI scheme for any registered CloudDrive (Local / Filen / Internxt / WebDAV)
- macOS arm64 packaging: `scripts/bundle_macos_native_libs.sh` co-bundles `libcrispasr.dylib` + `libcrispembed.dylib` + ggml backends + homebrew transitives into `.app/Contents/Frameworks/` with rewritten LC_RPATH entries

For per-feature deep-dives, see [HISTORY.md → "Phase ship index"](HISTORY.md).

---

## In Progress

**P13 Bilder vertical** — both tiers complete (A1–A4 + B1–B5) +
all follow-ups landed: by-hash resolver (`/api/images/by-hash/...`),
semantic search wired through to `/api/search/semantic` (cross-
lingual DE↔EN via `paraphrase-multilingual-MiniLM-L12-v2`, live-
verified against `https://<crisplens-host>` on 2026-05-12), and
image-overlay face boxes plumbed through the CrispSorter preview
pane.  The CrispLens-side deploy uses `CRISPEMBED_REINSTALL=1`
on `fix_db.sh` when CrispEmbed cuts a registry-changing release.

**Test coverage:** 311 unit tests pass in `tauri-app` (+2 `#[ignore]`'d
WebDAV-live integration tests gated by
`WEBDAV_TEST_URL`/`USER`/`PASS`), 20 in `crispcat`, 29 in
`crisplens-protocol`, 5 in `crisp-index-protocol` = **365 passing**.
Run with `cargo test --workspace --lib`.

---

## Open TODOs

Only `[ ]` items live here.  Shipped items are in HISTORY.md.

### P3.5 — CrispEmbed / CrispASR bundling

- [x] Phase 1 — macOS arm64 (see HISTORY.md)
- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session)
      RPATH / DLL colocation; each platform needs 1-2 release iterations.
      Opening prompt: `handover-prompts/session-prompt-crispembed-ci-matrix.md`
      (local-only — see .gitignore).
- [ ] **Phase 3 — mobile** (deferred)

### P5 — Future / planned

- [ ] **Auto-process toggle on watch detection** — risky, needs UX
      design pass before any code
- [ ] **PWA demo via File System Access API** — speculative

### P7.8 — OCR Tier 3 polish + Tier 4

- [ ] **SLANet table extraction** on top of Tier 3 PaddleOCR — adds
      structured table output for invoices / bank statements / grids.
      The `usls` crate already hosts a SLANet model.  ~3-5 h.
- [ ] **Tier 4 — VLM OCR** (~1 wk) — `deepseek-ocr.rs`-style via
      Candle (not ort).  DeepSeek-OCR / PaddleOCR-VL, Q4_K–Q8_0
      quantisation, 4.7-9 GB models, macOS Metal target.

### P8.2 — CLI polish remaining

- [ ] **`cargo install crispsorter`** for the Tauri-app binary — needs
      binstall recipe + signing (macOS Developer ID, Windows
      Authenticode).  `cargo install --path crates/crispcat-cli` already
      ships.  ~2-4 h once a signing identity is in hand.

### P13.5 — Audio + video vertical (in flight)

> **Scope axis 1 — backends:** all **24 ASR + 5 TTS + 3 translation +
> 3 LID-capable** backends from the CrispASR registry are first-class.
> ASR: `whisper`, `parakeet`, `canary`, `qwen3`, `distil-whisper`,
> `cohere`, `granite{,-4.1,-4.1-plus,-4.1-nar}`, `fastconformer-ctc`,
> `voxtral{,4b}`, `wav2vec2`, `glm-asr`, `kyutai-stt`, `firered-asr`,
> `moonshine{,-streaming}`, `omniasr{,-llm}`, `vibevoice`, `gemma4-e2b`,
> `mimo-asr`.  TTS: `kokoro`, `qwen3-tts`, `vibevoice-tts`, `orpheus`,
> `chatterbox`.  Translation: `m2m100`, `m2m100-wmt21`, `madlad`.
> LID: whisper-encoder, Silero-95, GlotLID/CLD3/LID-176 (text).
> No backend hard-coded as the only option anywhere — every surface
> takes a backend string that resolves via `crispasr::registry_lookup`.

> **Scope axis 2 — input media:** WAV / MP3 / M4A / FLAC / OGG / OPUS
> / AAC for pure audio; MP4 / MOV / MKV / WebM / M4V for video (we
> demux the audio stream — no video decode).  Long-tail containers
> (.avi DivX, .wmv, .flv, .ts, .amr, .ra) via ffmpeg shell-out only
> when symphonia can't read the file natively.

> **Scope axis 3 — language handling:**
> - **LID** before transcription: detect the audio's language via
>   `crispasr::detect_language_pcm` (whisper-encoder or Silero) so
>   we don't send German audio to an English-only model.
> - **Backend capability table** in `asr/lang.rs` — per-backend
>   supported-language list curated from the README feature matrix
>   (RegistryEntry doesn't expose languages today; an upstream
>   addition would let us drop the curation).
> - **Routing policy** on language mismatch: `Strict` (error),
>   `Auto` (switch to a backend that supports the detected language,
>   default), `Ignore` (proceed anyway, last resort).
> - **Translation post-processing** for ASR output and stored
>   extractions: optional `--translate-to <code>` runs m2m100 /
>   m2m100-wmt21 / madlad / gemma4-e2b after transcription.
>   Available in `chat transcribe`, in the extractor (extracted
>   documents from any source — PDF / DOCX / EPUB / TXT / audio
>   transcript — can be translated for indexing, useful for an
>   English-only corpus that wants foreign-language documents to be
>   searchable), and on-demand from the search-results UI (user
>   finds a hit in a Bosnian PDF via vector search, clicks "translate
>   to en", we render the translated chunk inline alongside the
>   original — no LanceDB rewrite needed).  WMT21-dense is direction-
>   specific (EN ↔ {zh, de, fr, ja, ru, is, ha}); long-tail
>   languages like Bosnian go through m2m100 (100 langs, any-to-any)
>   or madlad-400 (419 langs).  CrispASR has the C++
>   `crispasr_session_translate_text` dispatcher (M2M-100, WMT21,
>   MADLAD, Gemma4-E2B) — but **the C-ABI symbol isn't wrapped in
>   `crispasr-sys` or the safe `crispasr` Rust crate today** (the
>   Rust `set_translate(true)` method is the audio-side Whisper
>   sticky flag, not the text-to-text dispatcher).  Bringing it to
>   CrispSorter needs an upstream change, exactly analogous to the
>   text-LID one: add the `extern "C"` decl to crispasr-sys, expose
>   a safe `Session::translate_text(text, src, tgt, max_tokens)`
>   wrapper.  Tracked as **Phase 8** (after text-LID lands so
>   detection feeds translation).
> - **Text-LID for all extracted documents** (PDF / DOCX / TXT /
>   transcript): tag every document with its detected language at
>   index time so the UI can filter / facet by language and so we
>   can pick a per-language reranker.  CrispASR has the C++
>   `text_lid_dispatch` (CLD3 ~1.5 MB, GlotLID-V3 ~2102 langs,
>   LID-176 ~176 langs) — but **none are exposed through the Rust
>   crate or `crispasr-sys` FFI today** (only the C++ CLI uses
>   them via `--lid-on-transcript`).  Bringing them to CrispSorter
>   needs an upstream CrispASR change first: add `crispasr_text_lid_*`
>   C-ABI exports, mirror in `crispasr-sys`, surface in the safe
>   wrapper.  Tracked as **Phase 7** (after the audio + translation
>   foundation lands).

**Speed-tier defaults** (called out so we don't default to slow models
for batch jobs): `parakeet-tdt-0.6b-v3` (25 EU langs, FastConformer
+ TDT — much faster than whisper), `distil-whisper/distil-large-v3`
(6.3× faster than whisper, EN only), `moonshine-{tiny,base}` (34–
245 M, designed for streaming), `omniasr` (CTC, 1600+ langs).
Slow-but-quality fallback: `whisper-base` (default for GUI push-
to-talk for back-compat with shipped releases).

**Streaming** is universal — every backend in the feature matrix
carries the Streaming cap.  `crispasr::Session::stream_open()` →
`Stream::feed(pcm)` + `Stream::get_text()` + `Stream::flush()` is
the call shape.  Used by slice B for long-form files (bounds peak
memory, emits per-chunk progress) and stays available for slice A's
`--stream` flag (live captions when reading from stdin).

**Phased delivery (each phase commits separately so progress is
visible and reversible):**

1. **Phase 0** — PLAN consolidation + Cargo.toml dep additions
   (hound, symphonia, rubato) + AsrConfig refactor (enum-of-one → 
   string-based, language-aware).
2. **Phase 1** — `src-tauri/src/audio/` module: symphonia decoder
   + ffmpeg fallback + rubato resampler.  Tests + smoke example.
3. **Phase 2** — `src-tauri/src/asr/lang.rs`: LID wrapper + curated
   backend-capability table + routing policy enum.  Tests.
4. **Phase 3** — Slice A: CLI `chat transcribe` + `chat tts` end-to-
   end with LID + translation flags.  Tests + life example.
5. **Phase 4** — Slice B: `extractors/audio.rs` for index-time
   transcription, wired into the existing extractor dispatch.
   Tests + life example (audio file becomes searchable).
6. **Phase 5** — Translation post-processing wrapper consumed by
   both surfaces.  Tests + life example (DE transcript → EN search).
7. **Phase 6** — Audio-LID routing applied: detect language before
   transcription, switch backend on mismatch per `BackendFallback`
   policy.  Tests + life example (DE audio with English-only
   parakeet-EN config → auto-falls-back to whisper).
8. **Phase 7** *(blocked on CrispASR upstream)* — Text-LID for all
   extracted documents: requires `crispasr_text_lid_*` C-ABI
   exports in CrispASR + crispasr-sys + safe wrapper.  Then
   `extractors/mod.rs` runs LID on every `ExtractedDocument`,
   stores the language code as a new LanceDB column, exposes a
   language-filter facet in the search UI.
9. **Phase 8** *(blocked on CrispASR upstream)* — Cross-document
   text translation, both index-time-batch and on-demand-per-hit.
   Builds on Phase 7's text-LID (need to know the source language
   before translating).  Upstream work: add `extern "C" fn
   crispasr_session_translate_text(*mut CrispasrSession,
   *const c_char, *const c_char, *const c_char, c_int) -> *mut
   c_char` to `crispasr-sys` (mirroring how text-LID's exports get
   added in Phase 7), then expose a safe `Session::translate_text(
   text: &str, src: &str, tgt: &str, max_tokens: i32) -> Result<
   String>` in the high-level crate that handles the malloc'd
   UTF-8 return + free.  CrispSorter side: new
   `src-tauri/src/asr/text_translate.rs` module (sibling of
   `lang.rs`) that picks the right backend per language pair
   (m2m100 default, wmt21 for EN-paired-with-the-7 supported
   langs, madlad for the 400-language long tail), with a per-pair
   capability check.  Two consumer surfaces:
   - **Index-time batch:** new extractor config flag
     `translate_to: Option<String>` on every extractor; when set,
     run text-LID → translation → write the translated text into a
     dedicated LanceDB column (`text_translated_<tgt>`) alongside
     the original.  Search defaults to querying the translated
     column when set, falling back to the original.
   - **On-demand:** new Tauri command `translate_document_text(
     chunk_id: String, target_lang: String) -> Result<String>` that
     looks up the chunk's original text, calls the translation
     wrapper inline, and returns the result.  Search-results UI
     adds a per-result "Translate to …" affordance; the SvelteKit
     side caches translations in component state so repeated
     clicks on the same chunk don't re-run the model.  Optional
     persistent cache: a side SQLite table keyed by
     `(chunk_id, target_lang)` for cross-session reuse.

   Tests + life example: a Bosnian PDF gets ingested into an
   English-only corpus, the user queries "kako se zoveš" through
   the vector index, the hit's UI surface offers "Translate to
   en" and renders the M2M-100 output inline.

Three distinct surfaces, only #1 ships today.

1. **GUI chat push-to-talk** (shipped — currently Whisper-only via
   `AsrModel::Whisper` enum-of-one in `src-tauri/src/asr/mod.rs`).
   **Refactor in flight:** replace the enum with `AsrConfig { backend:
   String, model_path: Option<String> }` so the same handle accepts any
   backend.  Default stays whisper-base for back-compat.  GUI backend-
   picker dropdown is a follow-up — the data layer just stops gating it.

- [ ] **2. CLI `chat transcribe` + `chat tts`** (~6 h, slice A) — direct
      in-process integration via the existing `crispasr` Rust crate
      (path-dep at `../CrispASR/crispasr`; safe wrapper over the C-ABI,
      no shell-out).  Extends `src-tauri/src/asr/mod.rs`'s `Session`
      wrapper to all 24 ASR + 5 TTS backends and adds output-format
      glue (txt / SRT / VTT / JSON for ASR; WAV for TTS).  Single
      `Session` object handles both — `Session::synthesize(&str) ->
      Vec<f32>` is the TTS half.

      ```text
      crispsorter chat transcribe <input.wav>
                       [--backend whisper|parakeet|canary|qwen3|…]
                       [--model auto|<path>]   # auto = registry_lookup
                                               #        + cache_ensure_file
                       [--language auto|en|de|…]
                       [--format txt|json|srt|vtt]
                       [--output -|<path>]

      crispsorter chat tts "Hello world"
                       [--backend kokoro|qwen3-tts|orpheus|chatterbox|…]
                       [--model auto|<path>]
                       [--voice af_heart|…]    # backend-specific name
                       [--output out.wav]
      ```

      For shell scripting, batch jobs, headless servers — power-user
      surface, not the end-user win.  Reads any input the shared
      `src-tauri/src/audio/` module can decode (axis-2 scope above):
      WAV directly via hound, everything else via symphonia, ffmpeg
      as last-resort fallback.

- [ ] **3. Index-time audio + video transcription** (~10-14 h, slice B)
      — the end-user win.  Audio AND video files in scanned folders
      become first-class searchable documents in the LanceDB +
      Tantivy index, exactly like PDFs and OCR'd images today.

      **Shared `src-tauri/src/audio/` module** consumed by both
      slice A and slice B (single decoder, no divergence):

      - **Decoder Tier 1 (pure-Rust, no system deps):** `symphonia`
        — covers WAV / MP3 / M4A / FLAC / OGG / OPUS / AAC for
        audio AND MP4 / MOV / MKV / WebM / M4V for video (symphonia
        demuxes containers and gives back just the audio stream;
        no video decode needed).  Pure-Rust = ships everywhere
        CrispSorter does, no extra install.
      - **Decoder Tier 2 (last-resort, shell-out):** ffmpeg via
        `crate::audio::ffmpeg_fallback`.  Triggered for the long
        tail of containers symphonia can't read (.avi DivX, .wmv,
        .flv, .ts, .amr, .ra).  Runtime-detects ffmpeg-on-PATH;
        emits a clear "install ffmpeg for .avi" error if absent
        rather than silent failure.  Cross-platform: `which ffmpeg`
        works on macOS / Linux; `where.exe ffmpeg` on Windows.
      - **Resampler:** `rubato` (pure-Rust SRC).  Source files are
        typically 44.1 / 48 kHz stereo; CrispASR wants 16 kHz mono.
      - **Streaming path:** files longer than a configurable
        threshold (default 5 min) use `crispasr::Session::stream_open`
        + per-chunk `feed` + `get_text` instead of buffering the
        whole decoded PCM.  Bounds peak memory and emits per-chunk
        progress to the existing ingest UI.

      **Extractor entry point:** `extract_audio_text(path,
      &AsrConfig) -> Result<ExtractedDocument>`, mirroring
      `ocr.rs`'s shape.  Registered in `extractors/mod.rs`'s
      dispatch behind `AV_EXTS` + `ExtractOptions::transcribe_audio`
      (default OFF — transcription is expensive, opt-in like OCR).

      **Output:** `ExtractedDocument { full_text: <transcript>,
      headings: <none>, ext: "<mp3|mp4|wav|…>" }`.  Indexer treats
      long transcripts the same as long PDFs (chunk + embed).

      **CLI parity:** `crispsorter index ingest --transcribe-audio
      [--asr-backend NAME] [--asr-model PATH]` passes options
      through to `ExtractOptions` + `AsrConfig`, and the slice-A
      `crispsorter chat transcribe` accepts the same audio/video
      inputs through the shared decoder.

      Long-form transcript caching (sidecar `.cidx` keyed on
      content-hash so re-indexing is free) deferred to a follow-up
      once the v1 path ships.

**Cross-platform target — covered by `crispasr-sys` cmake auto-build:**

- macOS arm64 + x86_64: works today (`libcrispasr.dylib` already
  ships in the .app bundle via `scripts/bundle_macos_native_libs.sh`)
- Linux x86_64 + arm64: cmake builds without extra setup
- Windows x86_64: needs MSVC + cmake (same toolchain the GUI build
  already requires); covered by P3.5 Phase 2 distribution work as a
  single unit

GPU feature flags inherited from the GUI:
`crispasr-metal` / `crispasr-cuda` / `crispasr-vulkan`.

**Out of scope for these slices (deferred until basic shape ships):**
streaming / mic capture (`crispasr::Stream` + `crispasr::Mic`),
diarization (`crispasr::diarize_segments`), word-level alignment
(`crispasr::align_words`), punctuation restoration
(`crispasr::PuncModel`), source/target translation pairs, transcript
sidecar caching.  All exposed by the Rust crate — adding them is
incremental flag-plumbing, not a new integration.

### P13 — Bilder vertical (Photos / images)

- [x] **Tier 1 — local-only Bilder tab** (`A1–A4`, ~25 h, shipped)
      Image-row filtered view (`Übersicht → Bilder`), lazy-loaded
      thumbnails (PNG via `image` crate), EXIF preview pane
      (`kamadak-exif` with permissive `continue_on_error` for
      piexif-shaped IFD chains), SHA-256 dup view, perceptual-hash
      near-dup view (`image_hasher`'s `HashAlg::Gradient` at 8×8 —
      see the slice-doc deviation note for why DCT-pHash didn't fly
      at 64-bit).  Zero external server deps.  Full CLI parity:
      `crispsorter images {extensions, count, list, thumbnail, exif,
      duplicates, near-duplicates}`.
- [x] **Tier 2 — complete** (`B1–B5`, ~21 h spec, shipped)
      All five Tier-2 slices on `main`:
      - **B1** (`0aa3a51`) — `crisplens-protocol` crate, Keychain
        session storage, Settings UI, Tauri + CLI parity.
      - **B4** (`250f137`) — `/api/health` + `/api/auth/me` polling
        with banner state machine (offline / session-expired /
        warming-up / ok).  Plus `enable-crispembed.sh` cargo
        target-dir fix.
      - **B5** (`8a4a2e0`) — Open-in-CrispLens deep-link from the
        Bilder preview pane; watchfolder cross-reference via
        `/api/watchfolders` with prefix-match hint when the
        previewed image lives under a CrispLens-watched folder.
      - **B3** (`01e6203`) — People view (Faces subtab equivalent)
        listing person clusters from `/api/people`; per-image
        faces endpoint `/api/images/{id}/faces` plumbed end-to-end
        with the live-verified `Face { bbox: Bbox }` nested-object
        shape.  Image-overlay face boxes deferred (need sha256
        cross-reference at the list endpoint).
      - **B2 reduced** (`814efe8`) — Remote text search via
        `/api/search` (filename / person-name substring — the live
        API doesn't expose semantic search; spec's "semantic search
        bar" wording is aspirational and tracked as a future
        CrispLens-upstream item).  Inline UI search box visible
        only when Tier 2 is authenticated.

      All five live-verified end-to-end against
      `https://<crisplens-host>`.  29/29 `crisplens-protocol`
      tests + 18/18 `images::crisplens` tests + 64/64 `images::*`
      tests pin both v2 and v4 wire shapes captured from live
      payloads.

Full design + slice breakdown + risk register + spec-vs-reality
notes: [**docs/P13_Bilder_integration.md**](docs/P13_Bilder_integration.md).

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
