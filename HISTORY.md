# CrispSorter — History & Archived Plans

This file collects historical planning documents that are no longer
"living" but are still useful as context — explanations of *why* parts
of the codebase look the way they do.

For active development plans, see [PLAN.md](PLAN.md).
For technical pitfalls / non-obvious patterns, see [LEARNINGS.md](LEARNINGS.md).

---

## Session log — 2026-05-06 — `crisp-index-server` integrated as workspace member

The Axum VPS backend that PLAN.md P11 names as the server side of the
remote-architecture story used to live in a sibling directory
(`../crisp-index-server`) without a git repo. P11 still described it as
"a documented skeleton with stub handlers", but the local code had
already grown a full LanceDB + Tantivy + RRF implementation. Two
parallel definitions of the wire format (`IngestChunk` / server,
`IngestPayload<'a>` / client) were drifting silently.

This session vendored the server into the CrispSorter repo as a Cargo
workspace member, with a third member crate (`crisp-index-protocol`)
holding the wire types both sides depend on.

### Layout change

```
CrispSorter/
├── Cargo.toml             ← new workspace root (resolver = "2")
├── Cargo.lock             ← unified workspace lockfile
├── crisp-index-protocol/  ← wire types + serde tests (new)
├── crisp-index-server/    ← copied from ../crisp-index-server (no prior git)
└── src-tauri/             ← existing Tauri 2 desktop app, now a member
```

The previous `src-tauri/Cargo.lock` was deleted; the workspace root
owns the lockfile.

### Why a workspace, not a separate GitHub repo

- P11 steps 1, 2, 4, 5 are intentionally paired client + server changes
  (`embedderLocation` flag, `IngestBatch`, `/v1/ingest/batch` 202 +
  task_id). One repo lets one PR touch both sides.
- The protocol crate ends the parallel-types problem: change the
  `IngestChunk` shape and both crates rebuild; change one and serde
  tests in `crisp-index-protocol` catch it.
- The server can still be released and deployed independently — its
  `crisp-index-server/README.md` Docker / systemd / nginx recipes work
  unchanged.
- No prior git history existed for `../crisp-index-server`, so a clean
  import was free.

### Protocol crate (`crisp-index-protocol`)

Single source of truth for: `IngestChunk`, `IngestResponse`,
`SearchRequest`, `SearchFilters`, `SearchHit`, `UpdateLocationBody`,
`UpdateLocationByUriBody`, `UpdateLocationResponse`, `DeleteResponse`,
`StatsResponse`, `HealthResponse`, `ErrorResponse`. Plus
`SearchFilters::to_lance_sql()` (pure-string), used by the server's
LanceDB layer.

`SearchHit` is the strict wire subset. The client-side `SearchResult`
in `src-tauri/src/index/schema.rs` is a superset — its extra optional
fields (`metadata_json`, `catalog_source`, `volume_id`) are populated
locally for catalog-channel hits, ignored when reading server
responses (default `None`). Keeping these split means the server
doesn't depend on LanceDB schema details that are only meaningful
client-side.

