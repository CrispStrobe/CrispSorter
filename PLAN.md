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

### P3 — Voice chat (CrispASR integration)

(In-scope items shipped — see HISTORY.md.)

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

(Phase 1 — macOS arm64 — shipped. See HISTORY.md.)

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

- [x] **Model-cache directory boot-drive hint** — shipped. The
  Settings → Search Index → Model cache directory hint now flags the
  10-20 GB boot-drive risk and recommends an external SSD with the
  hf-hub layout. EN + DE.

- [x] **i18n cleanup** — Settings.svelte + LogPanel.svelte audit
  shipped. ~80 strings keyed across both components: full LogPanel
  toolbar; shared sidecar badges (`settings.sidecar.*`) for Ollama +
  llamacpp; the German leaks in the index init block; Tesseract
  download labels; Ollama (Start/Fetch/logs/custom-tag/Pull); MLX
  (Start/Stop/server-log/lines/custom-placeholder); llamacpp
  (PORT/sidecar-logs/custom-HF-repo); benchmark (model picker, empty
  state, table headers, View, modal Run/cold/warm/error/empty);
  Inference Backend section + ONNX/GGUF labels + hint; per-file
  conversion timeout label + 3-segment hint; data-dir + model-cache
  placeholders; Active model header; Move up/down round-robin
  tooltips; licenses Generated line + Source button; ~12 icon-button
  aria/title fallbacks (clear log, delete from disk, remove from
  list, refresh models, etc.). EN + DE matching. svelte-check clean.
  The embedder optgroup labels (PIXIE-Rune, BGE en-v1.5 …) stay
  unlocalised — they're brand/technical names, not UI chrome.

### P5 — Future / planned

1. **Auto-process toggle on watch detection** — risky (auto-moves files
   without review); needs a confirmation step or a "watch + queue, don't
   auto-move" mode that's distinct from full auto.
2. **PWA demo** — generate `.sh`/`.bat` sorting scripts or browser-based sorting via File System Access API

### P6 — Catalog / Cathy integration

(Phases 1-4 shipped end-to-end — `.caf` I/O + parallel scanner,
duplicate engine + deletion-script generator, Catalog/Duplicates UI
tabs, and the hybrid-storage `catalog_entries` Lance table. See
HISTORY.md for the original spec, format details, and design rationale.)

- [ ] **Phase 5 (optional, deferred) — extract `crispcat` workspace crate**
  Move `src-tauri/src/catalog/` to a sibling workspace crate
  (`crates/crispcat/`) so a thin standalone CLI binary
  (`crates/crispcat-cli/`) can ship the catalog-only feature for users
  who want it without the rest of CrispSorter. Keeps the Tauri app's
  binary footprint unchanged.

