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

- **P11 step 5 — UI polish + server queue blob fix** — see open items below.
- **P11 step 5 — server-side embedding hardening** — the initial path is live behind `SERVER_EMBED=1`: remote mode can now post chunks with empty `embedding` vectors and the server fills them with BGE-M3 before writing, and remote vector/hybrid search can omit query embeddings too. Remaining work is operational hardening (model/config selection, better progress reporting, and integrating this into the eventual file-level SQLite queue design).

## Recently Shipped

- **Sparse retrieval in the search pipeline** — hybrid search now opportunistically adds a sparse BGE-M3 / SPLADE channel when the active embedder exposes a sparse head.
- **Cross-encoder reranking in the search pipeline** — `SearchEngine` can rerank post-RRF candidates via `RerankerHandle`, with Settings/UI wiring for model selection and top-N.
- **P11 step 1 — batched ingest across the frontend + local backend** — the Svelte call sites now buffer documents into `index_ingest_batch` calls (N=16), coalescing LanceDB writes and Tantivy commits.
- **P11 step 2 — `embedderLocation: client | server`** — shipped end-to-end in Rust + Settings UI; remote mode can post raw text for a future embedding-capable server.
- **P11 step 3 — local single-writer queue** — `IngestPipeline` now serialises LanceDB/Tantivy mutations through one tokio writer task and exposes queue depth via `index_queue_depth`.
- **P11 step 4a/4b bridge — remote bulk-ingest queue** — `crisp-index-server` accepts `POST /v1/ingest/batch`, persists queued tasks in `crisp_jobs.db`, drains them through one background worker with lease/heartbeat/retry semantics, and exposes `/v1/tasks/:id`; the Tauri remote batch path enqueues and polls to completion instead of erroring. The server now auto-imports legacy `ingest_tasks.json` state on upgrade, claims work with `BEGIN IMMEDIATE` + `UPDATE ... RETURNING`, and exposes optional operator metadata for attempts / lease timing on task-status reads.
- **P11 step 4d — UI wired to durable job queue** — `IndexIngest.svelte` (Hinzufügen tab) now routes all file additions through `jobs_create` (`job_type: 'hinzufuegen'`) + `jobs_add_files`, and the L1/L2/L3 ingest loops use `jobs_claim_batch` → process → `jobs_mark_done/error/skipped`. On mount, the component restores any active `hinzufuegen` job via `jobs_list` + `jobs_reclaim` (resets in-progress rows) + `jobs_list_files` (repopulates display). `clearAll` calls `jobs_delete`; `clearDone` calls `jobs_remove_files_by_status`; `removeEntry` calls `jobs_remove_file`. Three new Tauri commands added (`jobs_list_files`, `jobs_remove_file`, `jobs_remove_files_by_status`); `jobs_create` gains an optional `job_type` parameter. `BatchReview.svelte` (Stapel) already has session persistence via `batchManager.saveCurrentSession()` and doesn't require the same migration.
- **P11 step 4c — client durable file-level queue** — `src-tauri/src/jobs/` ships `JobQueue` (rusqlite WAL, bundled SQLite) with a two-table schema: `ingest_jobs` (one row per logical batch job — type, status, source paths, target level, timestamps, error) + `file_queue` (one row per file — per-file status `pending|in_progress|done|skipped|error`, retry count, doc_id, error message; `INSERT OR IGNORE` dedup on `(job_id, path)`). Key methods: `claim_batch` (immediate `BEGIN IMMEDIATE` tx), `mark_done`, `mark_error` (requeue up to `max_retries` or terminal error), `reclaim_in_progress` (reset stale in-flight rows on startup). Wrapped in `Arc<Mutex<Option<JobQueue>>>` in `AppState`, initialised in the Tauri setup hook from `app_data_dir`. Exposed via 13 `jobs_*` Tauri commands (all use `spawn_blocking`). `mtime_unix` promoted from `Option<u32>` to `Option<i64>` (Y2038 fix) across `ingest.rs`, `bg_ingest/mod.rs`, `tauri_commands.rs`, `local_index.rs`. Remaining: wire the Svelte ingest flows to consume the queue.
- **P11 step 5a — initial server-side embedding** — with `SERVER_EMBED=1`, `crisp-index-server` loads a CPU-side BGE-M3 fastembed model and hydrates missing chunk embeddings before LanceDB/Tantivy writes. The remote Tauri ingest paths now send empty vectors when `embedderLocation = server`, and the remote vector/hybrid search path can let the server embed the query too when no local query embedder is loaded.

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