Bonus correctness fix: `SearchHit` now includes `ext` (was missing
from the server's old local `SearchResult`); the server reads it from
the LanceDB `ext` column in `batches_to_results`.

`tags` standardised to `Vec<String>` with `#[serde(default)]` on both
sides — was `Option<Vec<String>>` on the server and `&[String]` on the
client; round-trip was already compatible but the new shape removes
one Option unwrap.

### Build system tweaks

- `crisp-index-server/build.rs` now mirrors `src-tauri/build.rs`'s
  protoc fallback via `protoc-bin-vendored`. The transitive
  lance-encoding requirement on `protoc` is now covered by both
  workspace members independently.
- Root `.gitignore` adds `/target` (workspace target dir) and
  `crisp-index-server/{data,target}` so a real deployment's hundreds
  of GB of LanceDB shards never get accidentally `git add`-ed.

### Verification

- `cargo build -p crisp-index-protocol` — green.
- `cargo test  -p crisp-index-protocol` — 4/4 passing
  (round-trip, omit-None, tolerant-deserialize, lance SQL).
- `cargo build -p crisp-index-server` — green (with two pre-existing
  unrelated `unused_mut` / `dead_code` warnings).
- `cargo build -p tauri-app` — desktop app still compiles unchanged
  modulo the protocol-crate dep (verified after the workspace
  conversion; the move from `src-tauri/target` to `/target` triggers
  a full re-link but no source changes).

### What this unblocks

P11 steps 4-7 (server bulk ingest API, server-side embedding, IVF-PQ
with sample_rate, sharding) are now in-tree work. The next concrete
step is P11 refactor (a) — `index_ingest_batch` Tauri command + the
parallel `crisp_index_protocol::IngestBatch { chunks: Vec<IngestChunk> }`
wire type. Both can land in one commit since both halves live here.

### Disk hygiene fallout from the workspace move (commits `4654c18`, `10ecaab`)

The workspace promotion above silently changed where `cargo build`
puts artefacts: from `src-tauri/target/` to `/target/` at the repo
root. None of the developer scripts noticed:

* `enable-crispembed.{ps1,sh}` kept staging DLLs into
  `src-tauri/target/{debug,release}/` while cargo was writing the
  .exe to the new location. STATUS_DLL_NOT_FOUND on every cuda /
  vulkan build, except where a previous run had left the same DLLs
  in `src-tauri\bin\` (Tauri's bundled-resources dir, which
  *is* searched).
* `recompile-exe.ps1` kept reporting "Build successful! Executable
  located at: src-tauri\target\release\CrispSorter.exe" when the
  .exe was actually somewhere else.
* `release.sh`, `scripts/build.sh`, `scripts/bundle_macos_native_libs.sh`
  all looked in the legacy paths. The macOS notarisation /
  bundling pipeline would have failed on first run on any clean
  machine.
* `scripts/build.sh`'s `CRISPSORTER_TARGET_VOLUME` symlink-to-
  external-SSD trick was silently broken — it created a symlink
  at `$SRC_TAURI/target/` while cargo wrote to
  `$REPO_ROOT/target/`. Users who'd set it up to keep build
  artefacts off the boot drive were quietly seeing them back
  on the boot drive.

The workspace orphaned `src-tauri/target/` accumulated 26 GB of
pre-workspace artefacts on the user's notebook (debug 20 GB +
release 5.7 GB) — boot drive at 99% full, 6.4 GB free.

Fix in two commits:

* **`4654c18` — script paths.** All six callers now write to /
  read from `target/` at the workspace root first; legacy
  `src-tauri/target/` paths kept as graceful fallbacks for
  branches that haven't picked up the workspace move.
* **`10ecaab` — `CARGO_TARGET_DIR` honoured.** The DLL-staging
  code in `enable-crispembed.ps1` and the .exe-locator in
  `recompile-exe.ps1` both read `$env:CARGO_TARGET_DIR` if set,
  falling back to `$ProjectRoot\target`. User-facing
  "Staged N DLL(s) to ..." message reads from the same
  variable so the printed path is honest.

After cleanup:

* `rm -rf src-tauri/target` recovered 26 GB instantly (build
  cache only; .gitignored; regenerated on next build).
* Repo size 31 GB → 5.2 GB on disk; free 6.4 GB → 32 GB.
* User's standard incantation for "build with target on D:\":

  ```powershell
  $env:CARGO_TARGET_DIR = "D:\cargo-target\crispsorter"
  .\enable-crispembed.ps1 -Backend cuda
  ```

  Documented in PLAN.md → P4 → "Disk hygiene: redirect Cargo
  target dir to an external drive."

---

## Session log — May 2026 — index-test → main reconciliation + P9 step 1+2

### Branch reunification (commits `400df29`, `33479da`)

The `index-test` branch had drifted significantly from `main` — 18
commits ahead carrying the L1/L2/L3 multi-level ingest, hf-hub
Windows-symlink workaround, GGUF model registry expansion,
NC-license gating, embedder benchmark, CAF round-trip restore,
`enable-crispembed.{ps1,sh}` + DLL staging, and protoc bootstrap
in `paths.ps1`. Meanwhile `main` had shipped 65 commits worth of
independent work — full Catalog/Cathy subsystem with `.caf v6`
+ volume-header round-trip, parallel scanner (jwalk-backed),
dedup engine, deletion scripts; LanceDB materialisation with
unified search; live preview pane; pure-Rust OCR via `ocrs`;
background ingest scheduler with mtime-skip + foreground-search
throttling; saved searches; field-prefix FTS syntax;
cross-mount volume awareness (P7.6); single-binary CLI mode;
matryoshka dim selection; cross-encoder reranking; macOS/Linux
native-lib bundling for `libcrispasr` / `libcrispembed`.

17 conflicts resolved with the better-of-both rule. Notable choices:

* **`index/embedder.rs`** — main's expanded model set
  (EmbeddingGemma300M, GTE base/large) wins; HEAD's
  `approx_download_mb` / `gguf_download_mb` /
  `gguf_quant_suffix_str` / `gguf_file_name` helpers + the
  hf-hub Windows workaround `fastembed_native_files()` preserved.
  Octen variants now route through fastembed-rs's auto-download
  (3 of 4 variants), keeping the local-only Int8 fallback.
* **`index/mod.rs`** — `IndexConfig` combines HEAD's `use_vector`
  master switch with main's `reranker_model` / `rerank_top_n`
  / `model_cache_dir` / `matryoshka_dim`.
* **`index/search.rs`** — combined HEAD's `Option<Arc<Mutex<Embedder>>>`
  (so L1/L2 paths work with `use_vector=false`) with main's
  reranker support and `EmbedRole::Query` asymmetric retrieval.
* **`index/tauri_commands.rs`** — pre-compute `models_dir` +
  `effective_dim` up-front so they're available even when
  `load_embedder=false`. Then conditionally construct the
  `Option<Arc<Mutex<Embedder>>>`.
* **`index/local_index.rs`** — kept main's `update_location_by_uri`
  AND HEAD's `update_l2_fields`. `SearchResult` carries both
  HEAD's `metadata_json` and main's `catalog_source` + `volume_id`.
* **`src/lib/log.ts`** — main's `frontendLogs` store + `flog`,
  HEAD's `logInfo` / `logWarn` / `logError` as wrappers that
  push to the local store (no Rust round-trip → no LogPanel
  duplication).
* **Settings.svelte** — kept HEAD's organised-by-size embedder
  dropdown with engine-aware filtering and NC-license gating;
  updated all model values + i18n keys to main's naming. Added
  EmbeddingGemma + GTE base/large to the mid/large optgroups.
* **Catalog subsystem** — main wins entirely. Pulled HEAD's
  pruned `caf.rs` / `index.rs` restore in favour of main's
  full v6-writer + dedup + lance modules.

After the merge, three trivial compile errors fell out
(`SearchResult` missing `metadata_json` in two call sites,
`embed_dense` arity change to take `EmbedRole`); fixed in
commit `33479da`. `cargo check --no-default-features`: green.

Branch hygiene: `index-test` was fast-forwarded into `main` (no
force push, since main was a strict ancestor of index-test
post-merge) and then deleted both locally and on origin. `main`
is once again the canonical branch.

### PowerShell scripts unstuck on PS 5 / German locale (commit `1f1d2a9`)

`paths.ps1` started failing to parse on a German Windows shell
with `Unerwartetes Token "Active"` and `Die Zeichenfolge hat kein
Abschlusszeichen`. Cause: PowerShell 5 reads UTF-8-without-BOM
files using the system code page (CP1252 on a German install),
so the multi-byte em-dash bytes inside string literals get
re-interpreted as a quote-like character and the parser sees
an unterminated string. Cascading "missing closing brace" errors
follow.

Belt-and-braces fix:
1. Replaced every em-dash with `--` (ASCII) in `paths.ps1`,
   `enable-crispembed.ps1`, `recompile.ps1`, `recompile-exe.ps1`.
2. Re-wrote each script with a UTF-8 BOM (EF BB BF) so PS 5
   detects UTF-8 explicitly.

Bonus: `enable-crispembed.ps1`'s "Staged 0 runtime DLL(s)"
message was misleading on the no-op happy path (re-running the
script when target dirs already had the DLLs at the right size).
Branch: print "Staged N DLL(s)" in green only when something was
actually copied; otherwise print a calmer "DLLs already up to
date (N files, no copy needed)".

### Settings + DB persistence on app restart (commit `e41d704`)

Symptom: every app restart, the Search-Index model selection
silently reverted to BgeM3 (the Rust default). Cause: the Rust
`IndexState` always boots with `IndexConfig::default()`. The
JS-side `tauri-plugin-store` carried the user's persisted
choices, but `Settings.svelte`'s onMount loaded them only
into JS state — never pushed them to Rust until the user
opened Settings and clicked Apply.

Fix: added a boot block in `+page.svelte` `onMount` that loads
every `index_*` setting from the store, translates UI keys
(`bge_m3`, `auto`, etc.) to Rust kebab strings (`bge-m3`,
`auto`) via inline maps mirroring `Settings.svelte`'s
`*ToRust` helpers, invokes `index_set_config`, and -- if
`cfg.enabled` is true — auto-invokes `index_init` with
`withEmbedder=false`. The L1 LanceDB rows from a previous
session now appear in Übersicht on app start instead of
showing an empty pane.

### UI cleanup: Kataloge umbrella + Duplikate sub-tab (same commit)

Pre-merge nav had two buttons firing on `activeTab === 'catalog'`
(both HEAD and main wanted that slot) plus a standalone
"Duplicates" button. Dropped both duplicates; single Kataloge
nav entry routes into `IndexIngest`, which now hosts the
.caf-volumes Catalog and Duplikate as sub-tabs alongside
Übersicht / Suche / Hinzufügen / Quellen.

### i18n coverage extension (commits `e41d704`, `2e92ec8`)

* Tab labels (Übersicht / Suche / Hinzufügen / Quellen) moved
  out of inline German literals into `i18n.t.indexIngest.tab_*`.
* New keys for the `tab_caf_catalog` + `tab_duplicates` sub-tabs.
* Full Duplicates pane: title / subtitle / source / destinations
  / match-mode strategy options / find / running / errors / 4
  picker dialog titles / result table column headers / matches +
  selected counts / deletion-script builder (format / target /
  generate / save / space-freed hint) / empty state. EN+DE.
* `Settings.svelte`'s "CrispEmbed was built with the cuda
  backend" hint moved to `i18n.t.crispembed_engine_built` /
  `_cpu` with `{backend}` substitution and minimal
  `**bold**` / `` `code` `` markdown rendering.

Open follow-up: the `.caf` Catalog sub-tab (`Catalog.svelte`)
still has hard-coded English strings. The `caf_catalog.*` i18n
keys exist (EN+DE) but the component hasn't been wired through
them yet.

### P9 step 1 — `index_query_documents` Tauri command (commit `4ecfd7a`)

Paginated, filterable, sortable browse of the documents table,
designed to drop in cleanly today and graduate to keyset + DB-side
ORDER BY without breaking the API contract.

* `DocumentFilter` / `SortSpec` / `PageSpec` / `PageCursor` /
  `DocumentPage` types in `index/schema.rs` (with `#[serde(default)]`
  on every field of `DocumentFilter` so the frontend can omit
  fields it isn't constraining).
* `LocalIndex::query_documents` + helpers (`filter_to_sql`,
  `sort_rows`) in `index/local_index.rs`. `total_estimate` via
  `count_rows` against the same predicate. 50k-row hard cap on
  the in-process sort window because LanceDB 0.26's public Rust
  query API doesn't expose `ORDER BY`.
* Tauri command + `lib.rs` registration. Returns an empty page
  silently when the index isn't yet initialised — the Übersicht
  pane polls during boot before `index_init` finishes, and
  erroring there would surface as red log lines instead of an
  empty state.
* 7 pure-function unit tests pinning filter SQL generation, sort
  ordering, and PageCursor offset round-trip.

### P9 step 2 — columnar Übersicht + multi-select (commit `9cbe0c1`)

* CSS-grid table — single `grid-template-columns` shared
  between thead and every row, sticky header. Columns: select,
  ext, name, author, year, size, modified, folder, level,
  actions.
* Server-side filter + sort + pagination via
  `index_query_documents`; the chip bar (folder prefix, ext
  multi-select, L1/L3 toggle, name substring, sort header)
  serialises into a `DocumentFilter`. Completeness chip stays
  client-side until P9 step 3 promotes those flags to scalar
  columns.
* "Load more" button paginates (fetches next 200 rows, appends
  in-place, preserves selection, total estimate shown alongside).
* Multi-row selection with mouse: bare click = single-row
  select, Shift+click = range from last anchor, Ctrl/Cmd+click
  = toggle. `user-select:none` on rows so dragging selects rows
  instead of highlighting filename text. Mirrors the
  `BatchReview.svelte` handler so the two panes feel identical.
* Bottom-left stats now shows a collapsible "Stapel: 271 · DB:
  321k" summary; click to expand the per-extension breakdown.
  `dbDocCount` polls `index_stats` every 4 s.
* Settings sidebar alignment fix — wrapped icon + label in a
  flex `<span class="prov-label">` so the App-Einstellungen
  buttons hug their icons; status checkmark sits to the right
  via the parent's `space-between`.

Bugs hit on first walkthrough, all fixed:

* "Übersicht is empty when entering Kataloge from another tab"
  — `loadContents` was wired only to the tab-button onclick;
  fixed via a `$effect` that fires on first activation +
  every chip change.
* "Always says Lade…" + ERROR loop — combined cause: an earlier
  auto-load `$effect` re-ran on every `_allContents = []` reset
  (which the catch path did on every error), and
  `DocumentFilter` rejected payloads that omitted any field
  (`missing field 'ext'`). Fixed: dropped the redundant effect,
  added `#[serde(default)]` on `DocumentFilter`, made
  `index_query_documents` return an empty page when local
  isn't initialised yet, deduped error log to one line per
  distinct message.

---

## RAG / Search Extension — Original Plan (March 2026)

> Originally `rag_plan.md`. Phases P1–P13 are all shipped; this section
> remains as the design rationale for the search-index architecture
> (LanceDB + Tantivy + dtSearch query syntax + URI-based location
> tracking + multi-user-from-day-one). When the code references
> "§N rationale" it points at the corresponding section below.
>
> Original status: planning → implementation. Last edited 2026-03-16.

---

## 0. Goals

1. Add **local LanceDB** semantic + full-text search to CrispSorter
2. Support **remote LanceDB** via a self-hosted Rust/Axum VPS server
3. Track **where every file lives** with a typed, forward-compatible URI scheme
4. Handle **hundreds of thousands** of German + English academic documents
5. Provide **advanced** proximity/wildcard/boolean full-text search
6. Keep the setup **versatile from the UI** (mode, embedder, device, backend)
7. Design for **multi-user** from the start without forcing it on single-user installs

---

## 1. File Location URI Model

Every indexed document carries a `location_uri` — a single UTF-8 string, structured as a typed URI.

### Scheme

```
crisp+local://{user-uuid}@{machine-uuid}/{absolute-path}
crisp+vps://{user-uuid}@{host}:{port}/{path}
crisp+internxt://{user-uuid}/{cloud-path}
crisp+internxt-zip://{user-uuid}/{archive-cloud-path}#{internal-path}
```

### Design decisions

- `user-uuid` — UUID v4 assigned at first CrispSorter launch, stored in app config.
  **Not** a username: usernames change, collide across machines, and leak PII into the index.
- `machine-uuid` — UUID v4 generated once per installation.
  For single-user installs both UUIDs are auto-populated and invisible in the UI.
- A **user registry** (small JSON sidecar, not in LanceDB) maps `user-uuid → display-name`
  so the UI shows "stc @ Desktop" rather than raw UUIDs.
- The `internxt-zip` scheme uses `#fragment` for the in-archive path — same convention
  as URL fragments, making the URI round-trippable with standard URL parsers.
- `metadata_json` (see schema) is the escape hatch for future location types — no
  schema migration needed.

### Rust enum

```rust
pub enum FileLocation {
    Local   { user_id: Uuid, machine_id: Uuid, path: PathBuf },
    Vps     { user_id: Uuid, host: String, port: u16, path: String },
    Internxt { user_id: Uuid, cloud_path: String },
    InternxtZip { user_id: Uuid, archive_cloud_path: String, internal_path: String },
}

impl FileLocation {
    pub fn to_uri(&self) -> String { … }
    pub fn from_uri(s: &str) -> Result<Self> { … }
    pub fn retrieval_cost(&self) -> RetrievalCost { … }  // Free / Cheap / Expensive
}
```

`retrieval_cost` lets the UI warn: "This file must be downloaded from Internxt before opening."

---

## 2. Embedder Selection

### Primary: `BAAI/bge-m3` via `fastembed-rs` 5.x

| Property | Value |
|---|---|
| Context window | **8 192 tokens** (decisive for long academic texts) |
| Languages | 100+; German and English both top-tier |
| Output | Dense 1024d + multilingual sparse (SparseModel::BGEM3) |
| Crate | `fastembed` 5.13.x (`EmbeddingModel::BGEM3`, `SparseModel::BGEM3`) |
| Execution providers | CoreML (Metal/Neural Engine, macOS) · CUDA (Windows/Linux) · CPU |

#### Quantisation

| Format | Size | CPU speedup | Quality loss | Status |
|---|---|---|---|---|
| FP32 | ~1.1 GB | 1× | 0% | **available** (`EmbeddingModel::BGEM3`) |
| INT8 | ~280 MB | 2–3× | <1% on BEIR | **not yet in fastembed hub** — load via `try_new_from_user_defined` with custom ONNX |
| Q4 | N/A | — | — | not applicable to ONNX encoder models |

> **Q4 answer**: ONNX Runtime does not support 4-bit quantisation for transformer encoder
> models the way llama.cpp/GGUF does for decoder LLMs. INT8 is the practical limit.
> A custom INT8 bge-m3 ONNX can be produced with `optimum-cli` and loaded via
> `TextEmbedding::try_new_from_user_defined` — planned as a future UI option.

#### Embedder menu (UI dropdown) — `EmbedderModel` enum

| Variant | fastembed model | Dims | Context | Sparse | Best for |
|---|---|---|---|---|---|
| `BgeM3` ★ default | `BGEM3` | 1024 | 8192 | `BGEM3` (multilingual) | de+en, all sizes |
| `MultilingualE5Large` | `MultilingualE5Large` | 1024 | 512 | none | lighter, still multilingual |
| `MultilingualE5Base` | `MultilingualE5Base` | 768 | 512 | none | faster, medium quality |
| `MultilingualMiniLm` | `ParaphraseMLMiniLML12V2` | 384 | 512 | none | VPS CPU, very fast |
| `BgeSmallEn` | `BGESmallENV15` | 384 | 512 | `SPLADEPPV1` | English-only |

VPS server defaults to `MultilingualMiniLm` (CPU-friendly) unless overridden by config.

#### Sparse model pairing rationale

- `BgeM3` → `SparseModel::BGEM3`: same model, multilingual sparse weights. ✓
- `BgeSmallEn` → `SparseModel::SPLADEPPV1`: English-only collection, SPLADE is fine.
- `MultilingualE5*` / `MultilingualMiniLm` → **no sparse**: English SPLADE against German
  text produces poor recall. Better to do dense-only than degrade hybrid results.

---

## 3. Chunking Strategy

Do **not** embed whole documents. Embed overlapping chunks aligned to section boundaries.

```
Document → extract headings (from Markdown structure)
         → split at headings; subdivide long sections into 512-token windows
            with 128-token stride overlap
         → one LanceDB row per chunk
         → whole-document row (chunk_index = -1) for metadata queries
```

At query time: retrieve top-K chunks → deduplicate by `doc_id` → rank by max-chunk-score.
Heading-aligned chunks are semantically coherent; stride overlap prevents boundary misses.

---

## 4. LanceDB Schema

One table per "library" (user-configurable). Rows are **chunks** (one per embedding unit).

```
id                Utf8            SHA-256 of (doc_id + chunk_index)
doc_id            Utf8            SHA-256 of file content (stable across moves)
location_uri      Utf8            crisp+* URI
owner_id          Utf8            user UUID (denormalized for fast filter)
filename          Utf8
title             Utf8
author            Utf8
year              Int32
ext               Utf8
language          Utf8            "de" | "en" | "de+en" | …
page_count        Int32
headings_text     Utf8            all headings joined (for boosted FTS field)
full_text         Utf8            stripped plain text (FTS source + embedding source)
full_text_md      Utf8            Markdown with heading hierarchy (for display/preview)
embedding         FixedSizeList<Float32>[1024]     bge-m3 dense vector
embedding_sparse  Utf8            JSON: {"term": weight, …}  bge-m3 sparse weights
embedding_model   Utf8            model ID that produced this embedding
chunk_index       Int32           0-based; -1 = whole-document metadata row
chunk_total       Int32           total chunks for this doc
chunk_start_char  Int32           byte offset in full_text
chunk_end_char    Int32
indexed_at        Timestamp
source_hash       Utf8            MD5/SHA256 of original file bytes
tags              List<Utf8>
metadata_json     Utf8            forward-compat escape hatch (Internxt zip paths,
                                  batch IDs, session IDs, future location types, …)
```

### Indexes

| Index | Type | Column(s) | Notes |
|---|---|---|---|
| Vector | IVF-PQ | `embedding` | `num_partitions=256`, `num_sub_vectors=128` |
| Full-text | Tantivy (direct) | `full_text`, `headings_text` | separate Tantivy index, see §5 |
| Scalar | B-tree | `owner_id`, `language`, `year` | pre-filter before ANN |

---

## 5. Full-Text Search — Tantivy Direct (not via LanceDB FTS API)

LanceDB has built-in FTS via Tantivy, but its query API is too simplified for advanced
proximity + wildcard queries. We use the `tantivy` crate directly alongside LanceDB.

### Tantivy schema (parallel to LanceDB table)

```
doc_id            TEXT STORED       links back to LanceDB row
headings          TEXT STORED       boosted field (^3 at query time)
body              TEXT              full stripped text, positional index
language          FACET             for per-language filtering
owner_id          TEXT STORED       for multi-user filtering
```

The Tantivy index lives at `{data_dir}/fts/` next to the LanceDB directory at `{data_dir}/lance/`.
Both are written atomically during ingest.

### Query Translator

Parses an advanced query string → Tantivy query tree.

Supported operators:

| Query syntax | Meaning | Tantivy implementation |
|---|---|---|
| `foo AND bar` | both terms | `BooleanQuery::must` |
| `foo OR bar` | either term | `BooleanQuery::should` |
| `NOT foo` | exclude term | `BooleanQuery::must_not` |
| `"foo bar"` | exact phrase | `PhraseQuery(slop=0)` |
| `foo*` | prefix wildcard | prefix scan TermDictionary → `BooleanQuery::should` |
| `fo?` | single-char wildcard | regex on TermDictionary |
| `foo~2` | fuzzy (edit distance) | `FuzzyTermQuery(distance=2)` |
| `foo w/N bar` | within N words, either order | `PhraseQuery([foo,bar], slop=N)` + `PhraseQuery([bar,foo], slop=N)` OR'd |
| `foo pre/N bar` | foo before bar within N words | `PhraseQuery([foo,bar], slop=N)` only |
| `(foo OR bar) w/N baz` | grouped proximity | recursive parse → cross-product slop queries |

**Implementation note on `w/N`**:
Tantivy `PhraseQuery` with slop is *directional*: `["foo","bar"]` with slop N matches
"foo … bar" with up to N intervening tokens. To get bidirectional `w/N` semantics we emit
**two** phrase queries (both orderings) wrapped in `BooleanQuery::should`.
Wildcard expansion happens first via TermDictionary prefix scan, then slop queries are built
for each expanded term pair. The expansion is cached per-query.

```rust
// src-tauri/src/index/fts_query.rs
pub fn translate(query: &str, reader: &IndexReader) -> Result<Box<dyn Query>>;
```

---

## 6. Search Modes

| Mode | What runs | When to use |
|---|---|---|
| **Text only** | Tantivy BM25 | exact terms, author names, theological vocab |
| **Vector only** | LanceDB ANN | semantic / paraphrase / cross-language |
| **Hybrid** | BM25 + ANN → RRF rerank | best recall, default for large corpora |
| **Sparse+Dense** | bge-m3 sparse + dense → rerank | best when embedder is bge-m3 |

Reciprocal Rank Fusion (RRF) for hybrid reranking — simple, parameter-free, robust.

---

## 7. Extraction Pipeline (updated)

```
File drop
  → [existing] text extraction (PDF/DOCX/EPUB/OCR via pdfjs, mammoth, tesseract)
  → [new] produce three outputs:
       full_text_raw    stripped plain text (for embedding)
       full_text_md     Markdown with heading structure preserved
       headings[]       ordered list of section titles
  → chunk(full_text_raw, headings) → chunks[]
  → for each chunk:
       embed(chunk.text) → embedding (dense 1024d) + embedding_sparse
       write to LanceDB (lance/) and Tantivy (fts/)
       location_uri = FileLocation::from_current_context()
  → on Sort step: update_location(doc_id, new_uri)
```

**Why `.md` extraction?**
- Heading boundaries → semantically coherent chunks
- Heading text → boosted FTS field (headings rank higher in BM25)
- Markdown stored as `full_text_md` → rich preview rendering in UI
- Strip markdown syntax before embedding → cleaner vectors

---

## 8. Remote VPS Server

### Technology: Rust + Axum

| Criterion | Rust+Axum | Python FastAPI | Node+LanceDB |
|---|---|---|---|
| Same LanceDB crate | ✓ | ✗ | ✗ (N-API bindings) |
| Same fastembed-rs | ✓ | ✗ | ✗ |
| Static binary | ✓ (musl) | ✗ | ✗ |
| No runtime deps on VPS | ✓ | ✗ (Python env) | ✗ (Node) |
| CPU perf | excellent | good | good |

Compile target: `x86_64-unknown-linux-musl` — fully static, no glibc version concerns.

### REST API

```
POST   /v1/ingest              body: { text, metadata, location_uri } or { embedding[], metadata, location_uri }
POST   /v1/search/text         body: { query, filters, limit }
POST   /v1/search/vector       body: { embedding[], filters, limit }
POST   /v1/search/hybrid       body: { query, embedding[], filters, limit }
DELETE /v1/docs/{doc_id}
PATCH  /v1/docs/{doc_id}/location   body: { location_uri }
GET    /health
GET    /v1/stats               index size, doc count, model info
```

### Authentication

`Authorization: Bearer <api-key>` — HMAC-SHA256 signed token.
Key stored in `.env` on VPS, in Tauri secure store (OS keychain) on client.

### VPS vs local embedding

The server can embed on ingest (from raw text) **or** accept pre-computed vectors
(client embedded locally). Config flag: `server_side_embedding: bool`.
This lets a GPU-equipped client send vectors and the CPU VPS just stores+indexes.

### IndexBackend trait (shared abstraction)

```rust
#[async_trait]
pub trait IndexBackend: Send + Sync {
    async fn ingest(&self, doc: DocumentChunk) -> Result<()>;
    async fn search_text(&self, query: &str, filters: &Filters, limit: usize) -> Result<Vec<SearchResult>>;
    async fn search_vector(&self, emb: &[f32], filters: &Filters, limit: usize) -> Result<Vec<SearchResult>>;
    async fn search_hybrid(&self, query: &str, emb: &[f32], filters: &Filters, limit: usize) -> Result<Vec<SearchResult>>;
    async fn delete_doc(&self, doc_id: &str) -> Result<()>;
    async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()>;
}
```

`LocalIndex` and `RemoteClient` both implement this. The active backend is chosen at
runtime from settings, wrapped in `Arc<dyn IndexBackend>` in `AppState`.

---

## 9. Settings UI

New section "Search Index" in Settings:

```
┌─ Search Index ───────────────────────────────────────────────┐
│  [ ] Enable search index                                      │
│                                                               │
│  Backend       ○ Local    ○ Remote (VPS)                      │
│  Remote URL    [_________________________________]            │
│  API key       [_________________________________] [Test]     │
│                                                               │
│  Search mode   ○ Text only  ○ Vector only  ○ Hybrid           │
│  Embedder      [bge-m3 INT8 ▼]                                │
│  Device        ○ Auto  ○ CPU  ○ Metal (macOS)  ○ CUDA         │
│                                                               │
│  [ Re-index current session ]  [ Rebuild IVF-PQ index ]       │
│  [ Export index stats ]                                       │
└───────────────────────────────────────────────────────────────┘
```

---

## 10. Multi-user Design

- Every ingest call carries `owner_id` (user UUID from app config)
- Every LanceDB row and Tantivy document stores `owner_id`
- Search pre-filters by `owner_id` unless "all users" mode is explicitly enabled
  (admin setting on the VPS server)
- Single-user installs: `owner_id` is auto-populated, never shown in UI
- User registry (`users.json` alongside the index dir) maps `uuid → { display_name, email }`

---

## 11. Cargo Dependencies (additions to `src-tauri/Cargo.toml`)

```toml
# Search / RAG
# lancedb 0.26.2 = latest stable on crates.io. 0.27.0-beta.5 is git-only,
# adds only JS native array inference (irrelevant to us).
lancedb          = { version = "0.26.2", default-features = false }
tantivy          = "0.22"
# fastembed 5.x has real EmbeddingModel::BGEM3 + SparseModel::BGEM3 (multilingual sparse).
# lancedb has NO ort dep → no version conflict.
fastembed        = { version = "5.13.0", features = ["ort-download-binaries-native-tls", "hf-hub-native-tls"] }
arrow            = { version = "57", default-features = false }
arrow-array      = { version = "57", default-features = false }
arrow-schema     = { version = "57", default-features = false }
arrow-select     = { version = "57", default-features = false }
# ort pinned to match fastembed 5.13.0; used directly for CoreML/CUDA EP types.
ort              = "=2.0.0-rc.11"

# Utilities
uuid             = { version = "1", features = ["v4", "serde"] }
async-trait      = "0.1"
serde_json       = "1"    # already present
```

For the VPS server (separate crate `crisp-index-server`):
```toml
axum             = "0.8"
tower            = "0.5"
tower-http       = { version = "0.6", features = ["cors", "auth"] }
tokio            = { version = "1", features = ["full"] }
lancedb          = "0.14"
tantivy          = "0.22"
fastembed        = "4"
hmac             = "0.12"
sha2             = "0.10"
dotenvy          = "0.15"
```

---

## 12. Rust Module Layout (in `src-tauri/src/`)

```
index/
  mod.rs              pub re-exports, IndexBackend trait, AppState integration
  location.rs         FileLocation enum, URI parse/serialize, RetrievalCost
  schema.rs           Arrow schema builder, chunk helper types
  embedder.rs         fastembed-rs wrapper: model enum, device selection, batch embed
  fts_query.rs        advanced query → Tantivy query translator
  fts_index.rs        Tantivy index open/create/write/search
  local_index.rs      LanceDB local: open/create/ingest/search, IVF-PQ build
  remote_client.rs    HTTP client to VPS server (reqwest)
  ingest.rs           orchestration: text → chunks → embed → write both indexes
  search.rs           unified search: dispatch to text/vector/hybrid, RRF merge
```

VPS server (separate workspace member or repo):
```
crisp-index-server/
  src/
    main.rs
    state.rs          SharedState: LanceDB conn + Tantivy index + embedder
    auth.rs           Bearer token HMAC validation
    routes/
      ingest.rs
      search.rs
      delete.rs
      health.rs
      stats.rs
```

---

## 13. Phased Implementation

| Phase | Deliverable | Est. | Status |
|---|---|---|---|
| **P1** | `location.rs` — full URI model with tests | ½ day | ✅ Done |
| **P2** | `fts_query.rs` — advanced query translator with tests | 1 day | ✅ Done |
| **P3** | `embedder.rs` — fastembed-rs wrapper, model enum, device picker | 1 day | ✅ Done |
| **P4** | `fts_index.rs` — Tantivy index CRUD + search | 1 day | ✅ Done |
| **P5** | `local_index.rs` — LanceDB CRUD, IVF-PQ, vector search | 2 days | ✅ Done |
| **P6** | `ingest.rs` — full pipeline: chunk → embed → write | 1 day | ✅ Done |
| **P7** | `search.rs` — unified FTS+vector with RRF reranking | 1 day | ✅ Done |
| **P8** | `tauri_commands.rs` + `crisp-index-server` skeleton + `index_init` command | 2 days | ✅ Done |
| **P9** | Svelte Settings UI — Search Index panel in Settings.svelte | 2 days | ✅ Done |
| **P10** | `remote_client.rs` + remote mode switching in `init_index` | 1 day | ✅ Done |
| **P11** | Sort-step `update_location` hooks in `execute_batch` | ½ day | ✅ Done |
| **P12** | `.md` extraction + heading detection in extractors | 1 day | ✅ Done |
| **P13** | Internxt-zip URI parsing (stub, no retrieval) | ½ day | ✅ Done (P1) |

---

## 14. Session Continuity

### What is fully built (P1–P9 complete, cargo check ✅)

- `src-tauri/src/index/mod.rs` — `IndexBackend` trait, `IndexState` (+`local` field), `IndexConfig`, `SearchMode`, `BackendType`
- `src-tauri/src/index/location.rs` — `FileLocation` URI model (Local/Vps/Internxt/InternxtZip), tests
- `src-tauri/src/index/schema.rs` — Arrow schema, `DocumentChunk`, `SearchResult`, `SearchFilters`
- `src-tauri/src/index/embedder.rs` — `Embedder` (fastembed 5.x), correct model mappings, `chunk_text`
- `src-tauri/src/index/fts_query.rs` — advanced query → Tantivy (AND/OR/NOT/phrase/wildcard/fuzzy/w/N/pre/N)
- `src-tauri/src/index/fts_index.rs` — Tantivy index CRUD + search with owner-filter
- `src-tauri/src/index/local_index.rs` — LanceDB CRUD, IVF-PQ build, `batches_to_search_results_with_scores`
- `src-tauri/src/index/search.rs` — `SearchEngine`: FTS+ANN+RRF(k=60), parallel tokio::spawn
- `src-tauri/src/index/ingest.rs` — `IngestPipeline`: chunk→embed→write, `RawDocument`, `IngestStats`
- `src-tauri/src/index/tauri_commands.rs` — `index_search`, `index_ingest_document`, `index_update_location`,
  `index_build_ivf_pq` (uses `IndexState.local`), `index_get_config`, `index_set_config`, `index_init`
- `src-tauri/src/lib.rs` — `AppState.index`, `get_app_data_dir` command, all 7 index commands registered
- `src/lib/components/Settings.svelte` — Search Index panel: enable toggle, mode/backend/embedder/device selectors,
  remote URL+key, data-dir picker, Apply & Init button, IVF-PQ button, status indicator
- `src/lib/i18n.svelte.ts` — `settings.index.*` keys (en + de)
- `crisp-index-server/` — Axum VPS server skeleton (stub handlers)

### All phases complete — what is built (P1–P12)

All Rust backend and TypeScript frontend code compiles cleanly.

**Remaining work (non-critical):**
1. **crisp-index-server real handlers** — stub Axum routes in `crisp-index-server/` need
   real LanceDB+Tantivy implementations (same logic as `local_index.rs` and `fts_index.rs`).
2. **Frontend: pass `doc_id` + `new_location_uri` in batch execute** — `BatchExecutionItem`
   now accepts optional `doc_id` / `new_location_uri`; the Svelte batch store needs to
   populate these fields when the document was previously indexed (requires a lookup by
   `source_hash` → `doc_id` mapping stored locally).
3. **Frontend: call `index_ingest_document` after extraction** — in `store.svelte.ts` after
   `item.extractedText` is populated, call `invoke('index_ingest_document', {...})` if
   `indexEnabled` is true. Pass `markdownText` and `headings` from the extraction result.
4. **IVF-PQ direct access** — `LocalIndex` already implements `build_vector_index`; it is
   now exposed via `IndexState.local` and the `index_build_ivf_pq` command.
5. **User/machine UUID persistence** — app startup should generate and store UUIDs in
   `settings.json` (`userUuid`, `machineUuid`) and use them to build `crisp+local://` URIs.

### Cargo.toml state (src-tauri)

```toml
lancedb      = { version = "0.26.2", default-features = false }
tantivy      = "0.22"
fastembed    = { version = "5.13.0", features = ["ort-download-binaries-native-tls", "hf-hub-native-tls"] }
ort          = "=2.0.0-rc.11"
arrow        = { version = "57", default-features = false }
arrow-array  = { version = "57", default-features = false }
arrow-schema = { version = "57", default-features = false }
arrow-select = { version = "57", default-features = false }
uuid         = { version = "1", features = ["v4", "serde"] }
async-trait  = "0.1"
```

futures = "0.3" needs to be added (for TryStreamExt when reading LanceDB result streams).

---

## 15. Open Questions / Future Work

- **Chunking for scanned PDFs**: OCR produces flat text; heading detection needs heuristics
  (line length, font-size metadata from pdfjs) rather than Markdown parsing.
- **Cross-language search**: bge-m3 handles de+en in the same vector space, so a German
  query naturally retrieves English documents. Confirm with benchmark on actual corpus.
- **Internxt retrieval**: when a file at `crisp+internxt-zip://` is requested, the retrieval
  pipeline must: authenticate to Internxt → stream zip → extract single member.
  This mirrors `retrieve.py` from the cloud-backup system. Can reuse the same VPS
  as a retrieval gateway (VPS has Internxt credentials, client does not).
- **Index versioning**: when the embedding model changes, re-indexing is required.
  Track `embedding_model` per row → allow mixed-model indexes with per-model ANN subindexes.
- **Sync between local and remote**: local LanceDB can sync a subset (recent / tagged)
  to the VPS index for shared search. Use LanceDB's delta/versioning (Lance format
  is versioned by design) for efficient sync.

