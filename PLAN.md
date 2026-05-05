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

### P10 — Robust ingest at scale: parallelism, timeouts, broken-task recognition

The current ingest pipeline processes one file at a time, blocks
indefinitely on slow / corrupt inputs, and has no notion of "this
task is wedged, kill it." At the catalog sizes P9 is sized for
(millions of files), three issues become inevitable:

1. **Throughput** — extraction is CPU-bound and OCR is GPU- or
   CPU-bound; doing them serially leaves cores idle.
2. **Hangs** — a single corrupt PDF with a malformed xref table
   can sit in `pdf-extract` forever. Today there's a per-file
   timeout configured in Settings (P8.1) but it doesn't fire
   until the file's *whole* budget is exhausted; you can't
   distinguish "slow but progressing" from "stuck since byte 1."
3. **Known-impossible inputs** — DRM-protected EPUBs, password-
   locked PDFs, scanned images with no OCR available — the
   pipeline retries them every run and produces the same error,
   wasting time and burying useful messages in the log.

#### What we already have

* **Catalog scanner** (`catalog/scan.rs`) is rayon-parallel via
  jwalk, so directory walking and SHA hashing already saturate
  cores. The bottleneck is downstream of the scanner.
* **Per-file conversion timeout** (P8.1, `conversionTimeoutSeconds`
  in Settings, default 120 s) wraps each `extractText` call in a
  `Promise.race` with a wall-clock alarm. Coarse but it works.
* **LLM round-robin** across remote providers
  (`roundRobinProviders` in Settings) already balances chat /
  classification queries across configured API keys.
* **Background ingest scheduler** (P7.4) with QoS throttling
  pauses ingest while the user is searching — orthogonal to
  this phase, but the same scheduler is the right host for
  the new worker pool.

#### Concurrency: parallel workers + the right back-pressure

* **Extraction worker pool** — N tokio tasks pulling from a bounded
  channel of `IngestEntry`s. N defaults to `min(num_cpus, 4)`;
  configurable per backend (PDF native ↔ JS, OCR ↔ ocrs, etc.) so
  a slow stage doesn't starve a fast one.
* **Embedder workers — single by default.** The fastembed and
  CrispEmbed backends both serialise GPU access internally, and
  multiple model instances would balloon RAM. The pool size
  here is "1 per active backend"; CPU-only ONNX with batch=32 is
  already at GPU-saturating throughput on the embedder side.
* **LanceDB writes — coalesce.** Today each chunk is written via
  `ingest_batch`. The worker pool produces batches of K chunks
  (K ≈ 64) which the writer coalesces into one Arrow `RecordBatch`
  per K-block. Tantivy commits on the same K boundary.
* **LLM round-robin** — extend the existing rotation so background
  classification (e.g. "infer subject from title") obeys the
  same provider list the chat panel uses. Today only the chat
  flow consults `roundRobinProviders`.
* **Back-pressure**: the channel between scanner and worker pool
  has a bounded capacity (1000 entries). When full, the scanner
  awaits — keeps memory usage bounded regardless of catalog size.

#### Timeouts at the right granularity

Layer the timeouts so a fast stage failure doesn't get masked by
a coarse per-file alarm:

| stage | budget | enforced where |
|---|---|---|
| **Stage timeout — first byte** | 60 s | extractor wrapper. If the extractor produces zero output (no first page from PDF, no first chunk from OCR) within 60 s, abort. Strong signal of a wedged file. |
| **Stage timeout — total** | per-stage configurable (default: PDF 5 min / OCR 10 min / DOCX 1 min) | extractor wrapper. Hard wall-clock cap. |
| **Whole-file timeout** | `conversionTimeoutSeconds` (Settings, default 120 s today; raised to 600 s when the per-stage caps land) | pipeline orchestrator. Catches bugs the per-stage timeouts miss. |
| **Embedder timeout** | 30 s per batch | call site. Embedder is pinned to one backend; if it hangs we want to see it surface, not retry forever. |
| **LLM query timeout** | 30 s default, settable per provider | already enforced upstream of round-robin (`abort_aware_rate_limit_sleep` etc.) |

Each timeout produces a structured `TaskFailureReason` so the
"broken-task" classifier below has something to work with.

#### Broken-task recognition — `TaskFailureReason` taxonomy

Every failure to ingest a file is bucketed into one of these:

