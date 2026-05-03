# CrispSorter — Development Plan

## Capabilities (shipped)

- **LanceDB + Tantivy hybrid search** — persistent embedded library with dense ANN + BM25 full-text, RRF fusion
- **ONNX / CoreML backend** — run ONNX-format models via `ort` crate with CoreML execution provider for Apple Neural Engine acceleration
- **CrispEmbed GGUF backend** — feature-gated optional backend using libcrispembed for GGUF model inference (Metal/CUDA/Vulkan GPU acceleration)
- **Expanded model registry** — 36 ONNX/GGUF model variants (BGE-M3, BGE en-v1.5 small/base/large, PIXIE-Rune, Snowflake Arctic L-v2, Jina v2/v3/v5, Qwen3-Embedding, Octen, MiniLM, Multilingual-E5 small/base/large, Nomic-Embed v1.5, Mxbai-Embed Large v1, all-MiniLM-L6-v2, EmbeddingGemma 300M, GTE base/large en-v1.5)
- **OrtPath backend** — handles ONNX models with external `.onnx_data` companion files and KV-cache decoder models
- **Cross-platform release workflow** — GitHub Actions builds for macOS ARM64/x86, Windows, Linux with llama-server sidecar
- **CrispEmbed CI integration** — sibling repo checkout + path rewrite so `cargo metadata` resolves on clean runners

## In Progress

- **Wire CrispEmbed sparse encoding into search pipeline** — BGE-M3/SPLADE sparse vectors via GGUF backend (C API ready, needs UI integration). Tracked under P2.
- **CrispEmbed reranking in search** — cross-encoder and bi-encoder reranking APIs are wired in `CrispEmbedBackend` but not yet used by the search pipeline. Tracked under P2.

---

## Open TODOs

### P2 — Search index / RAG

(All P2 items shipped — see Recent changes.)

### P3 — Voice chat (CrispASR integration)

(All in-scope P3 items shipped — see Recent changes. Hotword gating
remains an explicit non-goal for v1.)
  Decide: native macOS `say` / Windows SAPI for v1 (zero deps), or a small
  GGUF TTS (Piper / Kokoro) sidecar for cross-platform consistency.
  Settings: voice picker, rate, "auto-speak replies" toggle.
- [ ] **Hotword / wake word (optional)** — out of scope for v1, but the ASR
  thread should be designed so a separate small KWS model can gate full-ASR
  decoding when this lands.

### P3.5 — Bundling CrispEmbed / CrispASR native wrappers

Both `crispembed` and `crispasr` are currently `optional = true` cargo
path-deps. The default release binary doesn't link them — users get the
ONNX/fastembed embedder and no on-device ASR. To actually ship these
features we have to bundle their native shared libraries
(`libcrispembed`, `libcrispasr`, plus ggml backends) into the Tauri app
per platform. The sidecar/server alternative was explored and rejected:
it doesn't work on mobile (iOS/Android sandbox forbids spawning helper
processes), and the wrapper approach already has a proven cross-platform
recipe in CrisperWeaver (sibling Flutter app, same C library).

**Proven pattern (from CrisperWeaver `scripts/build_*` + `bundle_*` helpers):**

1. **Build `lib{crispasr,crispembed}.{so,dylib,dll}` from source per
   platform** with `-DBUILD_SHARED_LIBS=ON` plus the right GGML backend
   (`-DGGML_METAL=ON` / `-DGGML_VULKAN=ON` / `-DGGML_CUDA=ON`).
2. **Bundle libs into the per-platform app dir** — macOS
   `.app/Contents/Frameworks/`, Linux `bundle/lib/`, Windows next to
   the exe.
3. **Symlink aliases** so both `lib{crispasr,crispembed}` and
   `lib{whisper,...}` names resolve, plus the SONAME-versioned alias
   (`lib*.so.1` / `lib*.1.dylib`).
4. **Patch install names / RPATH** so the loader finds them at `@rpath`
   / `$ORIGIN`.
5. **Bundle Homebrew transitives** (e.g., kokoro pulls espeak-ng on
   macOS) + ad-hoc codesign.
6. **Repeat for ggml shared libs** (libggml.so, libggml-base.so,
   libggml-cpu.so).

For Tauri the only difference vs Flutter is the per-platform "lib dir":
`tauri.conf.json > bundle.macOS.frameworks` for macOS, `bundle.resources`
+ RPATH patching for Linux .deb, DLL colocation for Windows.

**Scope estimate per platform:**

| Platform       | Risk   | Effort | Notes                                                                  |
| -------------- | ------ | ------ | ---------------------------------------------------------------------- |
| macOS arm64    | low    | ~3-4h  | CrisperWeaver scripts adapt directly; Metal built-in                   |
| macOS x86_64   | low    | ~1h    | once arm64 works, just a target swap (still queue-starved on macos-13) |
| Linux x86_64   | medium | ~4-6h  | Tauri .deb + RPATH + Vulkan SDK install; less prior art                |
| Windows x86_64 | medium | ~4-6h  | DLL placement + Vulkan SDK install + signing                           |