---

## Shipped Phases — Archived from PLAN.md

This section preserves the original specs of phases that have shipped.
Kept for context (commit history / review only tells the *what*; these
entries explain the *why* and the design choices that didn't end up in
code comments). For active work, see [PLAN.md](PLAN.md).

### P2 — Search index / RAG (full plan)

The detailed P2 plan — LanceDB schema, dtSearch query translator,
embedder selection, dense + sparse + BM25 + RRF + cross-encoder
reranking — is the §1-§13 archive at the top of this file (originally
`rag_plan.md`). All P1-P12 phases shipped; the §14 "Session Continuity"
notes record the implementation order.

### P3 — Voice chat (CrispASR integration, in-scope items)

ASR via the CrispASR sibling repo (whisper-cpp wrapper exposed through
a C library), TTS via the platform's native synth (`say` on macOS,
SAPI on Windows, `espeak` on Linux), Settings UI for voice picker /
rate / "auto-speak replies" toggle. Hotword/wake-word gating remains
an explicit non-goal for v1 and stays in PLAN.md as a pending item.

### P3.5 Phase 1 — macOS arm64 native-lib bundling

CrispEmbed + CrispASR are shipped as Cargo path-dep wrappers around
`libcrispembed.dylib` / `libcrispasr.dylib`. The post-build script
(`scripts/bundle_macos_native_libs.sh`) copies the cmake-built dylibs
+ ggml backends + transitive libs into `Contents/Frameworks/`,
patches install names with `install_name_tool`, and re-codesigns.
Pattern proven on v0.1.36's .dmg; recipe documented in LEARNINGS.md.
Phases 2 (Linux/Windows) and 3 (mobile) remain pending.