- [x] **Disk hygiene: redirect Cargo target dir to an external drive**
  — shipped (commit `10ecaab`). After the workspace promotion (commit
  `7326771`) `cargo build` writes to `/target/` at the repo root
  instead of `src-tauri/target/`. Six scripts still pointed at the
  legacy path and were silently recreating an orphan
  `src-tauri/target/` on every dev run. On our notebook this
  ate **26 GB** of pre-workspace artefacts that nobody was reading
  any more, pushing the boot drive to 99 % full / 6.4 GB free.

  Fix in two parts:

  1. **Script rewrites** (commit `4654c18`) — `enable-crispembed.{ps1,sh}`,
     `recompile-exe.ps1`, `release.sh`, `scripts/build.sh`, and
     `scripts/bundle_macos_native_libs.sh` updated to write to / look
     in the workspace-root `target/` first. Legacy
     `src-tauri/target/` paths kept as graceful fallbacks for
     branches that haven't picked up the workspace move yet.
  2. **`CARGO_TARGET_DIR` honoured** (commit `10ecaab`) — Cargo's
     standard env var. The DLL-staging code in
     `enable-crispembed.ps1` and the .exe-locator in
     `recompile-exe.ps1` now both read `$env:CARGO_TARGET_DIR` if
     set, falling back to `$ProjectRoot\target`. The user-facing
     "Staged N DLL(s) to ..." message reads from the same variable
     so the printed path is honest.

  **Workflow for a build with target on D:\\\\:**

  ```powershell
  $env:CARGO_TARGET_DIR = "D:\cargo-target\crispsorter"
  .\enable-crispembed.ps1 -Backend cuda
  ```

  Or persistently, in the PowerShell profile:

  ```powershell
  [Environment]::SetEnvironmentVariable(
      'CARGO_TARGET_DIR',
      'D:\cargo-target\crispsorter',
      [EnvironmentVariableTarget]::User)
  ```

  Same on Linux/macOS:

  ```bash
  export CARGO_TARGET_DIR=/Volumes/External/cargo-target/crispsorter
  ./enable-crispembed.sh --backend metal
  ```

  Notes:

  * The target drive must be NTFS (Windows) or APFS / ext4 / Btrfs
    (Unix). exFAT / FAT32 break Rust builds because some crates use
    symlinks at build time.
  * `CARGO_TARGET_DIR` is shared across *every* Cargo project — if
    you also build other Rust projects, give each one its own
    sub-folder (`D:\cargo-target\crispsorter`,
    `D:\cargo-target\some-other`, etc.) rather than pointing them
    all at `D:\cargo-target\` directly.
  * `scripts/build.sh` has a separate `CRISPSORTER_TARGET_VOLUME`
    env var that sets up a symlink at `$REPO_ROOT/target` →
    external SSD. Equivalent effect; preserved for users who'd set
    it up before `CARGO_TARGET_DIR` was wired.
  * To reclaim space at any time: `cargo clean` from the repo root
    drops the workspace target dir entirely (regenerated on next
    build).

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

3. ✅ **Promote `parent_dir` to a column** + scalar index on it.
   `parent_dir Utf8` added at schema position 25; `migrate_add_parent_dir_column`
   adds it non-destructively (AllNulls backfill) on existing tables.
   `build_scalar_index()` creates BTree indexes on `parent_dir` **and**
   `volume_id`. `filter_to_sql` now generates `parent_dir LIKE 'prefix%'`
   instead of the JSON LIKE hack. L1 ingest writes the column directly;
   L3 derives it from `p.parent()`. `SortColumn::ParentDir` added.
   `index_build_scalar_index` Tauri command + Settings UI button added.

4. ✅ **Folder-tree breadcrumb** + `index_folder_children` command.
   `LocalIndex::folder_children(parent, owner_id)` queries the BTree-indexed
   `parent_dir` column, groups by immediate child segment in Rust, returns
   `Vec<FolderChild> { name, path, doc_count }`. Tauri command
   `index_folder_children` exposes it. Übersicht filter bar now shows a
   clickable breadcrumb (root `/` → each path segment is a button) with a
   dropdown listing immediate subfolders + their subtree doc counts.
   Navigating a child sets `contentsFolder` (triggers server-side filter)
   and re-issues `index_folder_children` for the new parent. Full left-pane
   tree layout left for a future step.

5. **DB-side ordering via `lance::Scanner`**. Drop down to
   `lance::dataset::Dataset::scan` to get a real Datafusion
   query with `order_by` + `limit` + `offset`. Keyset cursor
   replaces offset; 50k cap goes away. Combine with TanStack-
   Virtual on the frontend so the visible row window stays
   bounded regardless of dataset size.

6. ✅ **Column registry** + persistence. `COLUMN_DEFS` array in
   `IndexIngest.svelte` declares 9 toggleable columns (Name, Autor,
   Jahr, Größe, Geändert, Ordner, Sprache, Volume, L). A `Columns2`
   icon button in the result-count bar opens a checkbox picker; user
   choices saved to `catalogCols` in `index-ingest.json` via
   `tauri-plugin-store`. The `--cat-cols` CSS variable on `.catalog-table`
   drives `grid-template-columns` on thead + every row (single point of
   truth). Two new columns exposed: `language` (from `doc.language`)
   and `volume` (from `doc.volume_id`, truncated to 8 chars + ellipsis).
   A `$effect` closes the picker on outside click.

7. ✅ **Promote `volume_id` to a column** + scalar index on it.
   `volume_id Utf8` added at schema position 26; `migrate_add_volume_id_column`
   adds it non-destructively (AllNulls backfill) on existing tables.
   `filter_to_sql` now generates `volume_id IN (...)` instead of the
   LIKE-on-JSON hack. `build_scalar_index` covers both columns. L3
   ingest writes the column directly from `RawDocument.volume_id`;
   L1 rows get `None` (L1FileEntry doesn't carry volume_id yet).
   `SearchResult.indexed_at` promoted to a real field (was doc_id fallback).
   `SortColumn::IndexedAt` now sorts by the real timestamp.

8. ✅ **Preview pane** wired to existing extractor paths, lazy-rendered
   on Eye-button click. `openDocPreview(doc)` resolves the URI via
   `uriToPath`, classifies the extension into `pdf | image | text |
   unsupported`, and either sets `convertFileSrc(path)` for native
   `<object>`/`<img>` rendering or reads up to 512 KB via
   `readTextFile` into a `<pre>`. The table area is now wrapped in
   `.overview-split` (flex row) → `.catalog-col` (flex column) +
   `<aside class="preview-pane">`. An Eye icon button appears in the
   actions cell; it highlights blue when that row's preview is open;
   clicking the same row again closes the pane.

Each step is independently shippable — the UI never breaks mid-flight,
because the Rust command keeps returning the same `SearchResult`-shaped
rows (just paginated). Steps 3 and 7 require a schema migration; the
existing `IndexConfig::dims`-driven schema rebuild is the same hammer
we use today when the embedder model changes.

#### Open UX follow-ups (post-merge cleanup)

These came out of the post-merge user-walkthrough and aren't yet
captured under their own phases:

* **`Catalog.svelte` tooltip fragments** (Alias / Serial / Free at
  scan / Archive flag set, lines ~417–420) — 4 keys needed in
  `caf_catalog.*` EN+DE. Main body is already wired; only these
  inline string-builder fragments remain.
* ✅ **Duplicates results-table actions** — per-row tooltips
  (Keep newest / Keep only / Delete all / Remove from list /
  Toggle Accept) and bash/batch/ps1 format option labels wired
  to `i18n.t.batch.dupe_*` / `i18n.t.duplicates.script_format_*`.
* ✅ **Settings → bench panel** — `embed_bench_*` and `alert_*`
  keys added EN+DE; all hardcoded literals in the embedding
  benchmark section replaced with `i18n.t.settings.benchmark.*`.
* ✅ **Catalog overview metadata for L3 rows** — `file_size: Option<i64>`
  added to `RawDocument`; `build_metadata_json` now emits `fs_size`
  and `fs_mtime` (ms) alongside `mtime_unix`; `tauri_commands.rs`
  and `bg_ingest/mod.rs` hoist `fs::metadata` and pass file size.

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
* `crisp-index-server` (Axum) **lives in this repo as a workspace
  member** (since the May 2026 integration). It's no longer a
  stub: real LanceDB writes via Arrow record batches, real Tantivy
  BM25 + delete-by-doc-id, real cosine ANN, real RRF (k=60) hybrid
  merge, real IVF-PQ admin endpoint. Deployed standalone via
  Docker / systemd; the workspace is purely the build system.
* Wire-format types (`IngestChunk`, `SearchRequest`, `SearchHit`,
  …) live in the `crisp-index-protocol` workspace crate so the
  client and server cannot drift on the JSON shape. Tests in the
  protocol crate pin the on-the-wire serialisation.
* The client today **always** embeds locally regardless of the
  backend (`Local` or `Remote`). Remote-mode posts the
  pre-computed vector with each chunk.
* Each `index_ingest_document` call is one full extract → embed
  → write cycle, and remote-mode does one POST per chunk. For
  100 k files × ~10 chunks = 1 M HTTP round-trips before any
  server-side queue.

#### Pillar 1 — Async ingestion queue ✅ shipped (both tiers)

**What shipped (steps 4a–4c):**

*Server tier (`crisp-index-server/src/queue.rs` — `TaskQueue`):*
SQLite-backed (`crisp_jobs.db`), `rusqlite` with WAL mode. Schema: `ingest_tasks(id TEXT PK, payload_json TEXT, state TEXT, attempt_count INT, max_attempts INT, lease_expires_at INT, last_heartbeat_at INT, created_at INT, done_at INT, error TEXT)`. `claim_next_task` uses `BEGIN IMMEDIATE` + `UPDATE … RETURNING`. Background worker loops with 30 s heartbeat lease; on crash/restart `reclaim_expired_tasks` resets stale rows. Operator env knobs: `QUEUE_LEASE_SECS`, `QUEUE_RETRY_BASE_MS`, `QUEUE_MAX_ATTEMPTS`. `GET /v1/tasks/:id` exposes `queued|processing|done|failed` + optional attempt/lease metadata. Legacy `ingest_tasks.json` auto-migrated on first boot. Known limitation (issue C): `payload_json` stores the full `IngestBatch` including pre-computed embeddings — ~112 KB per 16-chunk batch at BGE-M3 dims; 3.5 GB SQLite risk for a 500K-file backlog. Fix: store only chunk references; re-embed at work time.

*Client tier (`src-tauri/src/jobs/` — `JobQueue`):*
SQLite-backed (bundled rusqlite, WAL mode) in `app_data_dir`. Two tables: `ingest_jobs(id TEXT PK, job_type TEXT, status TEXT, source_paths TEXT, target_level INT, config_json TEXT, created_at INT, updated_at INT, error TEXT)` and `file_queue(id INT PK, job_id TEXT FK, path TEXT, status TEXT, retry_count INT, max_retries INT, doc_id TEXT, error TEXT, created_at INT, updated_at INT)` with unique index on `(job_id, path)` for `INSERT OR IGNORE` dedup. Key methods (all synchronous, called via `spawn_blocking` from Tauri commands): `create_job`, `add_files` (bulk insert, returns added count), `claim_batch` (`BEGIN IMMEDIATE` tx, marks rows `in_progress`), `mark_done`, `mark_error` (requeue or terminal), `mark_skipped`, `set_doc_id`, `reclaim_in_progress` (startup reset), `pending_count`. `Arc<Mutex<Option<JobQueue>>>` in `AppState`, initialised in Tauri setup hook. 13 `jobs_*` Tauri commands exposed.

**Remaining (step 4d):**
Wire Svelte ingest flows (`IndexIngest.svelte`, `BatchReview.svelte`) to consume `jobs_*` commands instead of ephemeral component state. Adaptive polling backoff in remote task poll path (issue F). Server queue blob-size fix (issue C above).

The original design spec (for reference):

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

The concern is real. LanceDB's IVF-PQ build runs K-Means
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

#### Pillar 7 — Unified payload format (multi-producer indexing)

`crisp-index-server` ends up with **three independent producers**
all feeding the same index:

1. **CrispSorter desktop** — Stapel / Hinzufügen flow.
2. **cloud-backup VPS worker** — opens 7z archives in transit on
   the way to Internxt; extracts text or generates
   thumbnails+embeddings for files not yet in the server's index.
   Output is much smaller than input (a 50 MB PDF becomes a few
   KB of extracted text + a 1 KB embedding), so cheap to ship.
3. **CrispLens** — once it migrates to the shared server (P13),
   produces image embeddings + face descriptors instead of text.

For this to work without each producer reinventing the wire format,
`/v1/ingest/batch` must accept a **discriminated-union payload**
that's media-agnostic. Rough shape:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IndexPayload {
    /// Text chunk (PDF/DOCX/EPUB/HTML extraction). Today's
    /// DocumentChunk is the seed of this variant.
    Text {
        doc_id: String,                // SHA-256(file content)
        location_uri: String,          // crisp+local / crisp+vps / crisp+cb-archive / ...
        owner_id: String,
        chunk_index: i32,              // -1 = whole-doc metadata row, 0 = first chunk, ...
        chunk_total: i32,
        full_text: Option<String>,
        full_text_md: Option<String>,
        embedding: Option<Vec<f32>>,   // null if embedderLocation == 'server' (server fills)
        embedding_sparse: Option<String>,
        embedding_model: Option<String>,
        // ── shared metadata block (same for every kind) ───────
        filename: Option<String>,
        title: Option<String>,
        author: Option<String>,
        year: Option<i32>,
        ext: Option<String>,
        language: Option<String>,
        page_count: Option<i32>,
        tags: Vec<String>,
        source_hash: String,
        indexed_at: i64,
        metadata_json: Option<String>, // forward-compat escape hatch
    },

    /// Image (JPEG / PNG / WebP / RAW thumbnail).
    Image {
        doc_id: String,
        location_uri: String,
        owner_id: String,
        // CLIP-style global embedding for "find similar"
        clip_embedding: Option<Vec<f32>>,
        clip_model: Option<String>,
        // Face descriptors (ArcFace 512D, one per face).
        // Empty when no faces detected or face engine disabled.
        faces: Vec<FaceDescriptor>,
        // Thumbnail bytes (JPEG, sized per server policy).
        // Optional: when None, server fetches from `location_uri`.
        thumbnail_jpeg: Option<Vec<u8>>,
        // ── shared metadata block (same fields as Text) ───────
        filename: Option<String>,
        title: Option<String>,
        ...
    },

    /// L1 manifest row -- filesystem metadata only, no content.
    /// Used by cloud-backup's manifest-import path so a 482k-file
    /// backup tree becomes browsable in seconds without any
    /// extraction.
    Manifest {
        doc_id: String,
        location_uri: String,
        owner_id: String,
        filename: String,
        ext: Option<String>,
        size: i64,
        mtime_ms: i64,
        ctime_ms: i64,
        parent_dir: String,
        volume_id: Option<String>,
    },
}
```