Plus a change in `crispasr-sys`: its `build.rs` currently only emits
link directives, expecting a system-installed lib. It needs to do what
`crispembed-sys` already does — run cmake on the parent CrispASR repo
with `BUILD_SHARED_LIBS=ON` and emit the right
`cargo:rustc-link-search`. CrispEmbed-side `CRISPEMBED_BUILD_SHARED=ON`
is already correct, but its output dylib still needs the bundling
treatment.

**Phased rollout:**

- [ ] **Phase 1 — macOS arm64 only** (~3-4h)
  1. Patch `crispasr-sys/build.rs` to cmake-build `libcrispasr.dylib`
     (mirror `crispembed-sys`).
  2. Add a Tauri `afterBuildCommand` (or post-build script) that
     locates the cmake-built dylibs, copies them to
     `Contents/Frameworks/` with the right symlinks, patches install
     names, and re-codesigns.
  3. Make `crispasr` and `crispembed` features default-on for macOS
     arm64 in release.yml.
  4. Ship v0.1.36 and verify the .dmg actually launches with ASR
     working on a clean machine.
  5. Commit the proven recipe to LEARNINGS.md.

- [ ] **Phase 2 — extend to Linux + Windows** (~8-12h, separate session)
  Once Phase 1 is proven, mirror the pattern. Linux .deb + RPATH +
  Vulkan SDK; Windows DLL colocation + Vulkan SDK + signing. Each
  platform likely needs 1-2 release iterations to settle.

- [ ] **Phase 3 — mobile (iOS / Android)** (deferred)
  Only relevant if/when CrispSorter targets mobile. CrisperWeaver
  already has the recipe for both.

**Why not sidecar/server?** Mobile sandboxes (iOS, Android) forbid
spawning helper processes; the wrapper (linked-library) approach is the
only one that works cross-platform. Server-based IPC remains a viable
*option* for desktop power-users wanting CUDA on demand, but not as the
default.

### P4 — Code quality / maintenance

- [ ] Audit remaining hardcoded UI strings in `Settings.svelte` (model manager sections)
  and `LogPanel.svelte` and move them to `i18n.svelte.ts`.

### P5 — Future / planned

1. **Auto-process toggle on watch detection** — risky (auto-moves files
   without review); needs a confirmation step or a "watch + queue, don't
   auto-move" mode that's distinct from full auto.
2. **PWA demo** — generate `.sh`/`.bat` sorting scripts or browser-based sorting via File System Access API

### P6 — Catalog / Cathy integration (Catfish port-and-merge)