### P6 Phases 1-4 — Catalog / Cathy integration (Catfish port-and-merge)

Brought Catfish's drive-cataloging + duplicate-finding + offline
file-search into CrispSorter as a Tauri-native feature, with byte-exact
read/write of any `.caf` file produced over the past 20+ years
(Cathy 1.x → Catfish v8). The `.caf` binary format spec
(little-endian, magic = `version × 1_000_000_000 + 500_410_407`,
NUL-terminated latin-1 strings, dirs encoded as `size < 0`) was
reverse-engineered from `core/file_index.py` in Catfish.

Phases shipped:

- **Phase 1 — `.caf` I/O + parallel scanner**: `src-tauri/src/catalog/`
  — `caf.rs` (versions 1-8 reader/writer, including v ≤ 6 size quirks),
  `index.rs` (in-memory `FileIndex` with size-bucket HashMap), `scan.rs`
  (rayon-parallel walker via `jwalk`), Tauri commands
  `catalog_load_caf` / `catalog_save_caf` / `catalog_scan_dir` /
  `catalog_metadata`.
- **Phase 2 — Duplicate engine + CLI parity**: `dedup.rs` size-bucket
  fast-path with parallel hash verify (mirroring Catfish's
  `find_all_duplicates_bulk`), generate-deletion-script for bash/batch/
  powershell, JSON output mode matching Catfish's `--output json`.