Why a tagged union and not three separate endpoints:

* **One ingest queue** on the server side. Same SQLite outbox,
  same writer thread, same retry semantics, same `task_id`
  reporting. Adding a new media kind = adding a variant + a
  `match` arm in the writer.
* **Same `doc_id` namespace.** A text doc and an image with the
  same SHA-256 (e.g. someone's PDF that's also been scanned and
  shipped as JPEGs) collide — the server keeps both rows
  (different `chunk_index` / `media_kind`) but the user sees
  them grouped in Übersicht as one logical file.
* **Same dedup check.** `HEAD /v1/docs/{doc_id}` returns 200 if
  any kind is already indexed. cloud-backup's VPS worker uses
  this to skip re-extraction: "do you already have this hash?"
  → "yes, kind=text, last indexed 2 weeks ago" → skip.

Server-side write path:

```
POST /v1/ingest/batch
  body: { payloads: Vec<IndexPayload> }
  202 ← { task_id, queue_depth }
                              │
                              ▼
            outbox writer task pulls payloads, dispatches:
              IndexPayload::Text     → run server-side embedder
                                       if .embedding is None →
                                       LanceDB.add(...) +
                                       Tantivy.add_document(...)
              IndexPayload::Image    → write to images table +
                                       store thumbnail blob;
                                       LanceDB.add(clip_embedding) +
                                       face_db.add(faces)
              IndexPayload::Manifest → metadata-only row in
                                       LanceDB (chunk_index = -1),
                                       no Tantivy entry yet
                                       (no text)
```