**Acknowledgments:** The .caf format spec is reverse-engineered from
[Catfish](https://github.com/CrispStrobe/Catfish) which itself drew on
[binsento42/Cathy](https://github.com/binsento42/Cathy) and the
original [Cathy](http://rva.mtg.sk/) by Robert Vašíček.

### P7 — Full-volume desktop search parity

(Phases 7.1-7.6 + 7.6 follow-up + 7.8 Tiers 1-2 shipped — unified
catalog/documents search, operator-grade query syntax, live preview
pane, background full-content ingest, saved searches, cross-mount
UUID tagging *with* search-time availability filtering, Tesseract +
ocrs OCR. See HISTORY.md for the original spec and design rationale.
The two follow-ups below close the loop on offline archive use and
higher-quality OCR.)

- [ ] **Phase 7.7 — Mountable archive index files** (~1-2 wks)
  Serialise a per-volume slice of the documents table to a portable
  `.cidx` file (Lance dataset export format). Load → drive's full
  search index lights up offline. Same offline-browse property the
  P6 catalog already has, extended to the rich content+embedding
  rows. Useful for archived backups: ship the archive drive + a
  `.cidx` file in the same backup snapshot.

**Phase 7.8 — OCR Tiers 3 + 4** (Tiers 1-2 shipped — see HISTORY.md):

- [ ] **Tier 3 — `usls` PaddleOCR pipeline** (~3-5 days)
  MIT, ONNXRuntime via the existing `ort` dep already in our
  binaries (no new heavy install). Provides PaddleOCR DB+SVTR
  multilingual text + SLANet table recognition + DocLayout-YOLO
  for structure. ~200-500 MB models that download from
  HuggingFace on first use, same auto-download pattern as our
  embedders. Massive quality jump for German / CJK / Arabic.
  Caveat: "personal project, spare time" maintenance — pin a
  known-good version.

- [ ] **Tier 4 — `deepseek-ocr.rs`-style VLM OCR** (~1 wk, opt-in
  cargo feature like `crispembed`/`crispasr`).
  Apache-2.0, 2.1k stars, pure Rust via Candle (NOT ort, so a
  second tensor stack carried). DeepSeek-OCR / PaddleOCR-VL /
  DotsOCR backends with Q4_K / Q6_K / Q8_0 quantizations. Highest
  quality available — layout-aware, reading-order-aware, handles
  hard scans the lower tiers fail on. Cost: 4.7-9 GB models,
  9-50 GB RAM minimum. macOS Metal works; Linux/Win CUDA "alpha".
  Right shape: `--features crispsorter-vlm-ocr` opts in for users
  with the hardware.

**Dispatch order in `extract_text_from_path_with_opts` once all
tiers are wired:** caller passes a `OcrTier::Auto | Tier1 | Tier2 |
Tier3 | Tier4` enum, default Auto picks the highest-quality tier
available for the platform / build / installation state.

**What we deliberately don't consume:**
* `readur` — competitor product (Paperless-ngx alt), zero
  embed-able pieces.
* `Crane` (lucasjinreal) — no LICENSE file in the repo, can't
  legally ship code that depends on it.
* Per-platform native OCR APIs (macOS Vision, Windows 10+ OCR)
  deferred — Swift / WinRT sidecars complicate the bundle pipeline
  we just stabilised in P3.5; Tiers 2-4 cover the same quality
  range without the per-platform sidecar tax.

### P8 — Versatility & operability

(P8.1 *Per-file conversion timeout* shipped — see HISTORY.md.
P8.2 *CLI mode* first cut shipped — clap router + version / doctor /
catalog subcommands.)

#### P8.2 — CLI mode (continuation)

The stateful subcommand families need a Tauri-runtime spinup so the
shared `AppState` (LanceDB pool, embedder, ASR/LLM session managers,
foreground-active counter) is available without bootstrapping the
webview. Each family below is a separate ~1-day task on top of the
existing scaffold.

- [ ] **`index`** — `init` / `ingest <folder|file>...` /
  `search <query> [--mode hybrid|text|vector]` / `stats` /
  `delete <doc-id>` / `list` / `build-ivf-pq`. Wraps the existing
  `index_*` Tauri commands; needs an in-process `IndexState` constructor
  that mirrors what the Tauri builder does today.

- [ ] **`batch`** — `add <file>...` (appends to the same persisted
  store the GUI reads, so the next GUI launch sees them) / `process
  [--filter STATUS] [--limit N]` (runs the batch pipeline headless,
  dumps proposed moves) / `apply <plan.json>` (executes a previously-
  processed plan).

- [ ] **`chat`** — `query "<prompt>" [--context-files X,Y,Z]` /
  `transcribe <audio-file>` (ASR via P3 backend) / `tts "<text>"`
  (platform synth via P3 TTS). The mistralrs / llama-server spawn
  needs to work without the GUI's process-tracking.

- [ ] **`completion`** — emit shell-completion scripts (`bash`, `zsh`,
  `fish`) via clap's built-in generator.

- [ ] **Polish** — man-page generation via `clap_mangen`, single-binary
  install story (`brew formula` / `winget` / `cargo install
  crispsorter` — the `cargo install` path needs the P6 Phase 5
  `crispcat` workspace-crate extraction so the CLI doesn't drag in
  the whole Tauri webview footprint).

**Why this matters in combination with P7:** the CLI is the natural
way to bootstrap a fresh machine ("ssh into the file server, run
`crispsorter index ingest /data/archives --recursive` once, then walk
away"). The GUI stays primary for interactive sort/review; the CLI
handles automation.

### P9 — Übersicht at million-file scale

The L1 quick-scan now lets a user point CrispSorter at a multi-TB
archive in seconds. The Übersicht / Catalog list, however, was
designed for the dozens-to-hundreds case — `index_list_documents` returns
the full set, the Svelte template iterates over it, all filter chips
re-run client-side. Two orders of magnitude past that and the panel
freezes on every keystroke. This phase reshapes the catalog view so it
stays smooth at one to ten million rows on a single machine, the
working scale for desktop-search parity (P7).

#### Three architectural layers

1. **Indexed columnar storage** (LanceDB) — promote the soft fields the
   Übersicht filter chips depend on out of `metadata_json` into proper
   columns with scalar indexes. New columns:

   | column | type | purpose |
   |---|---|---|
   | `parent_dir` | `Utf8` | folder filter, folder-tree pane |
   | `volume_id` | `Utf8` | already in `metadata_json`; promote so volume-filter is index-backed |
   | `source_kind` | `Utf8` | `documents` / `catalog` / future archives |

   Add scalar (BTree) indexes on: `parent_dir`, `ext`, `year`,
   `language`, `owner_id`, `indexed_at`, `volume_id`. LanceDB's
   `create_index(scalar)` builds these in one pass. Field-level
   filters then hit the scalar index instead of full-scanning the
   row group.

   Fields that stay in `metadata_json` (low-cardinality, rarely
   filtered): EXIF camera/lens, PDF producer, EPUB publisher,
   per-row tags. Promote them only when a real filter chip appears.

2. **Paginated API** — new Tauri command:

   ```
   index_query_documents(
       filter: DocumentFilter,
       sort:   SortSpec,
       page:   PageSpec,
       columns: Vec<ColumnId>,   // projection; only fetch what the UI shows
   ) -> DocumentPage
   ```

   `DocumentFilter` mirrors the chip set: parent path prefix, ext
   list, level (L1/L2/L3), date range, completeness flags, name
   substring. `SortSpec`: column + direction; `PageSpec`: keyset
   cursor (`(sort_value, doc_id)`) — *not* offset-based; offset
   pagination on a 10M-row table is O(N) per page. Returns
   `{ rows, next_cursor, total_estimate }`. `total_estimate` comes
   from a cheap LanceDB row-count query against the same filter,
   so the UI can show "342k matches" without listing them.

   Companion command `index_folder_children(parent, depth)` for the
   lazy-loaded folder tree pane: returns `{ name, child_count }`
   tuples grouped by `parent_dir LIKE 'parent/%'`. Only the visible
   subtree is materialised.

3. **Virtualised UI** — `<div class="contents-list">` becomes a
   TanStack-Virtual-backed table. Only the rows in the viewport (~30)
   plus a small overscan are mounted. New layout:

   ```
   ┌─ folder tree ─┬─────── row list ──────────┬─ preview ─┐
   │  /            │  filename  ext  size  …    │           │
   │   ├ archives  │  ...                       │  PDF page │
   │   ├ scans     │                            │           │
   │   └ …         │                            │           │
   └───────────────┴───────────────────────────┴───────────┘
   ```

   * **Row density** defaults to compact (24px), toggle to comfortable
     (40px) — a 24px row × ~30 visible rows = the entire viewport's
     DOM cost is bounded regardless of dataset size.
   * **Column registry**: the set of columns the user can show is
     declared once in TypeScript (id, label, accessor, default
     visibility, default width, sortable). User toggles + reorders
     persist in the store via `getSetting('catalogColumns', defaults)`.
   * **Header sort**: clicking a header re-issues the query with the
     new `SortSpec`. Free because the scalar index sorts on the
     column server-side.
   * **Search-as-filter**: the Suche tab's hit list, when piped
     through "Show in Catalog", becomes a `doc_id IN (…)` filter
     chip on the Übersicht — same virtualised pane, just filtered.

#### Performance budget

| operation | target | how we get there |
|---|---|---|
| open Übersicht (cold) | < 300 ms | first page = 200 rows, total estimate runs in parallel |
| scroll | < 50 ms / frame | virtualisation; only render viewport ± 5 rows |
| change sort | < 200 ms | scalar index hit, server-side ORDER BY |
| change filter chip | < 200 ms | scalar index hit; cursor reset |
| folder-tree expand | < 100 ms | one `index_folder_children` call per level |
| preview render (PDF page 1) | < 500 ms | reuse existing extractor; lazy on selection |

#### Migration path (incremental commits, in priority order)

1. ✅ **`index_query_documents`** Rust command (commit `4ecfd7a`).
   `DocumentFilter` / `SortSpec` / `PageSpec` / `PageCursor` types
   in `index/schema.rs`, implementation on `LocalIndex` in
   `index/local_index.rs`, Tauri command + `lib.rs` registration.
   `total_estimate` via `count_rows` against the same predicate.
   **Implementation detail diverges from the design above:**
   `PageCursor` currently encodes an offset, not a keyset. Reason:
   LanceDB 0.26's public Rust query API doesn't expose `ORDER BY`,
   so a keyset cursor `(sort_value, doc_id) <op> (cursor)` would
   only work on the first page. Step 5 / step 6 swap this for a
   real keyset by dropping to `lance::dataset::Dataset::scan` (the
   layer under `lancedb`, which exposes Datafusion-backed
   `order_by` + `limit` + `offset`). The cursor wire format is
   the only thing that changes; callers pass the cursor through
   unchanged. A 50k-row hard cap is in place until then so we
   don't accidentally materialise an entire 10M-row table to
   re-sort the window in-process.

2. ✅ **Columnar Übersicht** (commit `9cbe0c1`). CSS-grid table
   (single `grid-template-columns` shared between thead + every
   row), sticky header with sortable columns (filename / author /
   year), server-side filter+sort+pagination via
   `index_query_documents`, "Load more" button, multi-row select
   (single-click / Shift-range / Ctrl-or-Cmd-toggle, mirrored
   from `BatchReview.svelte`'s pattern), `user-select:none` so
   dragging selects rows instead of highlighting filename text.
   No TanStack-Virtual yet — the page-of-200 model is enough at
   the current scale; full virtualisation comes with step 5.

3. **Promote `parent_dir` to a column** + scalar index on it.
   Backfill existing rows. Folder filter chip switches from
   `metadata_json LIKE` (today's hack) to a column-indexed
   equality scan. First step that lifts the 50k cap meaningfully
   for path-prefix filtered views.

4. **Folder-tree pane** + `index_folder_children`. Becomes the
   primary navigation; the path chip is now a click on a tree node.

5. **DB-side ordering via `lance::Scanner`**. Drop down to
   `lance::dataset::Dataset::scan` to get a real Datafusion
   query with `order_by` + `limit` + `offset`. Keyset cursor
   replaces offset; 50k cap goes away. Combine with TanStack-
   Virtual on the frontend so the visible row window stays
   bounded regardless of dataset size.

6. **Column registry** + persistence. User can toggle Title /
   Author / Year / Size / Mtime / Volume / Path / Language /
   Tags. State persisted via `tauri-plugin-store`.

7. **Promote `volume_id` + `source_kind`** to columns + indexes
   (last because they have the smallest filter-cost win and the
   biggest schema-migration cost).

8. **Preview pane** wired to existing `extractPdfNative` /
   `extractDocxText` paths, lazy-rendered on row selection.

Each step is independently shippable — the UI never breaks mid-flight,
because the Rust command keeps returning the same `SearchResult`-shaped
rows (just paginated). Steps 3 and 7 require a schema migration; the
existing `IndexConfig::dims`-driven schema rebuild is the same hammer
we use today when the embedder model changes.

#### Open UX follow-ups (post-merge cleanup)

These came out of the post-merge user-walkthrough and aren't yet
captured under their own phases:

* **`Catalog.svelte` (the `.caf` registry sub-tab) still has
  hard-coded English strings.** The `caf_catalog.*` i18n keys
  exist (EN+DE) but the component hasn't been wired through them
  yet. Same shape as the recent Duplicates pass.
* **Duplicates results-table actions** (`per-row tooltips, the
  bash/batch/ps1 format option labels`) — the form + script
  builder are i18n'd; a few internal action attributes still leak
  English.
* **Settings → bench panel + a few dialog strings** still hold
  inline literals from earlier sessions. Audit pass needed.
* **Catalog overview metadata for L3 rows.** L1 rows carry
  `fs_size` / `fs_mtime` / `parent_dir` in `metadata_json` so the
  Übersicht columns render. The L3 ingest path
  (`build_metadata_json` in `index/ingest.rs`) only writes
  `mtime_unix` + `volume_id` — so L3 rows show blanks for size
  and folder. Step 3 of P9 fixes this for parent_dir; size needs
  to be plumbed through `RawDocument` first.

---

(For historical per-version changelog and shipped phase specs, see
[HISTORY.md](HISTORY.md).)