- **Phase 3 — UI tabs**: `Catalog.svelte` (registry + browse/refresh/
  delete + Active toggle) and `Duplicates.svelte` (source + N
  destinations, hash dropdown, results table, deletion-script export).
  `BatchReview.svelte` gained `exportCaf()` for round-tripping.
- **Phase 4 — Hybrid storage (option C)**: `.caf` is the canonical
  on-disk form; LanceDB has a derived `catalog_entries` table (thin
  schema `(catalog_path, entry_path, size, mtime, hash?)`) populated
  on `set_active(true)`. Cross-links to the existing `documents` table
  via `entry_path`. `catalog_export_sorted` dumps batch slices to a
  fresh `.caf` for archival/sharing.

Phase 5 (extract a `crispcat` workspace crate + standalone CLI) remains
deferred-optional in PLAN.md.

### P7 Phases 7.1-7.6 + 7.8 Tiers 1-2 — Full-volume desktop search

Closed the gap between "smart sort assistant" and "general-purpose
desktop search" by extending each P6 catalog row with extracted text
content + an embedding on a background schedule, plus operator-grade
query syntax, instant preview, saved searches, and cross-mount
awareness.

- **Phase 7.1 — Unified query covering catalogs**: `index_search`
  queries both `documents` and `catalog_entries` in one pass; catalog-
  only hits surface with `catalog_source` set, score=0.4, chunk_index=-1.