Client-side: the `IngestPayload` shape from
`src-tauri/src/index/remote_client.rs` becomes a thin alias /
re-export of `IndexPayload::Text`. CrispSorter's existing
`DocumentChunk` → `IndexPayload::Text` conversion is a single
function. Image variant ships when P13 lands. Manifest variant
ships with P12 step 12 (the cloud-backup manifest-import
command). The shape is forward-stable; new variants are
additive.

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

1. **`IngestBatch` shape across the whole pipeline.** Shipped.
   The Svelte ingest paths now buffer extracted docs and flush
   them via `index_ingest_batch` in groups of 16; the local
   backend coalesces them into one Arrow write batch and one
   Tantivy commit per flush. The remote backend still needs the
   matching HTTP bulk endpoint (`/v1/ingest/batch`) from step 4.

2. **`embedderLocation: 'client' | 'server'`** as a config flag.
   Shipped. Default `'client'`; in remote mode `'server'`
   short-circuits local embedder loading and posts raw text.
   This is infrastructure only until the server learns to embed
   missing vectors in step 5.

3. **Make the local backend a queue too.** Partially shipped.
   The local write path now runs through a tokio mpsc + a single
   writer task, so concurrent callers no longer race on LanceDB /
   Tantivy and the UI can poll real queue depth. What this does
   **not** give us is durable pause/resume or crash recovery:
   once the process exits, the in-memory queue disappears. The
   SQLite design below is the next step for days-long jobs.

These three are net-no-feature-loss but turn the eventual
client/server cutover from "rewrite the ingest path" into "swap
the implementation behind one trait method."

#### Migration order

1. ✅ **Refactor (a):** `index_ingest_batch` Tauri command +
   LocalIndex coalesced writes. **Shipped in this commit
   sequence.** Adds `IngestPipeline::ingest_documents_batch(Vec<RawDocument>)`
   that runs the existing per-doc embed loop but coalesces:
   one Arrow record-batch per ~`batch_size * 4` chunks
   (vs. one per doc), one Tantivy commit for the whole batch
   (vs. one per doc — Tantivy commits are dominant cost
   because of segment merges). Tauri command +
   `lib.rs` registration in place; `crisp-index-protocol` gains
   a parallel `IngestBatch { chunks: Vec<IngestChunk> }`
   wire-format struct (with round-trip + empty-`{}`-deserialise
   tests, 5/5 protocol tests pass) so the future server-side
   `POST /v1/ingest/batch` (step 4) shares the exact shape.
   Frontend wiring is now also shipped: `BatchReview.svelte`,
   `IndexIngest.svelte`, and the L3-promotion path all flush
   `index_ingest_batch` in N=16 buckets.

2. ✅ **Refactor (b):** `embedderLocation` config + the load gate.
   Default `'client'`; remote + `'server'` skips local embedder
   load and posts raw text chunks. This becomes semantically live
   once step 5 lands on the server.

3. ✅ **Refactor (c):** local writer queue. `IngestPipeline`
   serialises LanceDB/Tantivy mutations through a single writer
   task and exposes queue depth via `index_queue_depth`. Important
   limit: this is an in-memory process-local queue, not a durable
   resumable job system.

4. ✅ **Step 4a operator visibility:** foreground remote batch ingest
   no longer waits silently. The client now enqueues with
   `POST /v1/ingest/batch`, polls `/v1/tasks/:id`, emits live
   `queued` / `processing` / `done` progress messages, and mirrors
   the server-reported queue depth into the same bottom-left queue
   chip the local writer path already uses.

#### Durable queue design for step 4b

For 500k-file / multi-day remote backfills, the server ultimately
needs a **SQLite-backed durable work queue** distinct from the
LanceDB result store.
LanceDB tracks what has been successfully indexed; it is the wrong
place to track transient job state (`pending` / `in_progress` /
`retrying` / `failed`) because every status flip would create new
dataset versions without buying us queue semantics.

Current shipped state vs target:

| layer | shipped now | target |
|---|---|---|
| **Remote task persistence** | SQLite (`crisp_jobs.db`) task table with JSON payload column | SQLite (`crisp_jobs.db`) with richer `ingest_jobs` + `file_queue` tables |
| **Worker model** | single background worker with lease / heartbeat / retry semantics | one or more lease-aware workers |
| **Resume after restart** | yes, via leases / heartbeats / reclaim | yes, via leases / heartbeats / reclaim |
| **Task granularity** | batch task (`IngestBatch`) | file-level queue rows + richer job metadata |

The long-term split is:

| store | purpose |
|---|---|
| **SQLite** (`crisp_jobs.db`) | durable ingest/sort job state: what is queued, leased, failed, retried, paused, resumed |
| **LanceDB + Tantivy** | successful indexed result rows only |