| reason | source | retry-worthy? | UI badge |
|---|---|---|---|
| `Timeout::FirstByte` | first-byte stage timeout | no — 99% wedged file | red, "stuck" |
| `Timeout::Total` | total-stage or whole-file timeout | maybe — if config raised | red, "too slow" |
| `Encrypted::Drm { scheme }` | EPUB `META-INF/encryption.xml` present, or PDF `/Encrypt` dict | no — needs decryption key | yellow, "DRM" |
| `Encrypted::Password` | PDF user-password set | yes if user supplies password | yellow, "password" |
| `Corrupt::ParseError { stage }` | extractor returned a hard parse error in <2s | no | red, "corrupt" |
| `Unsupported::NoExtractor` | no extractor registered for this extension | no — needs new code | grey, "unsupported" |
| `Resource::OutOfMemory` | extractor died with OOM | no — file too large for current backend | red, "too large" |
| `Network::Unreachable` | only relevant once the cloud-storage location types are wired | yes after backoff | yellow, "offline" |
| `Other { msg }` | catch-all | no | red |

The classification happens at one place — the worker's per-stage
error handler — and the reason is persisted alongside the file's
row so the next ingest run can skip-or-retry per the taxonomy.

#### Graceful-degrade response: what to do when L3 is impossible

The user's example: *"EPUB extraction failed: The file is encrypted
with AES, but no symmetric key is provided."* The current pipeline
treats this the same as any other failure — the file ends up with
status `error` and a stack trace, and the user sees neither the
title nor a useful next step. Better:

1. **Always try L2 metadata first.** EPUB OPF (in
   `META-INF/container.xml` → `OEBPS/content.opf`) is *not*
   encrypted by ADEPT/FairPlay/B&N — only the chapter XHTML is.
   So the `dc:title` / `dc:creator` / `dc:date` fields are
   readable even when L3 fails. Same with PDFs that have a
   user-password set: the `/Info` dict + XMP packet sit outside
   the encryption envelope and `lopdf` can read them. Today
   `l2_metadata.rs` is invoked from a separate `index_promote_l2`
   command; the fix is to invoke it as a *fallback* from the L3
   path the moment the L3 extractor errors with one of the
   bucket reasons above.

2. **Persist the failure reason on the row.** New
   `metadata_json` field `extraction_failure: { reason, last_seen_at }`.
   Übersicht renders an icon next to the L-badge per the
   reason → badge column in the taxonomy table above.

3. **Don't silently retry.** The background ingest scheduler
   today re-tries every file every run. Add a "skip files with
   extraction_failure reason ∈ {DRM, Unsupported, Corrupt} unless
   the user explicitly clicks Retry" rule. `Timeout::Total` and
   `Network::Unreachable` stay retry-worthy.

4. **Surface a help link.** For `Encrypted::Drm`: the badge is
   clickable; the popover explains DRM, links to a help page,
   suggests Calibre + DeDRM as the typical workflow if the user
   has the legal right to decrypt their own purchases. We don't
   ship DRM removal; we point at it. (`Encrypted::Password`:
   prompt for the password and re-run.)

5. **CLI parity.** `crispsorter index ingest --skip-failed`
   honours the same skip rules so unattended bulk runs don't
   waste hours retrying DRM EPUBs.

#### Migration path

a. **`TaskFailureReason` enum** in `index/mod.rs` + the
   `extraction_failure` field on `metadata_json`. Wire L2 fallback
   in the L3 extractor on first-byte timeout / parse error.
b. **Per-stage timeout wrappers** (first-byte + total) in the
   extractor adapters. The `conversionTimeoutSeconds` Setting
   becomes a UI knob over the L3 *whole-file* budget; per-stage
   defaults stay sane.
c. **Worker pool** in the background-ingest scheduler. N
   configurable in Settings (default `min(num_cpus, 4)`).
d. **DRM detection** for EPUB + password-protected PDF. Surfaces
   the right reason even when the underlying extractor would
   otherwise give a generic error.
e. **Übersicht status badges** + the help-popover for `Drm`.
f. **CLI `--skip-failed`** flag and the `--retry-failed` complement.

Steps (a) and (b) are the load-bearing ones — once those land, the
"the file is encrypted with AES" message becomes a yellow DRM
badge in Übersicht with title + author still showing, and the
rest is incremental.

---

(For historical per-version changelog and shipped phase specs, see
[HISTORY.md](HISTORY.md).)