- **Phase 7.2 — Operator-grade query syntax**: custom `translate()` in
  `index/fts_query.rs` parses AND/OR/NOT, phrases, w/N + pre/N
  proximity, wildcards, fuzzy, parentheses, plus field-prefix
  (`title:foo`, `body:foo`, `headings:foo` / `h:`, `text:` aliases).
- **Phase 7.3 — Live preview pane**: right-side pane in
  `IndexSearch.svelte` rendering PDF/image/text via `convertFileSrc` +
  `readTextFile`.
- **Phase 7.4 — Background full-content ingest**: per-filetype
  extractor registry (`extractors/{pdf,text,html,ocr,ocr_ocrs}.rs`),
  background ingest scheduler (`bg_ingest/mod.rs` with `tokio::Mutex`-
  guarded queue, `ForegroundGuard` RAII for QoS yielding), mtime-skip
  via `LocalIndex::indexed_mtime_for_uri` parsing `metadata_json`'s
  `{"mtime_unix": v}` shape.
- **Phase 7.5 — Saved searches**: persisted `(query, filters)` tuples
  in `tauri-plugin-store`, surfaced as a dropdown in
  `IndexSearch.svelte`.
- **Phase 7.6 — Cross-mount UUID tagging**: `volume::volume_id_for_path`
  shells out to `diskutil info` (macOS) / `findmnt -no UUID` (Linux) /
  `wmic VolumeSerialNumber` (Windows); id is packed into the existing
  `metadata_json` column alongside `mtime_unix`. New
  `volume_list_mounted` Tauri command.