Suggested schema:

```sql
CREATE TABLE ingest_jobs (
    id            TEXT PRIMARY KEY,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    status        TEXT NOT NULL,   -- pending|running|paused|done|error|cancelled
    source_paths  TEXT NOT NULL,   -- JSON array of roots
    config_json   TEXT,            -- embedder, batch size, target level, etc.
    total_files   INTEGER NOT NULL DEFAULT 0,
    done_files    INTEGER NOT NULL DEFAULT 0,
    error_files   INTEGER NOT NULL DEFAULT 0,
    skipped_files INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE file_queue (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id           TEXT NOT NULL REFERENCES ingest_jobs(id),
    file_path        TEXT NOT NULL,
    file_hash        TEXT,
    doc_id           TEXT,
    target_level     INTEGER NOT NULL,   -- 1, 2, 3
    status           TEXT NOT NULL DEFAULT 'pending',
    retry_count      INTEGER NOT NULL DEFAULT 0,
    error            TEXT,
    worker_id        TEXT,
    lease_expires_at INTEGER,
    last_attempted   INTEGER,
    completed_at     INTEGER,
    UNIQUE(job_id, file_path)
);

CREATE INDEX file_queue_claim_idx
    ON file_queue(job_id, status, lease_expires_at, id);
```

Key design points:

* **Use leases, not just `in_progress`.** A row needs
  `worker_id + lease_expires_at` (optionally heartbeat) so a
  crash or disconnect turns into "lease expired, reclaimable"
  rather than requiring bespoke cleanup.
* **Claim work atomically.** Do **not** `SELECT ... LIMIT N`
  then `UPDATE ... WHERE id IN (...)` in two separate logical
  steps. Use `BEGIN IMMEDIATE` plus one `UPDATE ... RETURNING`
  statement that both claims and returns the rows.
* **Avoid `BEGIN EXCLUSIVE`.** WAL mode + `BEGIN IMMEDIATE`
  is the right lock level for a short claim transaction.
  `EXCLUSIVE` is heavier than needed.
* **Treat counters as cached summaries.** `done_files` /
  `error_files` are useful, but `file_queue` remains the source
  of truth. Either update counters transactionally with row-state
  changes or periodically recompute.
* **Use explicit completion criteria.** For L3, a `doc_id`
  presence check in LanceDB can be the idempotency tie-breaker on
  resume; for L1 / L2 / sort jobs we should define per-job-type
  completion rules instead of assuming `chunk_index = 0` means
  "done" universally.

Recommended claim shape:

```sql
BEGIN IMMEDIATE;
WITH picked AS (
  SELECT id
  FROM file_queue
  WHERE job_id = ? AND status = 'pending'
  ORDER BY id
  LIMIT 16
)
UPDATE file_queue
SET status = 'in_progress',
    worker_id = ?,
    last_attempted = ?,
    lease_expires_at = ?
WHERE id IN (SELECT id FROM picked)
RETURNING id, file_path, target_level, file_hash, doc_id;
COMMIT;
```

On restart / reconnect:

* Rows with expired leases become eligible for reclaim.
* The worker checks LanceDB/Tantivy for the relevant result row
  before re-processing, so a task that wrote successfully just
  before a crash can still be marked done.
* Retries increment `retry_count`; after `max_retries` the row
  moves to `error` with the last message preserved.

4. ✅ **Server step 1 — bulk ingest API + SQLite lease queue bridge.**
   `crisp-index-server` exposes `POST /v1/ingest/batch`
   returning `202 Accepted + { task_id, queue_depth }`, persists
   queued tasks in `crisp_jobs.db`, and drains them through a
   single writer with lease / heartbeat / retry semantics while
   preserving the existing `queued | processing | done | failed`
   client contract. The Tauri remote batch path now switches from
   "error in remote mode" to enqueue + poll. This bridge now also
   supports env-configurable lease / retry tuning and optional
   task-status metadata for `attempt_count`, `max_attempts`,
   `last_heartbeat_at`, and `lease_expires_at`. Follow-up work is
   the richer file-level queue design described above.

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
  3 (refac) | local writer queue                  | serialized local writes + queue-depth chip  | durable queue design
  8         | RuntimeMode + HybridBackend         | Settings exposes 3 modes                    | step 9
  9         | sync_outbox + reconnect detector    | "online/offline/syncing" pill                | step 4
  4         | server bulk ingest API              | remote-mode 100x faster + resumable batches | step 4b
  5         | server-side embedding               | TB-scale workflows (laptop + GPU box)       | -
  6         | IVF-PQ with sample_rate             | 100M+-vector search latency stays ms-class  | -
  10        | CloudDrive (SMB/SFTP)               | Hetzner StorageBox + NAS as first-class     | (Internxt/Filen later)
  7         | sharding by volume_id               | only when needed                            | -
```

### P12 — `cloud-backup` integration (storage layer)

The user already runs a 3-tier backup pipeline (`../cloud-backup`)
in Python: `controller.py` archives + uploads to VPS, `vps_worker.py`
mirrors to Internxt, `retrieve.py` does smart cross-tier retrieval,
SQLite manifest DB tracks every file's location across `local /
VPS-incoming / VPS-processing / cloud-blob / cloud-extracted`. ~12 k
lines, mature, encrypted-config + receipt-based verification.

CrispSorter and cloud-backup are **complementary, not competing**:
content/search layer vs. storage/replication layer. The right
integration is well-defined boundaries, not a merge. Same runtime-
mismatch story as CrispLens — patterns + HTTP/IPC contracts, not code
import.

**Important refinement (2026-05-06).** cloud-backup isn't only a
*passive storage layer that CrispSorter reads from*; once
`crisp-index-server` exists (P11 step 4), cloud-backup's VPS
worker can also be a **producer of indexing payloads**. The VPS
already decrypts each archive in transit on its way to Internxt
— that's the cheapest place in the whole system to extract text
or generate thumbnails+embeddings, because the file is already
in RAM/temp, no extra download. Output payloads are an order of
magnitude smaller than inputs (a 50 MB PDF → a few KB of text +
a 4 KB embedding), so shipping them to crisp-index-server is
nearly free. **A backup pass becomes an indexing pass for new
content** — the user doesn't have to manually run extraction on
every file from their desktop. See "Producer side: VPS-trigger
indexing" below.

#### Boundaries

| concern | owner | how the other side reads it |
|---|---|---|
| **Where a file is** (local path / VPS path / cloud blob ID / extracted cloud path) | cloud-backup `manifest_sync` DB | CrispSorter reads via a new URI scheme (below) |
| **What a file says** (extracted text, embeddings, metadata) | CrispSorter LanceDB+Tantivy | cloud-backup never reads it |
| **Backup orchestration** (when, what, encryption, receipts) | cloud-backup | CrispSorter just triggers / schedules |
| **Search & sort** | CrispSorter | cloud-backup never touches it |
| **Smart retrieval** (cheapest tier first) | cloud-backup `retrieve.py` | CrispSorter calls it as a subprocess for L3 promotion of files only in archives |

#### URI scheme additions

CrispSorter already has `crisp+local://`, `crisp+vps://`,
`crisp+internxt://`, `crisp+internxt-zip://`. cloud-backup adds two
more:

* **`crisp+cb-archive://{archive-id}/{internal-path}`** — file lives
  inside cloud-backup archive `archive-id` (encrypted 7z on the VPS,
  also replicated to Internxt as a blob). Resolution: subprocess
  `retrieve.py --archive {id} --extract {internal-path}` which picks
  the cheapest tier (local cache → VPS remote extract → cloud).
* **`crisp+cb-extracted://{cloud-path}`** — file was extracted on
  the VPS and uploaded to Internxt's `/root/` tree (cloud-backup's
  "standard mode"). Resolution: ordinary `crisp+internxt://` read.

#### Read paths CrispSorter wants

1. **L1 ingest of an entire backed-up tree, zero extraction.**
   `index_ingest_cb_manifest(manifest_db_path, owner_id)` reads
   cloud-backup's SQLite (`SELECT path, size, mtime, hash FROM
   file_manifest`) and writes one L1 LanceDB row per file. Catalog
   sees "you have 482k files indexed at the filesystem-metadata
   level" without any 7z work — the entire backup tree becomes
   browsable in Übersicht in seconds.

2. **L2/L3 promotion via `retrieve.py`.** When the user clicks
   "Promote to L3" on a row whose `location_uri` starts with
   `crisp+cb-archive://`, CrispSorter spawns `retrieve.py` to
   stream just that one file from the cheapest tier, runs the
   existing extractor pipeline against it, and writes the L3
   chunks/embedding rows. Bridge pattern matches the
   CrispLens-style CLI bridges in P11 Pillar 5.

3. **Reverse lookup**: clicking a hit in Suche surfaces "This file
   is at: local cache / VPS / Internxt blob / Internxt extracted"
   — read directly from cloud-backup's manifest. No new code on
   cloud-backup's side.

#### Write paths cloud-backup gives CrispSorter

4. **Backup CrispSorter's catalog itself.** cloud-backup gets
   pointed at `~/Library/Application Support/CrispSorter/` (or the
   data-dir override) so the LanceDB+Tantivy databases + .caf files
   are part of the same encrypted backup pipeline as the user's
   documents. No changes needed in cloud-backup — just a config
   line.

5. **Search-driven backup priority.** CrispSorter exports a
   "frequently-accessed-files" list (the docs the user actually
   opens via the Übersicht/Stapel flow). cloud-backup keeps those
   in the local cache tier longer; archives the rest. Optional
   future hook; doesn't need to land in v1.

#### Producer side: VPS-trigger indexing (cloud-backup → crisp-index-server)

Once both `vps_worker.py` and `crisp-index-server` are deployed
on the same VPS, an indexing hook slots cleanly into the
existing post-receipt phase of cloud-backup. Pseudocode for the
worker addition:

```python
# vps_worker.py — new step between "extract" and "upload to cloud"
def maybe_index_for_search(file_path: str, file_hash: str, manifest_row: dict):
    # 1. Ask crisp-index-server: do you already have this doc_id?
    resp = requests.head(f"{INDEX_SERVER}/v1/docs/{file_hash}",
                         headers={"Authorization": f"Bearer {API_KEY}"})
    if resp.status_code == 200:
        return  # already indexed; skip

    # 2. Build the right IndexPayload variant per file kind.
    ext = Path(file_path).suffix.lower()
    if ext in TEXT_EXTRACTORS:        # .pdf, .docx, .epub, .html, .doc
        text, meta = extract_text(file_path)        # cloud-backup's existing extractors
        payload = {
            "kind": "text",
            "doc_id": file_hash,
            "location_uri": cb_archive_uri(manifest_row),  # crisp+cb-archive://...
            "full_text": text,
            "embedding": None,                              # let server embed (GPU)
            **meta,
        }
    elif ext in IMAGE_EXTS:           # .jpg, .png, .heic, ...
        thumbnail, clip_emb, faces = run_image_pipeline(file_path)  # P13 hook
        payload = {
            "kind": "image",
            "doc_id": file_hash,
            "location_uri": cb_archive_uri(manifest_row),
            "thumbnail_jpeg": thumbnail,
            "clip_embedding": clip_emb,                     # 512-dim float32
            "faces": faces,
            **meta,
        }
    else:
        # Unknown / unsupported — emit a Manifest-only payload so
        # the row at least exists at L1 in the catalog.
        payload = {
            "kind": "manifest",
            "doc_id": file_hash,
            "location_uri": cb_archive_uri(manifest_row),
            **fs_meta_from(manifest_row),
        }

    # 3. Ship as one batch entry. Real implementation accumulates
    #    payloads across N files and POSTs in batches of, say, 64.
    requests.post(f"{INDEX_SERVER}/v1/ingest/batch",
                  json={"payloads": [payload]},
                  headers={"Authorization": f"Bearer {API_KEY}"})
```

Operational consequences:

* **Index latency = backup latency.** The user runs cloud-backup
  on Sunday night; by Monday morning everything new is searchable
  in CrispSorter desktop without any client-side work.
* **GPU stays on the VPS.** Server-side embedding (P11 Pillar 2)
  is the right default for this path — cloud-backup's VPS already
  has the file decrypted and a GPU available; the desktop just
  receives metadata + queries.
* **Idempotent.** The `HEAD /v1/docs/:hash` check makes re-runs
  free. Manual re-index is `POST /v1/docs/:hash/reindex`.
* **No double-extraction.** When the user *also* drags a file
  into Stapel that's already in cloud-backup, CrispSorter
  desktop's pipeline checks the same `HEAD /v1/docs/:hash`
  endpoint before extracting locally. Hit → just attach the
  doc_id to the Stapel row, skip extraction.

