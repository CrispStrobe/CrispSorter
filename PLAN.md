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
c. **Worker pool**. ✅ Foreground Stapel flow shipped
   (commits `b70ebae` + the N+M-worker upgrade in this commit
   sequence). N extraction workers + M LLM workers configurable
   in Settings (Extraktion / KI-Optionen, default 1+1 each,
   cap 16+16). Live worker / docs-per-min chip in the bottom-
   left nav next to the existing Stapel + DB stats. Background-
   ingest scheduler still TODO -- same shape, applied to the
   `bg_ingest` module instead of `batchManager.processAll`.

   The producer/consumer rewrite preserves the no-stalled-LLM
   property of the original two-phase loop while overlapping
   the phases:

   ```
   extractor producer ──► queue (bounded, ~8) ──► LLM consumer
        │ pulls items needing                      │ pulls ready
        │ extraction one at a time                 │ items, runs
        │ (preserves the existing per-stage        │ queryRR with
        │ timeouts + page watchdog)                │ round-robin
        └─ stops when this.items exhausted         │ fallback
                                                   └─ keeps draining
                                                      until queue
                                                      empty + producer
                                                      done
   ```

   Both run as `Promise.all([producer, consumer])`. An LLM stall
   stops draining the queue but doesn't stop extraction — the
   queue just fills to its bound, then extraction back-pressures
   on `await wakeProducer`. An extraction error doesn't block
   already-extracted items; the consumer keeps draining. Stop
   button aborts both cooperatively via the existing
   `extractionAbort` + `llmAbort` signals.

   This lands first as `Promise.all` over single-worker
   producer + single-worker consumer (the easy correctness
   win); the actual N-worker pool follows once the failure
   taxonomy from step (a) is in place so a wedged worker can
   be killed without taking the pool with it.
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

### P11 — Remote-server architecture for terabyte scale

P10 sized the foreground client for "millions of files on one
machine"; P11 is the server side: a user with a multi-TB archive
who runs CrispSorter on their laptop, the actual index on a
beefier VPS or in-house GPU box.

**Reference architecture (`../CrispLens` v4) studied 2026-05-06.**
CrispLens already ships the multi-mode pattern we want — three
runtime modes (Server / Standalone-PWA / Desktop-Electron) in one
codebase, a unified `api.js` that switches adapters based on
mode, a `SyncManager` for bidirectional cache sync (pull
metadata+thumbnails, push offline-queued work on reconnect), and
a `cloud_drive_manager` that abstracts SMB / SFTP / Filen /
Internxt as mountable "drives." The patterns below are translated
to CrispSorter's Rust + Svelte + Tauri shape. Eventual
convergence (one Tauri suite that handles documents and images,
sharing the same server + sync + cloud-drive layer) is sketched
in P14.

Six pillars: the original three from "scale concerns" (queue,
server-side embedding, IVF-PQ at scale) plus three borrowed from
the CrispLens reference (modes, cloud drives, sync).

#### Where we are today

* `IndexBackend` trait already abstracts local vs remote.
  `BackendType::Local` (default) drives `LocalIndex` (LanceDB +
  Tantivy on disk); `BackendType::Remote` drives `RemoteClient`
  (HTTP).
* `RemoteClient` (`src-tauri/src/index/remote_client.rs`) is
  wired and protocol-documented:
  `POST /v1/ingest` (per-chunk, includes pre-computed embedding),
  `POST /v1/search`, `POST /v1/docs/:id/location`,
  `DELETE /v1/docs/:id`, `GET /v1/stats`, `GET /health`.
* `crisp-index-server` (Axum) is a documented skeleton with stub
  handlers — the wire shape is defined; the LanceDB / Tantivy
  glue on the server side is not yet written.
* The client today **always** embeds locally regardless of the
  backend (`Local` or `Remote`). Remote-mode posts the
  pre-computed vector with each chunk.
* Each `index_ingest_document` call is one full extract → embed
  → write cycle, and remote-mode does one POST per chunk. For
  100 k files × ~10 chunks = 1 M HTTP round-trips before any
  server-side queue.

#### Pillar 1 — Async ingestion queue (server-side, 202 Accepted)

The user is right: per-chunk synchronous POSTs don't scale. The
shape we want:

```
client                                    crisp-index-server
  │                                              │
  │  POST /v1/ingest/batch  (N chunks)           │
  │  Authorization: Bearer …                     │
  │ ─────────────────────────────────────────►   │
  │                                              │ SQLite-queue.push(payload)
  │                                              │ ───────────────────────┐
  │                                              │                        │
  │                              ◄─── 202 Accepted, body { task_id }      │
  │                                              │                        │
  │  GET /v1/tasks/{task_id}                     │                        ▼
  │ ─────────────────────────────────────────►   │  worker thread:
  │                                              │   pop → embed (if needed) →
  │                              ◄─── { state, progress, error? }         LanceDB.add → Tantivy.commit
```

Implementation:
* New endpoint `POST /v1/ingest/batch` accepts `IngestBatch
  { chunks: Vec<IngestPayload> }`. Returns `202 Accepted` with
  `{ task_id, queue_depth }` immediately.
* Persistent task queue. Start with **SQLite** (sqlx + a
  `tasks(id, payload, state, error, created_at)` table) — it's
  already the right tool and survives server restarts. Redis is
  the upgrade for multi-writer fanout if/when the server itself
  becomes the bottleneck.
* Single writer thread drains the queue; LanceDB + Tantivy both
  prefer one writer at a time anyway.
* `GET /v1/tasks/{id}` reports `queued | processing | done |
  failed`. Client batches polling every ~2 s while a run is
  active.

Bonus — bulk delete + bulk update-location land in the same
queue, by the same path. Today they're synchronous which means
"move 50k sorted files" stalls the UI for minutes.

#### Pillar 2 — Optional server-side embedding

Today client always embeds. For TB scale where the client is a
laptop and the index lives on a GPU box, that's backwards. New
config:

```
embedderLocation: 'client' | 'server'
```