- **Phase 7.6 follow-up — Search-time availability filter**:
  `index_search` now drops hits whose recorded `volume_id` isn't in
  the currently-mounted set (single shell-out per query). New
  `include_unmounted: Option<bool>` parameter overrides the filter
  for browse / inventory cases. `SearchResult` carries `volume_id`
  through the pipeline (parsed out of `metadata_json` by a new
  hand-parser mirroring `indexed_mtime_for_uri`'s shape — 5 unit
  tests pin its behaviour). UI: a "Inkl. nicht eingehängter
  Laufwerke" checkbox in `IndexSearch.svelte`'s filter row.
- **Phase 7.8 Tier 1 — Tesseract via shell-out** (`bbbca1b`): zero
  binary bloat; user installs Tesseract on demand. Hardcoded
  `eng+deu`. PDFs with empty text layer fall through when `try_ocr`
  is on; image extensions dispatch directly.
- **Phase 7.8 Tier 2 — `ocrs` (pure-Rust RTen engine)**: Apache-2.0,
  CRAFT-shaped models in PyTorch → ONNX, executed via the project's
  RTen runtime (zero system-onnxruntime dep). Adds ~10-20 MB to the
  binary. Latin-script only; German users get a hint to install
  Tesseract for better results.

Tiers 3 (usls PaddleOCR) and 4 (deepseek-ocr.rs VLM, opt-in cargo
feature) remain pending in PLAN.md, along with Phase 7.7 (mountable
`.cidx` archive index files).

### P8.1 — Configurable per-file conversion timeout

New Settings UI knob *Per-file conversion timeout* (default 120 s,
0 = no timeout = pre-P8.1 behaviour). Wraps the whole `extractDocument`
promise with `Promise.race(extract, timer)` in
`src/lib/batch/store.svelte.ts`. Distinct from the page watchdog
(`PAGE_WATCHDOG_MS = 30 s`) — they coexist: page watchdog catches
"extractor froze", total-time timeout catches "extractor making slow
but real progress on a too-big file".

### P8.2 — CLI mode (first cut)

clap-based subcommand router (`src-tauri/src/cli/mod.rs`) with argv
sniff in `main.rs` to route between CLI and GUI modes on a single
binary. Subcommands wired:

* **version** — print app version
* **doctor** — env / model / lib check
* **catalog scan / info / browse / find-dupes / gen-script /
  set-active / search** — all matching the corresponding Tauri commands

JSON Lines is the default output format; `--format text` switches to a
human-readable column view. Stateless subcommands (catalog) work today;
the stateful families (`index` / `batch` / `chat`) need a Tauri-runtime
spinup for `AppState` / `Mutex` / `AtomicUsize` and stay pending.

### Per-version changelog (was PLAN.md scratchpad)

Versioned feature entries that previously lived at the bottom of
PLAN.md. Kept here for the *what-when-why* (commit messages have the
same span but lack the rationale lines).

- **XMP metadata extraction (May 2026, v0.1.35)** — `extract_pdf_metadata`
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
- **Multi-folder watcher (May 2026, v0.1.34)** — extends v0.1.32
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
- **BibTeX export (May 2026, v0.1.33)** — pure-TS `buildBibFile` in
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
- **Folder watcher v1 (May 2026, v0.1.32)** — drop a file into the
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
- **PDF metadata pre-fill (May 2026, v0.1.31)** — new
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
- **TTS auto-speak for chat replies (May 2026, v0.1.30)** — closes the
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
- **CrispASR voice input — sidecar + push-to-talk (May 2026, v0.1.29)** —
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
- **Matryoshka dimension selection (May 2026, v0.1.28)** — new
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
- **Sparse retrieval + Octen auto-download (May 2026, v0.1.27)** — BGE-M3
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
- **Configurable model cache dir (May 2026, v0.1.25)** — new
  `IndexConfig.model_cache_dir: Option<String>` + `resolve_model_cache_dir`
  helper picks: `CRISPSORTER_MODEL_CACHE_DIR` env > UI override >
  `{data_dir}/models/`. Single dir is shared by fastembed (ONNX), hf-hub
  (external-data ONNX + GGUF embedder + GGUF reranker), so one setting
  controls every weight on disk. Settings.svelte adds a "Model cache
  directory" picker; an external volume like
  `<external-volume>/ai/crispsorter-models` lets the cache survive app
  re-installs and (partially) share with CrispEmbed CLI. Three unit tests
  pin the resolve precedence.