This single worker addition (≈ 100 lines of Python on
cloud-backup's side, plus the unified payload shape from P11
Pillar 7 on the server side) turns the entire backup pipeline
into a continuous-indexing pipeline. No separate "indexer
daemon" to run.

#### Back-path: cross-tier resolution

The forward path (above) covers "extract once, ship to the
index." The back-path is the user opening / previewing / L3-
promoting a file that *isn't on the device they're searching
from* — they're on a laptop, the file lives on an external SSD
that's not attached, on the VPS, in an Internxt blob, or in
multiple tiers at once. Every Suche / Übersicht action needs
to know **where the file actually is right now** and **which
tier is fastest to fetch it from**.

cloud-backup already solves this. `retrieve.py` exposes
`ContentRetriever.locate_archive(archive_name, priority)` which
walks a configurable cascade:

```python
# default cascade
search_order = ['local', 'vps_incoming', 'vps_backup', 'cloud']
# with priority='VPS' (when the user is on the VPS itself)
search_order = ['vps_backup', 'vps_incoming', 'local', 'cloud']
```

Returns `(location_type, path)` without downloading anything.
And `extract_from_vps(archive, path, out)` does the right
thing for cloud-blob files: SSH to VPS, run 7z partial-extract
there, download **only** the matching file (vs. pulling the
whole archive). For a 2 GB encrypted archive containing a
20 KB PDF the user wants to read, this is the difference
between 5 seconds and 5 minutes.

CrispSorter doesn't reimplement any of this. Two things need
to happen on our side:

1. **Wrap `retrieve.py` as a subprocess sidecar.** Long-lived
   Python process started when CrispSorter detects a
   cloud-backup install at the user's data dir. Stdin/stdout
   JSON-RPC: `{"op": "locate", "doc_id": "..."}` →
   `{"tier": "vps_backup", "archive": "...", "internal_path":
   "..."}`. `{"op": "fetch", "doc_id": "...", "out": "..."}`
   → streams progress events, returns final path. Same bridge
   pattern as the Internxt / Filen CLIs from P11 Pillar 5;
   different language, identical shape.
2. **Track availability per row, surface in the UI.** New
   JSON field on `metadata_json`:

   ```json
   {
     "availability": {
       "local":         { "present": true,  "path": "/Users/.../file.pdf" },
       "vps_incoming":  { "present": false },
       "vps_backup":    { "present": true,  "archive": "abc.7z" },
       "cloud_blob":    { "present": true,  "blob_id": "..." },
       "cloud_extracted": null,
       "checked_at": 1762432000
     },
     "preferred_tier": "local"
   }
   ```

   Populated lazily — first time the user runs a search, the
   top hits get a `locate` call dispatched in parallel; the
   UI badges them as soon as each result returns. Cached in
   `metadata_json` for ~10 minutes (configurable; cloud
   storage moves rarely so a long TTL is fine).

#### UI: source-aware actions

In Suche + Übersicht, every row shows a **tier badge** in a
new column based on `availability.preferred_tier`:

| badge | meaning | open / preview behaviour |
|---|---|---|
| 🟢 **local** | file on this device | instant — `tauri-plugin-fs::open` |
| 🟡 **VPS-cache** | on VPS in `incoming` or `backup` dir | spawn `retrieve.py extract_from_vps` → progress bar (~seconds for small files) |
| 🔵 **cloud-extracted** | in Internxt as a raw file (cloud-backup standard mode) | direct cloud-drive read via `CloudDrive::read_file` |
| 🟣 **cloud-blob** | in Internxt as an encrypted 7z blob | spawn `retrieve.py` → VPS gateway → 7z partial-extract → progress bar (~minutes for first cold fetch) |
| ⚪ **unknown** | not yet checked | trigger `locate` on hover |
| ⚫ **unavailable** | the cascade returned nothing | grey out the row; surface a tooltip explaining where it should be |

Click → "Open" routes through whichever tier is preferred;
right-click → "Open from..." lets the user pick a non-default
tier (useful for verification, or to force-cache locally).

For L3 promotion + preview-pane PDF rendering, the same path
is used: ask for the bytes, get a progress event stream, and
when bytes arrive, run them through the existing extractor +
Übersicht preview code. **No code path in CrispSorter should
ever assume the file is locally readable** — it always goes
through the resolver, which fast-paths the local case.

#### Pre-fetch + cache hygiene

* When the user types in the Suche box, CrispSorter
  dispatches `locate` calls **in parallel** for the first
  page of hits, so by the time results render the badges are
  accurate. 200 ms budget; results without a determined tier
  yet show ⚪ until the locate returns.
* Successful fetches populate a local cache directory
  (`~/.cache/crispsorter/fetched/`) keyed by doc_id. Cache is
  size-bounded (default 10 GB), evicts oldest. cloud-backup's
  own `cache_archives` directory is read-only from
  CrispSorter's perspective — we don't write to retrieve.py's
  cache, we mirror its successful retrievals into ours so we
  can serve them without re-spawning the sidecar.
* `availability.checked_at` invalidates after 10 minutes; a
  search after that re-locates (cloud-backup's manifest may
  have been updated by another node).

#### Mode interactions

cloud-backup integrates cleanly with the runtime modes from P11
Pillar 4:

| RuntimeMode | cloud-backup's role |
|---|---|
| **Standalone** (laptop only) | invisible — CrispSorter reads the local copies; cloud-backup runs in the background as before |
| **Server** (CrispSorter on VPS) | the *same* VPS is `vps_worker.py`'s host. CrispSorter's `crispembed-cuda` GPU index sits next to cloud-backup's processing dir; both reference the same files via VPS-local paths instead of `crisp+vps://` URIs |
| **Hybrid** (laptop + VPS, the load-bearing case) | CrispSorter's `crisp+cb-archive://` resolution + the SyncManager from P11 Pillar 6 share the same reconnect detector. Offline edits queue in `sync_outbox`; cloud-backup ops queue in cloud-backup's existing checkpoint system; both flush when the VPS becomes reachable |

#### Migration order (slots into P11's table)