Bring [Catfish](https://github.com/CrispStrobe/Catfish)'s
drive-cataloging + duplicate-finding + offline file-search capabilities
into CrispSorter. The Python project is ~5.5kLOC and built around the
classic [Cathy](http://www.mtg.sk/rva/cathy/cathy.html) `.caf` binary
catalog format, which means CrispSorter would gain backwards-compatible
read/write of any `.caf` file produced over the past 20+ years
(Cathy 1.x → Catfish v8).

**Why integrate (vs. standalone Rust port):** CrispSorter is the
project's hub Tauri app and already does adjacent things — file
scanning, search, dedup-of-sorted-batches. A separate Catfish-RS
binary would split focus and reimplement the cross-platform shell that
Tauri already gives us. The two indexes are complementary: Catfish-style
`.caf` is a flat (path, size, mtime) snapshot of "everything on a
drive", whereas CrispSorter's LanceDB stores rich vector + LLM-derived
metadata for the smaller subset that the user actually sorted. Linking
them lets the sorter say "you've already filed this exact file under
project X on drive Y."

**`.caf` format spec (verified against Catfish `core/file_index.py`):**
* Little-endian binary; magic = `version × 1_000_000_000 + 500_410_407`
  with `version = 1..=8`. v ≥ 3 reads the version word as `<i16>` after
  the magic.
* Header: `<L>` date, NUL-terminated latin-1 device path (v ≥ 2),
  volume label, alias, `<L>` serial, comment (v ≥ 4), `<f32>` freesize
  (v ≥ 1), `<i16>` archive flag (v ≥ 6).
* Info block: `<i32>` dir_count, then for each dir a name (only first
  dir for v ≤ 3, all dirs for v ≤ 6 — different rule per version) plus
  `<i32, f64>` (file_count, total_size) for v ≥ 3.
* ELM block: `<i32>` file_count followed by entries. Per-entry struct:
  v ≤ 6 → `<L l H>` (mtime, size32, parent_id16) — **no per-file size,
  legacy quirk**; v == 7 → `<L q H>`; v == 8 → `<L q L>` (32-bit
  parent_id). Filename is NUL-terminated latin-1.
* `size < 0` encodes a directory: directory ID = `-size` (v > 6) or
  positional index (v ≤ 6).
* Hashes are **not** stored — recomputed on demand for dedup.

**Bridge with CrispSorter's existing data:** LanceDB stores rich rows
keyed by `path`. A `.caf` row is a strict subset of that. So:
* **Import `.caf` → CrispSorter:** treat each entry as a "candidate
  file" the user might want to sort/embed. No conflict with the
  existing batch flow — entries appear as a new "Catalog" source
  alongside the watcher queue and folder pickers.
* **Export CrispSorter → `.caf`:** dump the sorted-batch slice (or any
  query result) to a `.caf` for archival / sharing. Lossy in one
  direction (drops embeddings + LLM categories) but round-trips the
  Cathy-compatible bits perfectly.

**Phased plan (~3 weeks, each phase shippable independently):**

- [ ] **Phase 1 — `.caf` I/O + parallel scanner** (~1 wk)
  1. `src-tauri/src/catalog/{mod.rs, caf.rs, index.rs, scan.rs}`
  2. `caf.rs`: byte-exact reader + writer for versions 1-8, including
     the legacy v ≤ 6 size quirks. Round-trip property test (`load →
     save → load` must yield bit-identical bytes for ≥ 1 captured Cathy
     fixture).
  3. `index.rs`: in-memory `FileIndex` mirroring Catfish's structure
     (size_index `HashMap<u64, Vec<Entry>>`, all_files `Vec<Entry>`,
     optional sorted prefix/suffix arrays for fast name search).
  4. `scan.rs`: parallel directory walker via `jwalk` (rayon-backed) so
     scanning a hard drive uses all cores instead of one. Optional
     hashing inline (`md-5`, `sha-1`, `sha-2`) gated behind a config
     flag.
  5. Tauri commands: `catalog_load_caf(path)`, `catalog_save_caf(path,
     index)`, `catalog_scan_dir(path, hash_algo)`,
     `catalog_metadata(path)` (cheap header-only read for index
     listings).
  6. Unit tests cover: load fixture v8 .caf → expected file count;
     load/save round-trip; legacy v6 with no per-file size; mixed
     Windows/POSIX path device strings.

- [ ] **Phase 2 — Duplicate engine + CLI parity** (~1 wk)
  1. `dedup.rs`: size-bucket fast path (compare by size first, then
     hash only matching candidates), parallel hash verification via
     rayon. Mirror Catfish's `find_all_duplicates_bulk` API.
  2. JSON output mode for Tauri commands, matching Catfish's `--output
     json` so existing CLI scripts can swap binaries.
  3. Generate-deletion-script feature (.bat / .sh) for scriptable
     cleanup, matching Catfish's interactive deletion workflow but
     review-first by default.
  4. Tauri commands: `catalog_find_duplicates(source_path,
     destinations[], options)`, `catalog_generate_deletion_script(matches[])`.

- [ ] **Phase 3 — UI tabs in CrispSorter** (~1 wk)
  1. New "Catalog" tab in Settings.svelte (or a new top-level page):
     list available `.caf` files, create/refresh/delete, browse
     entries offline (works even if the source drive is unmounted).
  2. New "Find Duplicates" tab: source folder picker + N destination
     folder pickers, hash algorithm dropdown, "reuse existing
     indexes" / "force recreate" toggles, results table with
     per-row select + bulk delete-script export.
  3. Existing batch view gets a "duplicate of X in catalog Y"
     indicator badge when an entry's hash matches a cataloged file.

- [ ] **Phase 4 — Hybrid storage (option C)** (~1 wk)
  Decision: `.caf` stays the canonical on-disk persistent form;
  LanceDB gets a lightweight `catalog_entries` table populated
  on-demand for *active* catalogs. The `.caf` is the source of
  truth; LanceDB is a derived index that can be dropped/rebuilt
  any time. Lets users keep portable Cathy-compatible files while
  also getting catalog rows into the existing dense + Tantivy +
  RRF search stack when they want.

  - [ ] **4a — Catalog manager service** (~2 hr)
    Persistent registry of known `.caf` files with `active` flag,
    backed by `tauri-plugin-store`. Tauri commands:
    `catalog_register(path)`, `catalog_unregister(path)`,
    `catalog_list()`, `catalog_set_active(path, active)`. No
    LanceDB integration yet — UI in Phase 3 can wire against this
    immediately.

  - [ ] **4b — LanceDB materialization** (~3 days)
    New `catalog_entries` Lance table with a thin schema:
    `(catalog_path: Utf8, entry_path: Utf8, size: UInt64,
    mtime: UInt32, hash: Option<Utf8>)`. On `set_active(true)`:
    load `.caf` → batch-insert rows. On `set_active(false)`:
    delete rows where `catalog_path = X`. Cross-link to the
    existing `documents` table via `entry_path` (when a sorted
    document's path matches a cataloged entry, both rows surface
    as a single hit with provenance metadata).

  - [ ] **4c — Search integration** (~1-2 days)
    `SearchEngine` learns to optionally query `catalog_entries`
    alongside `documents`. Catalog-only hits show with a
    "[catalog: X]" badge in the existing results UI. RRF fusion
    treats catalog name-match scores as another channel —
    Tantivy's existing FTS infra handles filename tokenisation
    via a new `catalog_fts` Tantivy index over `entry_path`
    components.

  - [ ] **4d — Import/export between sorted batch and `.caf`**
    `catalog_export_sorted(out_path, scope)` dumps the
    sorted-batch slice (or any LanceDB query) to a fresh `.caf`
    for archival/sharing. `catalog_import_caf_to_batch(caf_path)`
    brings `.caf` entries into the batch view as candidate files
    (reuses the watcher's `add_item` path; user still presses
    Start to sort/embed). Lossy in the sorted → .caf direction
    (drops embeddings + LLM categories), bit-perfect in the
    other.

- [ ] **Phase 5 (optional, deferred) — extract `crispcat` workspace crate**
  Move `src-tauri/src/catalog/` to a sibling workspace crate
  (`crates/crispcat/`) so a thin standalone CLI binary
  (`crates/crispcat-cli/`) can ship the catalog-only feature for users
  who want it without the rest of CrispSorter. Keeps the Tauri app's
  binary footprint unchanged.

**Why not a standalone Rust app instead:** The Catfish UI is Tkinter,
which has no parity in the Tauri/Svelte stack — porting it would mean
choosing egui/iced and building a new GUI from scratch (~1-2 weeks of
UI work that doesn't add capability). Integrating into CrispSorter
reuses the existing Svelte UI infrastructure and gives users one app
instead of two. The Phase 5 escape hatch (extract to a workspace crate
+ CLI binary) preserves the option to ship a standalone tool later
without committing to that maintenance burden upfront.

**Acknowledgments:** The .caf format spec is reverse-engineered from
[Catfish](https://github.com/CrispStrobe/Catfish) which itself drew on
[binsento42/Cathy](https://github.com/binsento42/Cathy) and the
original [Cathy](http://rva.mtg.sk/) by Robert Vašíček.

### P7 — Full-volume desktop search parity

Where we are after P2 + P6: CrispSorter can do dense + BM25 + sparse
hybrid search with cross-encoder reranking, plus filename search across
mounted catalogs. But content indexing is *opt-in* — only files the
user explicitly added to a sort batch end up in the documents table.
P7 closes the gap between "smart sort assistant" and a general-purpose
desktop search engine: every PDF, Office doc, source file, and EPUB on
every mounted volume becomes searchable by content (not just filename),
with operator-grade query syntax, instant preview, saved searches, and
cross-mount awareness.

The catalog table from P6 is the foundation: a row already exists for
every file on every active drive. P7 extends each row with the
extracted text content + an embedding, on a background schedule, so
the existing dense + BM25 + RRF pipeline applies to the full filesystem
rather than just the curated batch.

**Why bother (vs. relying on the OS):** OS-level search (Spotlight on
macOS, Windows Search, tracker on Linux) is filename-good but
content-mediocre across the long tail of file types, indexes only the
boot volume by default, has no semantic / vector channel, and exposes
no programmable query syntax. CrispSorter's pipeline already has the
better backend; what's missing is just the "index everything in the
background" loop and a few UI conveniences.

**Phased rollout (~6-8 weeks total, each phase shippable):**

- [ ] **Phase 7.1 — Unified query covering catalogs** (~3-4 days)
  `index_search` learns to query both `documents` and
  `catalog_entries` in one pass. Catalog-only hits surface with a
  `[catalog: <name>]` badge in the existing results UI. RRF fusion
  treats catalog name-match scores as another channel. (Overlaps
  with P6 Phase 4c — implement once, count for both.)

- [ ] **Phase 7.2 — Operator-grade query syntax** (~2-3 days)
  Expose Tantivy's existing boolean / phrase / proximity / wildcard /
  field-prefix syntax through `index_search`. Today the query is
  pass-through term-vectorised; switching to Tantivy's `QueryParser`
  on a documented field whitelist costs ~150 LOC and unlocks queries
  like `title:"chemistry" AND year:[2020 TO 2024] -archived`.

- [ ] **Phase 7.3 — Live preview pane** (~3-4 days)
  Right-side or hover-popup pane in result rows that renders the
  matched document. PDF / image / plain text via the Tauri webview
  (PDF.js or native `<object>`); Office docs via a "open in app"
  fallback for v1, server-side conversion in a follow-up. Reuses the
  existing `extract_pdf_native` command for the snippet context.

- [ ] **Phase 7.4 — Background full-content ingest** (~2-3 weeks,
  the big piece)
  Walk active catalogs in the background, extract content per file
  type, embed, write to `documents`. Heavy infrastructure work split
  into sub-phases:
  1. **Per-filetype extractor registry** (~1 wk). Already shipped:
     PDF (`pdf_extract` + the LLM markdown pipeline). To add: docx /
     xlsx / pptx (via a Rust `dotnetzip`-style reader or `pandoc`
     sidecar), EPUB (via `epub` crate), RTF (via `rtf-grimoire`),
     plain text + source code (trivial), HTML (via `scraper`),
     compressed-archive members (via `zip`/`tar` crates — index file
     listings now, member contents later).
  2. **Background ingest scheduler** (~3-4 days). New
     `IngestState::Background` mode in `index/ingest.rs` that
     consumes a queue of (catalog_path, entry_path) pairs at a
     bounded rate (CPU-throttle, mtime-aware so unchanged files
     skip), persists progress to `tauri-plugin-store` so a restart
     resumes mid-walk.
  3. **Diff-based incremental updates** (~3-4 days). Reuse the P5
     `notify`-based watcher to enqueue changed files into the
     background queue, plus a periodic full-walk for catalog
     refreshes that miss watcher events (drives unmounted at the
     time, etc.).
  4. **Throttling + QoS** (~2 days). Pause ingest during user-driven
     work (dense embedding queries), respect macOS App Nap / Linux
     `nice` so background indexing doesn't spike CPU during normal
     use.

- [ ] **Phase 7.5 — Saved searches** (~2-3 days)
  Persist (query, filters, columns) tuples in `tauri-plugin-store`,
  surface as left-rail items. Click → re-run query, results refresh
  live as the background ingest catches up. Lightweight; pure
  frontend on top of P7.1 + 7.2.

- [ ] **Phase 7.6 — Cross-mount awareness** (~1 wk)
  Tag catalog rows with the source volume's UUID (macOS
  `getattrlist`, Linux `blkid`, Windows volume serial), not just the
  mount-point path. When a volume mounts/unmounts, `documents` rows
  pinned to it auto-toggle their searchability. Lets users keep an
  archive drive's index without ever needing the drive plugged in
  except to refresh.

- [ ] **Phase 7.7 — Mountable archive index files** (~1-2 wks)
  Serialise a per-volume slice of the documents table to a portable
  `.cidx` file (Lance dataset export format). Load → drive's full
  search index lights up offline. Same offline-browse property the
  P6 catalog already has, extended to the rich content+embedding
  rows. Useful for archived backups: ship the archive drive + a
  `.cidx` file in the same backup snapshot.

- [ ] **Phase 7.8 — OCR for scanned PDFs / images** (~2 wks)
  Run Tesseract (or platform-native: macOS Vision framework, Windows
  10+ OCR API) on rasterised pages of PDFs that contain no extractable
  text, plus standalone JPG/PNG. Results land in `full_text` like any
  other extraction. Opt-in per-catalog because OCR is CPU/memory
  heavy.

**What CrispSorter has that off-the-shelf desktop search doesn't:**

* Dense + sparse + BM25 hybrid via RRF — semantic queries find
  conceptually-related documents even when no keyword matches.
* Cross-encoder reranking on top-N — the `cstr/*-reranker-GGUF`
  models give markedly better precision than pure BM25.
* Voice queries via the P3 ASR backend — speak the query, get
  results read back via TTS.
* The whole sorted-batch + LLM-categorise pipeline that's *adjacent*
  to but separate from search — the same indexed content can be
  pulled into a sort batch with the LLM categoriser running over it.

P7 is what makes those backend strengths actually *visible* outside
the curated sort-batch use case.

- [x] **XMP metadata extraction (May 2026, v0.1.35)** — `extract_pdf_metadata`
  now reads the catalog's `/Metadata` stream (XMP RDF/XML) in addition to
  the `/Info` dict. XMP fields win when present (better-curated by
  publisher tooling); `/Info` fills any gaps via the new `merge_in`
  helper. quick-xml-based event walker tracks `dc:title`, `dc:creator`,
  `dc:subject`, `dc:description`, and `xmp:CreateDate`/`ModifyDate`/
  `MetadataDate` — handles the typical RDF wrapping (`Alt`/`Seq`/`Bag` >
  `li`). Multiple creators get joined with `" and "` (BibTeX-friendly
  format). Uses quick-xml's `xml_content()` to decode + unescape XML
  entities in one step. 5 new unit tests cover the dc:Alt/Seq pattern,
  Bag keywords, XMP-with-only-Producer (returns None — no merge needed),
  truncated input resilience, and the merge precedence.
- [x] **Multi-folder watcher (May 2026, v0.1.34)** — extends v0.1.32
  from single-folder to a list. `WatcherState` now holds
  `HashMap<PathBuf, RecommendedWatcher>` keyed by canonical path; one
  shared per-path debounce map across all watchers. Tauri commands:
  `watch_start` (idempotent), `watch_stop_one`, `watch_stop_all`,
  `watch_list`. Settings UI shifts to a list with `+ Add folder` /
  `×` per-row remove. `watchFolders: string[]` setting; on read,
  migrates the v0.1.32 single-folder shape (`watchEnabled` +
  `watchFolder`) to the list, so existing users don't lose their
  setup. `+page.svelte` resume loop calls `watch_start` for each;
  cleanup uses `watch_stop_all`. EN+DE.
- [x] **BibTeX export (May 2026, v0.1.33)** — pure-TS `buildBibFile` in
  `src/lib/export/bibtex.ts`. Citation key = sanitized `{LastName}{Year}`,
  numeric suffix on collisions. Author lastname extracted from "Smith,
  John" or "John Q. Smith"; falls back to "anon". Year regex-matched to
  the first 4-digit substring (handles "2023-03", "approx. 2019"). LaTeX
  special chars (`\ & % $ # _ { } ^ ~`) escaped per the BibTeX spec;
  capitalized words in titles wrapped in `{…}` so case-folding styles
  preserve them. All entries emit as `@misc` (universally accepted; we
  don't have enough metadata to differentiate article/book/report yet).
  Placeholder values (`Unknown Title`, `n/a`, `?`, `-`) are skipped
  rather than emitted as data. Export button in BatchReview header
  next to the source-update button; saves via Tauri dialog with
  `crispsorter-YYYY-MM-DD.bib` default name. Skips items the user
  marked Ignored. EN+DE.
- [x] **Folder watcher v1 (May 2026, v0.1.32)** — drop a file into the
  watched folder and it lands in the batch. New `watcher/` module wraps
  `notify` (FSEvents on macOS, inotify on Linux, `ReadDirectoryChangesW`
  on Windows). `watch_start` / `watch_stop` / `watch_status` Tauri
  commands; single-folder invariant for v1 (multi-folder is future
  work). Per-path 2-second debounce kills the duplicate events common
  to atomic-save patterns. Extension allowlist matches the rest of
  the app (pdf, epub, djvu, txt, md, rtf, doc, docx, odt); editor
  swap files (`.tmp`, `.crdownload`, dotfiles, `~`-suffixed) get
  dropped. Settings UI: folder picker + enable toggle + Apply button.
  `+page.svelte` owns the global `folder-watch:added` listener — calls
  `batchManager.addItem` with path/name/size; `addItem` already dedupes
  on path so retried events stay benign. **No auto-process** in v1:
  files queue up, user still presses Start. The architecture supports
  auto-process as a future toggle — flagged as risky in PLAN P5.
- [x] **PDF metadata pre-fill (May 2026, v0.1.31)** — new
  `extract_pdf_metadata` Tauri command reads the PDF /Info dictionary
  via `lopdf` (already a transitive dep of pdf-extract). Returns title /
  author / subject / keywords / year / producer; year parsed best-effort
  from the `D:YYYYMMDD…` PDF-date format. UTF-16BE-with-BOM and UTF-8
  string decoders handle the most common producer encodings; PDFDocEncoding
  falls back to lossy UTF-8 (covers Title/Author for most European PDFs).
  Frontend extraction phase invokes it on `.pdf` files when the new
  `pdfMetadataPrefill` Settings toggle is on (default true) and pre-fills
  empty `suggestedTitle/Author/Year` slots. The LLM (when enabled) still
  overwrites these in phase 2 — this is purely a fallback for runs where
  the LLM is off or fails. (XMP metadata streams added in v0.1.35.)
  6 new unit tests pin the date parser + string decoder shape.
- [x] **TTS auto-speak for chat replies (May 2026, v0.1.30)** — closes the
  P3 voice loop with zero-dep platform synth. New `tts/mod.rs` shells
  out to macOS `say` / Windows PowerShell SAPI / Linux `spd-say` or
  `espeak` (whichever is on PATH), piping text via stdin so arbitrary
  chat content needs no argv quoting. `tts_speak` and `tts_stop` Tauri
  commands. AppState holds the running child so `tts_stop` (and a fresh
  `tts_speak`) can kill it mid-utterance — no overlapping voices.
  Settings adds an "Auto-speak chat replies" toggle (default off).
  Chat.svelte detects new bot messages via the deep-chat `onMessage`
  delta, strips Markdown/HTML, and pipes plaintext to the synth. Mute
  button appears in the chat header while speaking. The contract is
  identical for a future GGUF Piper/Kokoro sidecar — only the spawn
  function would change.
- [x] **CrispASR voice input — sidecar + push-to-talk (May 2026, v0.1.29)** —
  optional `crispasr` path dep at `../../CrispASR/crispasr` with cargo features
  `crispasr`, `crispasr-metal`, `crispasr-cuda`, `crispasr-vulkan` mirroring
  the CrispEmbed pattern. New `src-tauri/src/asr/mod.rs` wraps `crispasr::Session`
  with auto-download via `cache_ensure_file`. `AsrHandle` is a cheap-clonable
  lazy-load handle on `AppState`. New `asr_transcribe` Tauri command takes
  Float32 PCM 16kHz mono and returns concatenated transcription text.
  `Chat.svelte` has a mic button next to Clear: WebAudio capture →
  OfflineAudioContext resample to 16 kHz → `invoke('asr_transcribe')` →
  `chatElement.submitUserMessage`. Stub-on-feature-off path so users without
  the `crispasr*` feature flag get a clean error toast. CI: release.yml now
  also checks out `CrispStrobe/CrispASR` as a sibling and rewrites the path
  dep, parallel to the existing CrispEmbed handling.
- [x] **Matryoshka dimension selection (May 2026, v0.1.28)** — new
  `IndexConfig.matryoshka_dim: Option<u32>` threads through
  `EmbedderConfig.with_matryoshka_dim` to `CrispEmbedBackend::set_dim` at
  load. `EmbedderConfig::effective_dim()` clamps to the model's nominal
  dim and treats `Some(0)` as `None` (model default). The LanceDB column
  width now uses the effective dim so the schema matches what the embedder
  emits — changing `matryoshka_dim` on an existing index requires
  re-ingestion (warned in the UI hint). UI: number-select (128/256/384/512/768)
  appears under "Inference Backend" only when GGUF is selected and the
  model has a GGUF spec — fastembed has no per-call truncation hook so
  ONNX paths ignore the field. Quality only holds for MRL-trained models
  (BGE-M3, Snowflake Arctic L v2, PIXIE-Rune); the hint flags this.
- [x] **Sparse retrieval + Octen auto-download (May 2026, v0.1.27)** — BGE-M3
  / SPLADE sparse vectors are now used at query time as a 3rd RRF channel
  alongside FTS + dense ANN. `LocalIndex::search_sparse_in_pool` scores the
  union of FTS+ANN candidates by sparse dot product (two-pointer merge for
  sorted indices, hash-join fallback otherwise) and `SearchEngine::maybe_sparse_search`
  fuses the result via the new generalized `rrf_merge_n`. Auto-on when the
  embedder has a sparse head (BGE-M3, BGE-small en-v1.5 with SPLADE++);
  silently skipped otherwise. Octen 0.6B variants (FP32, INT4, INT8-Full)
  switched from local-only `with_local_subdir` to fastembed-native
  auto-download via `cstr/Octen-Embedding-0.6B-ONNX*` HF repos. The
  matMul-only INT8 variant stays local-only (no fastembed equivalent —
  dropped in fastembed-rs 77cc2e45 due to platform-dependent checksums).
- [x] **Configurable model cache dir (May 2026, v0.1.25)** — new
  `IndexConfig.model_cache_dir: Option<String>` + `resolve_model_cache_dir`
  helper picks: `CRISPSORTER_MODEL_CACHE_DIR` env > UI override >
  `{data_dir}/models/`. Single dir is shared by fastembed (ONNX), hf-hub
  (external-data ONNX + GGUF embedder + GGUF reranker), so one setting
  controls every weight on disk. Settings.svelte adds a "Model cache
  directory" picker; an external volume like
  `<external-volume>/ai/crispsorter-models` lets the cache survive app
  re-installs and (partially) share with CrispEmbed CLI. Three unit tests
  pin the resolve precedence.
- [x] **Cross-encoder reranking pipeline (May 2026, v0.1.25)** — new
  `RerankerModel` enum (`BgeRerankerV2M3`, `BgeRerankerBase`,
  `JinaRerankerV2BaseMultilingual`) + `Reranker` wrapper around
  `crispembed::CrispEmbed::rerank` (cross-encoder only; bi-encoder skipped).
  `RerankerHandle` is a cheap-clonable lazy-load handle: GGUF download +
  model open happens on first `score_batch` call. `SearchEngine` now fetches
  `rerank_top_n` candidates (default 50) from FTS / ANN / RRF when a
  reranker is configured, scores each via `score_batch(query, snippets)`,
  and re-sorts; NaN scores fall back to RRF order. `IndexConfig` gains
  `reranker_model: Option<RerankerModel>` + `rerank_top_n: usize`. UI:
  Settings.svelte adds a "Reranker" section between Compute Device and Data
  Directory. GGUF-only — without the `crispembed` cargo feature, `Reranker::load`
  returns a clear error.
- [x] **Pre-existing FTS regression fixed (May 2026)** —
  `index::fts_index::tests::scenario_accent_folding` was failing on `main`
  before any of this branch's edits: query-side `fold_accents` was applied
  but the index used Tantivy's `default` tokenizer (lowercase only), so
  `München` was indexed as `münchen` and never matched the folded query
  `munchen`. Fixed by registering a custom `ascii_folding` tokenizer
  (SimpleTokenizer + RemoveLong + LowerCaser + AsciiFoldingFilter) on the
  index and using it for the title/headings/body fields. Existing FTS dirs
  need re-ingestion — see LEARNINGS.md for the migration note. Also cleaned
  up clippy: `wrong_self_convention` on `to_gguf_spec`/`to_model_spec`
  (`&self` → `self` since `EmbedderModel: Copy`), and explicit
  `#[allow(dead_code)]` on `CrispEmbedBackend` placeholders that future P2
  work will use.
- [x] **Query/passage prefix selection (May 2026)** — auto-apply model-specific
  prefixes via `EmbedderModel::prefix(EmbedRole)`. E5 (`query:` / `passage:`),
  Nomic v1.5 (`search_query:` / `search_document:`), BGE en-v1.5 + Mxbai
  (BGE-style query-only), Jina v5 (`Query:` / `Document:`), EmbeddingGemma
  (task templates). All other models pass through unprefixed. CrispEmbed path
  uses native `set_prefix`; fastembed/OrtPath paths prepend in Rust. Sparse
  encoders (BGE-M3, SPLADE++) untouched — trained without prefixes.
- [x] **CrispEmbed/fastembed-rs registry sync (May 2026)** — added 12 new
  `EmbedderModel` variants (`MultilingualE5{Small,Base,Large}`, `Bge{Small,Base,Large}EnV15`,
  `NomicEmbedTextV15`, `MxbaiEmbedLargeV1`, `AllMiniLmL6V2`, `EmbeddingGemma300M`,
  `Gte{Base,Large}EnV15`). Each wired through both ONNX (native fastembed-rs
  via `CrispStrobe/fastembed-rs@feat/new-model-entries`) and GGUF (CrispEmbed
  `cstr/*-GGUF` registry). `BgeSmallEnV15` paired with `SparseModel::SPLADEPPV1`
  per `HISTORY.md` §2 rationale. Serde kebab-case test pins frontend mapper.
- [x] Stop button — wires `AbortController` through extraction and LLM queries (v0.1.22)
- [x] Per-request LLM timeout — 3 min local / 60 s remote via `Promise.race` (v0.1.22)
- [x] Extraction hang timeout — 5 min auto-abort on `extractionAbort` controller (v0.1.22)
- [x] Frontend log panel — `flog()` store, merged with Rust `app-log` events in LogPanel (v0.1.22)
- [x] Live processing stats in footer — N/total done · extracting X · analyzing Y (v0.1.22)
- [x] Release workflow — auto-publish draft after matrix even if one platform runner is slow (v0.1.22)
- [x] macOS 13 / `crispembed` stub — created minimal stub so CI/dev builds resolve the optional dep
- [x] Stuck items on resume — `resumeLastSession()` resets extracting/analyzing → unfinished (v0.1.23)
- [x] Per-page extraction watchdog — 30 s no-progress timeout replaces flat 5-min timeout (v0.1.23)
- [x] Two-phase batch processing — extract-all then analyze-all; LLM stall never blocks extraction (v0.1.23)
- [x] `unfinished` status — amber badge, filter option, footer counter, resetStuckItems handles it (v0.1.23)
- [x] i18n status strings — all BatchStatus values translated EN + DE; Chat/BatchReview use them (v0.1.23)
- [x] Chat context title/author — shows suggestedTitle + suggestedAuthor for analyzed docs (v0.1.23)
- [x] Stop button during rate-limit wait — `abortableSleep()` makes 429 backoff honour AbortSignal (v0.1.23)
- [x] Rate-limit Retry-After cap — capped at 90 s to prevent 10-min dead waits (v0.1.23)
- [x] Provider round-robin fallback — processAll phase 2 cycles through fallback providers on failure (v0.1.23)
- [x] Round-robin Settings UI — ordered checklist in LLM Options with up/down reorder (v0.1.23)
- [x] Index location update on move — `index_update_location_by_path` Rust command + TS call (v0.1.23)
- [x] i18n audit: Chat.svelte — "Docs:", "Chat:", "Clear Messages" use i18n keys (v0.1.23)