- **Cross-encoder reranking pipeline (May 2026, v0.1.25)** — new
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
- **Pre-existing FTS regression fixed (May 2026)** —
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
- **Query/passage prefix selection (May 2026)** — auto-apply model-specific
  prefixes via `EmbedderModel::prefix(EmbedRole)`. E5 (`query:` / `passage:`),
  Nomic v1.5 (`search_query:` / `search_document:`), BGE en-v1.5 + Mxbai
  (BGE-style query-only), Jina v5 (`Query:` / `Document:`), EmbeddingGemma
  (task templates). All other models pass through unprefixed. CrispEmbed path
  uses native `set_prefix`; fastembed/OrtPath paths prepend in Rust. Sparse
  encoders (BGE-M3, SPLADE++) untouched — trained without prefixes.
- **CrispEmbed/fastembed-rs registry sync (May 2026)** — added 12 new
  `EmbedderModel` variants (`MultilingualE5{Small,Base,Large}`, `Bge{Small,Base,Large}EnV15`,
  `NomicEmbedTextV15`, `MxbaiEmbedLargeV1`, `AllMiniLmL6V2`, `EmbeddingGemma300M`,
  `Gte{Base,Large}EnV15`). Each wired through both ONNX (native fastembed-rs
  via `CrispStrobe/fastembed-rs@feat/new-model-entries`) and GGUF (CrispEmbed
  `cstr/*-GGUF` registry). `BgeSmallEnV15` paired with `SparseModel::SPLADEPPV1`
  per `HISTORY.md` §2 rationale. Serde kebab-case test pins frontend mapper.
- Stop button — wires `AbortController` through extraction and LLM queries (v0.1.22)
- Per-request LLM timeout — 3 min local / 60 s remote via `Promise.race` (v0.1.22)
- Extraction hang timeout — 5 min auto-abort on `extractionAbort` controller (v0.1.22)
- Frontend log panel — `flog()` store, merged with Rust `app-log` events in LogPanel (v0.1.22)
- Live processing stats in footer — N/total done · extracting X · analyzing Y (v0.1.22)
- Release workflow — auto-publish draft after matrix even if one platform runner is slow (v0.1.22)
- macOS 13 / `crispembed` stub — created minimal stub so CI/dev builds resolve the optional dep
- Stuck items on resume — `resumeLastSession()` resets extracting/analyzing → unfinished (v0.1.23)
- Per-page extraction watchdog — 30 s no-progress timeout replaces flat 5-min timeout (v0.1.23)
- Two-phase batch processing — extract-all then analyze-all; LLM stall never blocks extraction (v0.1.23)
- `unfinished` status — amber badge, filter option, footer counter, resetStuckItems handles it (v0.1.23)
- i18n status strings — all BatchStatus values translated EN + DE; Chat/BatchReview use them (v0.1.23)
- Chat context title/author — shows suggestedTitle + suggestedAuthor for analyzed docs (v0.1.23)
- Stop button during rate-limit wait — `abortableSleep()` makes 429 backoff honour AbortSignal (v0.1.23)
- Rate-limit Retry-After cap — capped at 90 s to prevent 10-min dead waits (v0.1.23)
- Provider round-robin fallback — processAll phase 2 cycles through fallback providers on failure (v0.1.23)
- Round-robin Settings UI — ordered checklist in LLM Options with up/down reorder (v0.1.23)
- Index location update on move — `index_update_location_by_path` Rust command + TS call (v0.1.23)
- i18n audit: Chat.svelte — "Docs:", "Chat:", "Clear Messages" use i18n keys (v0.1.23)

---

## Archived phase specs — 2026-05-09

The following are full design documents for phases that have shipped.
Kept here for "why does this code look this way" context.
See PLAN.md for the current active plan and open items.

### P3 — Voice chat / CrispASR (shipped except hotword/wake)

Full-spec in earlier HISTORY entries. Core shipped: Whisper + CrispASR
backend, ASR UI in Chat panel, TTS, push-to-talk, Rust audio bridge.
Remaining: hotword/wake word (out of scope for v1).

### P4 — Code quality / maintenance (shipped)

Model-cache boot-drive hint, CARGO_TARGET_DIR redirect, i18n audit
(Settings.svelte + LogPanel.svelte ~80 strings). All shipped.

### P5 — Future / planned

Auto-process toggle on watch detection (needs UX design), PWA demo
(File System Access API). Deferred.

### P6 — Catalog / Cathy integration (shipped, Phase 5 deferred)

.caf I/O, parallel scanner, duplicate engine, deletion-script generator,
Catalog/Duplicates UI tabs, hybrid-storage catalog_entries Lance table.
Phase 5 (crispcat workspace crate extraction) optional/deferred.

### P7.1–P7.6 — Full-volume desktop search (shipped)

Unified catalog/documents search, operator-grade query syntax, live
preview pane, background full-content ingest, saved searches, cross-mount
UUID tagging with availability filtering, Tesseract + ocrs OCR (Tiers 1-2).

### P8.1 — Per-file conversion timeout (shipped)

Settings knob conversionTimeoutSeconds, page watchdog in JS extractor.

### P9 — Übersicht at million-file scale (fully shipped)

8 steps: index_query_documents + columnar Übersicht, parent_dir column +
scalar index, folder-tree breadcrumb + index_folder_children, DB-side
ORDER BY via lance::Scanner, column registry + persistence, volume_id
column + scalar index, preview pane, mtime/size/parent_dir metadata.

### P10 — Robust ingest at scale (shipped, minor items remain)

TaskFailureReason enum, extraction timeouts (300s), L2 fallback via
ingest_l2_row, EPUB DRM detection, N-worker bg_ingest, Übersicht failure
badges + retry button, skip non-retryable failures on re-run.
Remaining: DRM help-popover (clickable), CLI --skip-failed (deferred).

### P11 — Remote-server architecture (partially shipped)

Shipped: IndexBackend trait, RemoteClient, crisp-index-server (Axum,
real LanceDB+Tantivy+RRF+IVF-PQ), crisp-index-protocol wire types,
async SQLite job queue (both tiers), batched ingest, embedderLocation
config, local single-writer queue, UI wired to durable job queue,
server-side embedding (SERVER_EMBED=1).
Remaining: server queue blob-size fix (store refs not full embeddings),
IVF-PQ at 100M+, runtime modes enum, cloud drives, SyncManager.

### P12 — cloud-backup integration (L1 shipped, L3 shipped 2026-05-09)

Shipped: index_ingest_cb_manifest (source_files → L1 LanceDB rows,
crisp+cb-archive:// URI scheme), index_promote_cb_archive (retrieve.py
bridge for L3 promotion), CloudDownload button in Übersicht.
Remaining: reverse lookup UI, VPS-trigger indexing hook, global_catalog sync.

### P13 — Image-vertical convergence with CrispLens (future)

CLIP image embedder, face recognition (SCRFD+ArcFace), Images tab.
Deferred until P11 server + sync layer is stable.

### P15 — Batch pre-processing (shipped 2026-05-09)

P15a: content-dedup (size→SHA-256, duplicateGroupId/isDuplicatePrimary,
orange row tint, "Duplikate überspringen" checkbox).
P15b: book-chapter detection (ISBN-13 prefix, fm/001/bm suffix priority,
representative LLM pass only, metadata propagation, edited-volume toggle).