When `client` (default, today's behaviour): client extracts +
embeds + posts pre-computed vector. Privacy-preserving; works
when the user runs the whole stack on one machine.

When `server`: client extracts + chunks, posts **raw text only**.
Server has its own `Embedder` (`crispembed-cuda` if GPU, fall
back to fastembed-CPU otherwise) and embeds in batches before
writing. New endpoint shape:

```
POST /v1/ingest/batch
  body: IngestBatch where chunks[i].embedding may be null
  202 ← { task_id }
```

Server worker thread checks each chunk: if `embedding` is null
*and* `config.embedderLocation == 'server'`, batches into a
GPU-friendly group of (e.g.) 64 chunks and runs the embedder.
Otherwise writes verbatim.

The split lets a single user mix: laptop runs in `client` mode
on its own files for privacy, but a backfill from "the entire
archive folder on the NAS" runs in `server` mode so the GPU box
chews through it overnight.

#### Pillar 3 — IVF-PQ at 100M+ vectors

The user's concern is real. LanceDB's IVF-PQ build runs K-Means
clustering over the vector column, which by default loads the
column into memory. At 1024-dim float32 × 100 M vectors = ~400
GB raw — won't fit, even on the beefiest VPS.

The fix is in the LanceDB API (modulo version availability):

```rust
IvfPqIndexBuilder::default()
    .distance_type(DistanceType::Cosine)
    .num_partitions(K)         // sqrt(N) is a reasonable default
    .num_sub_vectors(D / 8)    // 8-bit PQ codes, 1024d → 128 sub-vectors
    .sample_rate(SAMPLE_RATE)  // ★ what we need: K-Means trains on a sample
    .max_iters(50)
```

`sample_rate` exists in newer LanceDB (post-0.7); we're on 0.26
and need to verify the exact API. If not exposed at the LanceDB
layer, we drop to the lance crate directly (`lance::index::vector::ivf`)
which has had the sample-rate knob for longer.

Operational shape:
* Don't rebuild on every ingest. Threshold-driven: when row count
  crosses a power-of-2 milestone (1 M, 4 M, 16 M, …) **or** the
  user explicitly clicks "Re-index" in admin, schedule a build
  task.
* Build is itself a queue task — runs in the same worker thread,
  blocks new ingests until done (or gets a separate worker if
  contention shows up).
* Sample size needs tuning. LanceDB upstream recommends
  `100 * num_partitions` rows as the floor; that's typically
  `100 * sqrt(N)` so for 100 M vectors → 1 M sample rows = ~4 GB
  of vectors in RAM during the build, manageable.

#### Pillar 4 — Runtime modes (CrispLens parity)

CrispLens v4 supports three explicit modes; CrispSorter today
has effectively one (Tauri desktop with a stub `BackendType::Remote`).
The target is the same triad, namable in Settings:

| mode | DB | Inference | UI | Use case |
|---|---|---|---|---|
| **Standalone** | local LanceDB+Tantivy | local (CrispEmbed/fastembed/llamacpp) | Tauri desktop | "Just my MacBook." Offline. Privacy by default. |
| **Server** | remote (`crisp-index-server`) | remote (server-side embedder) | Tauri desktop *or* a hosted web UI | "My VPS holds everything." |
| **Hybrid** | local cache + remote authoritative | client embeds where viable, server elsewhere | Tauri desktop | "Laptop + Hetzner VPS + StorageBox." Mid-term default for power users. |

A fourth mode — **Browser-only PWA** — is what CrispLens calls
"Standalone (browser)" with WASM SQLite + IndexedDB. Out of
scope for CrispSorter v1 because LanceDB doesn't have a WASM
build yet, but the PWA shell is shippable for the search-side
read-only view (see P14 convergence note).

Modes are a runtime switch, not a build target. The same binary
runs all three; what changes is which `IndexBackend` impl is
wired and whether the embedder loads. Implementation:

* Today's `BackendType::Local | Remote` becomes
  `RuntimeMode::Standalone | Server | Hybrid` (additive — Hybrid
  is new). Standalone == today's "use local backend, embed
  locally"; Server == today's "remote backend, embed locally"
  (will become "remote backend, server embeds" once P11 step 5
  ships); Hybrid is two backends behind one trait.
* `HybridBackend` wraps `LocalIndex` + `RemoteClient`. Reads go
  local-first, fall through to remote for misses. Writes go to
  whichever side the user picked as authoritative for the
  catalog they're working in (per-catalog setting). The
  `SyncManager` (Pillar 6) keeps them consistent.
* Settings panel: a "Runtime mode" picker at the top of Such-
  Index. Changing it triggers a re-init — same flow as switching
  backends today.

#### Pillar 5 — Cloud-drive abstraction

CrispLens's `cloud_drive_manager.py` exposes SMB / SFTP / Filen /
Internxt as mountable drives — credentials are encrypted at
rest, sessions are cached in memory, and a "mount point" is
either a local path (SMB/SFTP via OS mount) or a virtual API
adapter (Filen/Internxt via their CLIs). CrispSorter today has
none of this — files come from `tauri-plugin-fs` only.

Architecture:

* New Rust module `src-tauri/src/drives/` with traits and impls:
  * `trait CloudDrive` — `list_dir(path)`, `read_file(path)`,
    `write_file(path, bytes)`, `stat(path)`, `delete(path)`.
  * `mod smb` / `mod sftp` — wrap a real OS mount; CloudDrive
    just delegates to `tauri-plugin-fs` against the mount point.
  * `mod filen` / `mod internxt` — subprocess to the official
    CLIs (`internxt-cli`, `filen-cli`) for the API ops; same
    bridge pattern CrispLens uses to its Python CLIs but
    Rust→exe instead of Python→exe.
* Credentials encrypted at rest in a Tauri-side keychain (one
  symmetric key per install, `keyring` crate or
  `tauri-plugin-stronghold`).
* New Tauri commands: `drive_list / drive_create / drive_mount
  / drive_unmount / drive_test`. Same shape as CrispLens's
  `routers/cloud_drives.py`.
* Übersicht filter chip "Volume" gains a "Mount" item
  alongside the OS volumes — a Filen archive shows up as a
  filter target the same way an SSD does.
* Ingest: scanner uses the `CloudDrive` trait so an Internxt
  folder can be L1-indexed without local mirror; L3 streams
  files as needed. `crisp+filen://...` and `crisp+internxt://...`
  URI schemes already reserved in `location.rs`.

Order: SMB + SFTP first (both are "mount the OS path, point
existing fs reads at it" — minimal new code). Internxt + Filen
are heavier (require shipping/spawning their CLIs); we add them
behind the `cloud-drives-cli` Cargo feature so users on minimal
installs can opt out of the binary bundling.

#### Pillar 6 — SyncManager (local ↔ remote)

CrispLens's `SyncManager.js` is the model. Two stores in
IndexedDB (images metadata + people / embeddings + a
`pending_push` outbox); two operations (`sync()` pulls a recent
window of metadata + thumbnails; `pushPending()` flushes the
offline outbox to the server with retry counters). CrispSorter
needs the analogous shape, but in Rust + LanceDB instead of
JS + IDB:

```
   ┌───────────────────────┐     pull (server→local)     ┌─────────────────┐
   │ local LanceDB+Tantivy │ ◄────────────────────────── │  crisp-index-   │
   │                       │     sync_state.last_pull_ts │     server      │
   │ + sync_outbox table   │ ──────────────────────────► │                 │
   └───────────────────────┘   push (local→server)       └─────────────────┘
                                + retry queue
```

Concrete shape:

* New table `sync_outbox(id, op, payload, retries, last_err,
  queued_at)` in the same SQLite database as the catalog
  metadata. `op` ∈ {ingest, delete, update_location}.
  Operations that today block on the remote round-trip become
  fire-and-forget (write to outbox, return), with the outbox
  worker draining it asynchronously.
* `sync_state` row tracks `last_pull_ts`, `last_pull_cursor`,
  `last_push_ts`, `pending_count`. Surfaced in the bottom-left
  chip from P10.
* Pull operation: `GET /v1/sync/since?ts=<last>&limit=N` returns
  the metadata-row delta — additions, modifications, deletions
  — since the timestamp. Local LanceDB applies them.
  **Importantly:** pull doesn't transfer embeddings unless the
  user opts into "full mirror" mode; the default is
  metadata-only ("offline-readable index") which is enough for
  the Übersicht columnar view to show every doc.
* Compact embeddings: borrowed from CrispLens
  `/api/people/embeddings`. Server exposes a "representative
  embedding per author / per topic cluster" so basic local
  search works offline without dragging the whole vector
  column down. Today CrispSorter doesn't have these clusters —
  P14 reranker work would compute them.
* Reconnect detection: a tokio task pings `/v1/health` every
  30 s. On `200` after a streak of failures, runs `pushPending`
  + `sync` automatically. UI shows a small green/yellow/red
  pill ("synced" / "syncing" / "offline").
* Conflict resolution: server is authoritative for hash
  collisions on `doc_id`. If a row was modified locally
  (Übersicht edit) AND remotely between pulls, the server's
  version wins; the local edit is queued in `conflict_log` for
  the user to review (rare in practice — locks on edit-in-flight
  prevent most cases).

The outbox doubles as the foreground producer/consumer queue
from P10/P11 step c. One implementation, two consumers (local
writer and remote pusher).

#### End-state architecture

```
                ┌─────────────────────────────────────────────────────┐
                │   user laptop (Tauri desktop app)                   │
                │   ───────────────────────────────                   │
                │   * Stapel UI + Catalog Übersicht                   │
                │   * extractor pool (P10)                            │
                │   * embedder ONLY if mode='client'                  │
                │   * llmClient (OpenAI / Anthropic / local llama)    │
                │   * IndexBackend trait → RemoteClient OR LocalIndex │
                └────────────┬────────────────────────────────────────┘
                             │
                             │ POST /v1/ingest/batch (raw text or vectors)
                             │ POST /v1/search       (text + maybe vector)
                             │ GET  /v1/tasks/:id    (poll progress)
                             ▼
                ┌─────────────────────────────────────────────────────┐
                │   crisp-index-server  (Axum + tokio)                │
                │   ──────────────────────────────                    │
                │   * /v1/ingest/batch → enqueue → 202                │
                │   * /v1/search       → fan out to local LanceDB     │
                │   * /v1/tasks/:id    → SQLite read                  │
                │                                                     │
                │   workers (tokio tasks):                             │
                │     ingest_writer  → SQLite-queue → embed (opt) →   │
                │                       LanceDB.add → Tantivy.commit  │
                │     reindex_worker → IVF-PQ rebuild on milestones,  │
                │                       sample_rate-bounded K-Means   │
                │                                                     │
                │   storage:                                           │
                │     LanceDB  (or sharded Lance datasets in v3)      │
                │     Tantivy  (single index in v1; per-shard in v3)  │
                │     SQLite   (task queue + admin metadata)          │
                └────────────┬────────────────────────────────────────┘
                             │
                             ▼
                ┌─────────────────────────────────────────────────────┐
                │   storage volume (SSD or NVMe array)                │
                │   * /var/crispsorter/lance/...                      │
                │   * /var/crispsorter/tantivy/...                    │
                │   * /var/crispsorter/queue.sqlite                   │
                └─────────────────────────────────────────────────────┘
```

Sharding (only if a single Lance dataset can't keep up):
* Partition by `volume_id` — already a column in LanceDB. Each
  shard is its own dataset. Search fans out, merges with RRF.
  Eyeballed scale at which this matters: ~500 M vectors, or the
  point where a single Lance scan + IVF-PQ index can't fit on
  one box's SSD.

#### What "build modularly toward this NOW" means

Before P11 starts in earnest, three small refactors lower the
eventual port cost. Each is independently shippable:

1. **`IngestBatch` shape across the whole pipeline.** Today the
   producer/consumer in `batch/store.svelte.ts` produces one
   `BatchItem` at a time → `index_ingest_document(per-chunk)`.
   The right next layer is: producer emits N `IngestPayload`s
   in one go to `index_ingest_batch(items)`. The local backend
   coalesces them on the LanceDB write; the remote backend
   POSTs them as one `/v1/ingest/batch` call. Same pipeline,
   one swap point.

2. **`embedderLocation: 'client' | 'server'`** as a config flag
   (not yet a feature toggle). Default `'client'`. The pipeline
   reads it before deciding to load an embedder; `'server'`
   short-circuits the local embedder entirely and posts raw
   text. We already had `use_vector` (no embedder at all);
   this adds the third state.

3. **Make the local backend a queue too.** Today the local
   write path is synchronous (caller awaits each chunk). If we
   wrap it in a tokio mpsc + a single writer task, the same
   "fire-and-forget plus poll" shape that the server will use
   already works locally — and the UI gets non-blocking writes
   for free. The local writer's queue depth becomes the same
   metric we'll show for remote-mode queue depth in the
   bottom-left chip from P10's throughput indicator.

These three are net-no-feature-loss but turn the eventual
client/server cutover from "rewrite the ingest path" into "swap
the implementation behind one trait method."

#### Migration order

1. **Refactor (a):** `index_ingest_batch` Tauri command + LocalIndex
   coalesced writes. Foreground Stapel pipeline (P10) feeds it
   chunks-at-a-time instead of one-doc-at-a-time. **No server work
   yet.** Client gets faster local ingest as a side effect (Arrow
   record-batch overhead amortised).

2. **Refactor (b):** `embedderLocation` config + the load gate.
   Default `'client'`; the second value becomes meaningful in
   step 5.

3. **Refactor (c):** local writer queue. UI sees real queue depth
   even in single-machine mode. Sets up the abstraction the
   remote queue will land in.

4. **Server step 1 — bulk ingest API.** Build out the
   `crisp-index-server` stubs: `POST /v1/ingest/batch` returns
   202 + task_id, SQLite-backed queue, single writer task that
   actually writes (still expects pre-computed embeddings).
   Client switches to bulk POSTs in remote mode.

5. **Server step 2 — server-side embedding.** Add the embedder
   to the server worker, gated by `embedderLocation == 'server'`.
   Client stops embedding when this is on.

6. **Server step 3 — IVF-PQ with sample_rate.** Background
   reindex worker, threshold-driven. Drop to the `lance` crate
   if `lancedb 0.26` doesn't expose the knob yet.

7. **Server step 4 — sharding by `volume_id`.** Only when needed.

Steps 1-3 are pure client refactors that ship CrispSorter
desktop without any server work and pay off immediately
(non-blocking writes, bulk Arrow batches, queue-depth
visibility). Steps 4+ are the actual server build-out, but the
client is already shaped right by then.

Three additional steps cover the CrispLens-parity pillars; they
slot in alongside the original seven, not after:

8. **`RuntimeMode` + `HybridBackend`.** Replaces today's two-state
   `BackendType::Local | Remote` with `Standalone | Server |
   Hybrid`. `HybridBackend` reads local-first, falls through to
   remote on miss; writes go to the per-catalog "authoritative
   side." Pairs naturally with step 4 (bulk ingest) — Hybrid
   mode means most-requested writes hit local + are queued for
   remote replication. Settings UI: a single mode picker at the
   top of the Such-Index panel.

9. **`sync_outbox` + reconnect detector.** New SQLite table for
   the offline write queue, a tokio task that pings
   `/v1/health` and drains the outbox on reconnect, a
   metadata-delta `/v1/sync/since?ts=…` endpoint on the server
   side. Surface "offline / syncing / synced" pill in the
   nav-bottom area next to the existing throughput chip. This
   is the load-bearing CrispLens-parity step — once it ships,
   the laptop-and-VPS workflow becomes seamless.

10. **`CloudDrive` trait + SMB/SFTP impls.** The minimal viable
    cloud-drive abstraction (mount the OS path, point existing
    `tauri-plugin-fs` reads at it). Adds `crisp+smb://...` and
    `crisp+sftp://...` to the URI scheme. Internxt + Filen land
    behind the `cloud-drives-cli` Cargo feature in a follow-up;
    they require bundling/spawning the official CLIs.

The total order, optimised for "every step ships value while
shaping the next one":

```
  P11 step  | what                                | ships value                                 | unblocks
  ──────────┼─────────────────────────────────────┼─────────────────────────────────────────────┼─────────────
  1 (refac) | index_ingest_batch                  | faster local ingest (Arrow batching)        | step 4
  2 (refac) | embedderLocation flag               | infrastructure                              | step 5
  3 (refac) | local writer queue                  | non-blocking writes, queue-depth chip       | step 9
  8         | RuntimeMode + HybridBackend         | Settings exposes 3 modes                    | step 9
  9         | sync_outbox + reconnect detector    | "online/offline/syncing" pill                | step 4
  4         | server bulk ingest API              | remote-mode 100x faster                     | -
  5         | server-side embedding               | TB-scale workflows (laptop + GPU box)       | -
  6         | IVF-PQ with sample_rate             | 100M+-vector search latency stays ms-class  | -
  10        | CloudDrive (SMB/SFTP)               | Hetzner StorageBox + NAS as first-class     | (Internxt/Filen later)
  7         | sharding by volume_id               | only when needed                            | -
```

### P12 — Image-vertical convergence with CrispLens

CrispSorter today indexes images at L2 (EXIF) but never embeds
them — no "find similar," no face recognition, no CLIP-style
text-to-image search. CrispLens has all three. Long-term we
either (a) merge the two codebases into one Tauri suite with a
"Documents" mode and an "Images" mode, or (b) keep them
separate but share the server + sync + cloud-drive layer from
P11.

P12 is option (a) — converge into a single tool. Justified
because:

* Most users hitting a TB-scale archive have *both* — papers
  and photos in the same Fachbereich folder.
* The server-side stack (LanceDB + Tantivy + reranker + sync
  manager + cloud drives) is identical regardless of payload
  type. Two front-ends maintaining their own copies of that
  stack is duplicated work.
* CrispLens's Electron app v4 is already pure Node/Express +
  ONNX (Python-free). Porting its core to Tauri/Rust + ORT
  reuses the inference path CrispSorter already uses
  (`ort = "=2.0.0-rc.11"`, the same `ort` crate version).

Concrete steps (long-term — not for the next release):

1. **CLIP-style image embedder** — `EmbedderModel::ClipB32`
   variant alongside the existing text embedders. CLIP ViT-B/32
   ships ONNX in the open-clip repo; ~150 MB per backbone, fits
   the existing `ort`/CrispEmbed pipeline. New
   `IngestPayload.is_image` flag routes images through the
   image branch on both client and server.
2. **Face recognition module** — port CrispLens's SCRFD + ArcFace
   pipeline (`renderer/src/lib/face-engine.js`) to a Rust
   `crispface` sibling crate using `ort`. Same ONNX models,
   same 512D representation, same `person_embeddings` table
   shape so the SyncManager can pull them.
3. **UI**: add an "Images" tab alongside "Stapel" / "Kataloge",
   with the gallery / lightbox / face-clustering UI from
   `electron-app-v4/renderer/src/lib/`. Shares the same
   IndexBackend, the same sync chip, the same cloud drives.
4. **One installer.** Single Tauri binary with both verticals;
   the current document-only and image-only installers become
   feature-gated builds for users who only want one.

Out of scope for now. But P11's modularity (especially steps
1-3 + 8-10) is what makes it eventually possible without
re-doing every component. The right framing today: keep
`IndexBackend` / `RuntimeMode` / `SyncManager` payload-shape-
agnostic so that "an image with EXIF + CLIP vector + face
embeddings" is just a different `DocumentChunk` flavour later,
not a separate stack.

---

(For historical per-version changelog and shipped phase specs, see
[HISTORY.md](HISTORY.md).)