| step | what | status |
|---|---|---|
| 11   | **`retrieve.py` sidecar bridge.** Persistent Python subprocess started when CrispSorter detects cloud-backup at the user's data dir; stdin/stdout JSON-RPC (`locate`, `fetch`). Wraps `ContentRetriever.locate_archive` + `extract_from_vps` + `extract_from_local`. New URI scheme `crisp+cb-archive://{archive-id}/{internal-path}` resolves through it. | medium (~2 days) -- the load-bearing back-path. cloud-backup unchanged. |
| 12   | **Availability tracking + tier badges.** Lazy `locate` calls populate `availability` JSON in `metadata_json`; Suche / Übersicht render a tier-badge column (🟢/🟡/🔵/🟣/⚪/⚫); right-click "Open from..." picker; click routes through the resolver, never assuming local FS. Cache mirror at `~/.cache/crispsorter/fetched/`. | medium (~3 days) -- mostly UI + the small `locate`-on-search-hit dispatcher. |
| 13   | `index_ingest_cb_manifest` Tauri command + Übersicht "Import cloud-backup manifest" button (`IndexPayload::Manifest` variant -- batched insert, no extraction) | small (~1 day) -- reads cloud-backup SQLite, writes L1 LanceDB rows |
| 14   | Settings -> "Backup CrispSorter catalog with cloud-backup" toggle (just adds the data-dir to cloud-backup's source list via its config) | trivial |
| 15   | **VPS-trigger indexing (cloud-backup as producer).** Hook in `vps_worker.py` that runs after archive extract / before cloud upload: `HEAD /v1/docs/{hash}` skip-or-extract, `POST /v1/ingest/batch` per N=64 files. Depends on P11 step 4 (server bulk-ingest API) + P11 Pillar 7 (unified payload format) being live. | medium (~3 days) -- ~100 lines of Python on cloud-backup side + crisp-index-server endpoint |
| 16   | "frequently-accessed" export hook (CrispSorter -> cloud-backup) | optional, not v1 |

Three load-bearing groups:

* **Consumer side (back-path):** steps 11 + 12. Once these
  ship, CrispSorter Suche / Übersicht know where every file
  actually is, surface tier badges, and resolve open / preview
  / L3-promote requests through cloud-backup's existing
  cascade — the user can search a multi-TB archive from a
  laptop with nothing local and the experience degrades
  gracefully (instant for hot files, seconds for VPS-cached,
  minutes for cloud-only).
* **Manifest L1 import:** step 13. Reads cloud-backup's
  `file_manifest` and writes L1 rows in seconds. The catalog
  pane shows every backed-up file even though nothing has
  been extracted.
* **Producer side (forward-path):** step 15. cloud-backup's
  VPS worker becomes a producer of `IndexPayload`s. Backup
  latency = index latency.

After all five ship (11-15):

* A user backs up a fresh batch of files Sunday night.
* Monday morning their CrispSorter desktop sees them already
  indexed at L3 with embeddings — no client-side extraction
  needed, the GPU-equipped VPS did it during the backup pass.
* Suche from any device — laptop, web PWA, second desktop —
  shows the new files with the right tier badges and lets the
  user open them through the cheapest available path.
* Documents already-indexed are skipped via the `HEAD /v1/docs`
  pre-check, so re-runs are free.

#### What we don't do

* **Don't rewrite cloud-backup in Rust.** It's mature, encrypted,
  in production. Subprocess bridge is enough and matches the
  CrispLens-CLI bridge pattern from P11.
* **Don't duplicate the manifest DB.** cloud-backup's
  `file_manifest` stays the source of truth for "where is this
  file"; CrispSorter's LanceDB row carries `crisp+cb-archive://...`
  and resolves at read time. Drift is impossible because resolution
  always re-asks cloud-backup.
* **Don't encrypt-encrypt.** When CrispSorter ingests via
  `retrieve.py`, the file is decrypted by cloud-backup → handed to
  CrispSorter as plaintext bytes → CrispSorter's LanceDB stores
  whatever its own encryption settings dictate (today: at-rest
  via the OS filesystem encryption; future: per-row).

#### Convergence checkpoint (the picture, end-to-end)

When P11 (server + bulk ingest + unified payload), P12 (cloud-
backup as producer), and P13 (CrispLens migrating onto the same
server) all land, the system shape is:

```
                 ┌──────────────────────────────────────────────┐
                 │     crisp-index-server  (Axum + LanceDB +    │
                 │     Tantivy + SQLite outbox + GPU embedder)  │
                 │                                              │
                 │     POST /v1/ingest/batch  (IndexPayload)    │
                 │     HEAD /v1/docs/{hash}                     │
                 │     POST /v1/search                          │
                 │     GET  /v1/sync/since?ts=…                 │
                 └─────────▲─────────▲──────────▲───────────────┘
                           │         │          │
                           │         │          │
       ┌───────────────────┘         │          └────────────────────────┐
       │                             │                                    │
┌──────────────┐         ┌────────────────────────┐         ┌──────────────────────┐
│ CrispSorter  │         │  cloud-backup VPS      │         │ CrispLens v4 / suite │
│ desktop      │         │  worker (vps_worker.py)│         │ (image vertical)     │
│ (Tauri)      │         │                        │         │                      │
│ * Stapel +   │         │ * decrypt 7z in transit│         │ * face engine        │
│   Hinzufügen │         │ * extract text /       │         │   (SCRFD+ArcFace)    │
│ * Sync local │         │   thumbnail+CLIP       │         │ * CLIP image emb     │
│   ↔ remote   │         │ * HEAD /v1/docs skip   │         │ * gallery / lightbox │
│ * IndexBackend│        │ * POST /v1/ingest      │         │                      │
│   trait swap │         │   (Text or Image       │         │ * shares server +    │
│              │         │   payload)             │         │   sync + auth        │
└──────┬───────┘         └────────────┬───────────┘         └──────────┬───────────┘
       │                              │                                │
       │ POST /v1/ingest/batch        │ POST /v1/ingest/batch          │ POST /v1/ingest/batch
       │   IndexPayload::Text         │   IndexPayload::Text           │   IndexPayload::Image
       │   IndexPayload::Manifest     │   IndexPayload::Image          │
       │                              │   IndexPayload::Manifest       │
       └──────────────────────────────┴────────────────────────────────┘
                              │
                       (one queue, one writer,
                        one search index, one
                        sync history)
```

Three producers, one server, one canonical index. Each producer
decides what kind of payload it can produce given the work
already in front of it (cloud-backup has decrypted bytes →
extract immediately; CrispSorter desktop has user-attention →
extract on demand; CrispLens has the user's photo workflow →
emit image payloads). The server doesn't care which client sent
what — same dedup, same retry queue, same search responses.

**This is the optimisation the user asked about**: the
indexing-trigger doesn't have to live on the desktop. Whichever
node already has the file in hand, in the right state, does the
work — and only that node.

### P13 — Image-vertical convergence with CrispLens

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
