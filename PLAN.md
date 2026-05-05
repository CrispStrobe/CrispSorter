# CrispSorter — Active Plan

> What still needs doing. Done items move to `HISTORY.md`.
> Everything below was scoped on **2026-05-05**.

---

## 1. Quick wins / polish

### 1.1 i18n & navigation labels

The left nav and Index tab strings are hard-coded German/mixed. Bring them under `i18n.t.nav.*`:

| Current | DE | EN |
|---|---|---|
| `Index Suche` (hard-coded) | `Kataloge` | `Catalog` |
| `Index Ingest` (hard-coded) | `Einlesen` | `Ingest` |
| `Logs` (hard-coded) | `Logs` | `Logs` |
| `Stapel` | `Stapel` | `Batch` |

`Duplicates` already has `dupe_*` keys but the German is fine — verify all panes use them.

### 1.2 About panel

- Add app version + Tauri version to the About card (read `__APP_VERSION__` from Vite or load from `package.json`).
- Investigate empty Open-Source-Licenses list — `fetch('/licenses.json')` should resolve from the static bundle. Add a visible loading / error state and a "regenerate" hint when empty.

### 1.3 Tesseract Model Manager — refresh button

Add a Refresh button that re-checks which language packs are installed. Hard-coded `isDownloaded` flags need to come from the on-disk `tessdata/` cache (Tesseract.js stores them in IndexedDB / tessdata cache dir). Expose a Rust command `list_tesseract_models(cache_dir)` that scans for `*.traineddata`.

### 1.4 Keyboard shortcuts

- **Stapel grid** — `Delete` / `Backspace` removes the selected entries (`batchManager.removeItems(selectedIds)`); ignore when focus is in a text input.
- **Detail pane Metadaten** — `Enter` in any of Title / Author / Year inputs commits via `saveDetailChanges()`; `Escape` reverts to the item's saved values.

---

## 2. Extractor coverage

Add three new file types to the extraction pipeline. `extractors/index.ts` routes by extension; the supported-extension lists in `BatchReview.svelte`, `IndexIngest.svelte`, and `scan_folder` (Rust) all need updating.

### 2.1 HTML / HTM

`htmlExtractor.ts`: parse with `DOMParser`, strip `<script>`/`<style>`, walk headings (`h1..h6`) for the markdown view, preserve paragraph breaks as `\n\n`. Pure browser, no extra deps.

### 2.2 WebP

`imageExtractor.ts`: feed the bytes to Tesseract.js (which already handles WebP via the browser's image decoder). Reuse the existing `tesseract.js` dependency. Same hook for PNG/JPG could be added later.

### 2.3 DOC (legacy MS Word)

Browser support is poor. Two options:
1. **Native (preferred)** — call out to `antiword` / `catdoc` if present; otherwise show "Install antiword to extract .doc, or convert to .docx".
2. **JS fallback** — try `mammoth` (it transparently rejects .doc, but better message).

Ship option (2) first; option (1) can land later behind a Rust command if user demand exists.

---

## 3. Multi-level Ingest

Current ingest is "all-or-nothing" — text extraction + embedding for every file. Add three explicit levels users can pick per-batch and upgrade later.

| Level | What is captured | Cost |
|---|---|---|
| **L1 — filesystem** | absolute path, filename, ext, bytes, mtime, ctime, parent folder | sub-millisecond per file |
| **L2 — file metadata** | PDF Info dict + XMP, DOCX `core.xml`, EPUB OPF, image EXIF, audio tags | ~10 ms per file |
| **L3 — content + embedding** | full text, headings, embeddings → LanceDB + Tantivy | seconds per file |

### 3.1 Schema additions

Add columns (or `metadata_json` keys) to the LanceDB table:

```
analysis_level    Int8        1 / 2 / 3
fs_size           Int64       bytes
fs_mtime          Int64       Unix ms
fs_ctime          Int64       Unix ms
parent_dir        Utf8        absolute path of parent
file_meta_json    Utf8        JSON of L2 metadata (creator, producer, …)
```

L1 rows have `chunk_index = -1` and no embedding (FixedSizeList allows nulls). L2 fills `file_meta_json`. L3 adds chunk rows and embeddings exactly as today.

### 3.2 Rust commands

- `index_ingest_l1(paths: Vec<String>)` — fast filesystem scan, batch insert metadata-only rows
- `index_ingest_l2(doc_ids: Vec<String>)` — open each file, extract embedded metadata via `lopdf` / `quick-xml` for OPF / `kamadak-exif` / etc., upgrade rows
- `index_ingest_l3(doc_ids: Vec<String>)` — existing pipeline, but reuse the row already at L1/L2 instead of inserting fresh

### 3.3 UI

In the Ingest tab, surface a level picker (radio: L1 / L2 / L3) for new batches and a "Promote selection to L2 / L3" action in the Catalog.

---

## 4. Catalog filters & combined view

Replace the current Index Ingest "Index-Inhalt" tab with a richer **Kataloge** view that lists every catalogued file regardless of level, with the following filters:

- **Folder / subtree** — pick a path; only files under it
- **Name substring**
- **Extension** — multi-select chips (pdf, docx, …)
- **Date range** — added / modified
- **Level done** — L1 / L2 / L3 toggle chips
- **Completeness** — has author? has title? has year? (only relevant where they apply)

Filter state lives in URL hash for shareable views. Bulk-selection toolbar (already partly built for delete) gains: "Promote to L2", "Promote to L3", "Open folder".

The current `BatchReview` (Stapel) flow is for *staging files for AI sorting*. The Kataloge view is for *managing the persistent index*. They share extractors and the underlying LanceDB rows.

---

## 5. Multi-catalog management

Today there is one global LanceDB+Tantivy directory. Add named catalogs so users can keep separate libraries side-by-side:

- `catalogs.json` config (list of `{ id, name, data_dir, embedder_model, embedder_device }`)
- Settings → Search Index gains a Catalogs list (create / rename / delete / select active)
- `index_init` is parameterised by catalog id; switching catalogs reloads `IndexState`
- Each catalog stores its embedder model — if you switch to a catalog with a different model, the embedder swaps

This unblocks separating, e.g. "Theology library" from "Music PDFs" without re-indexing.

---

## 6. Backend selector clarification

The existing radio in Settings → Search Index already maps to:

- `onnx` → fastembed-rs (our fork at `CrispStrobe/fastembed-rs`)
- `gguf` → CrispEmbed

Update the labels to say so explicitly, and put a one-line hint under each option so users understand the trade-off (CPU/GPU support, file format, quantisation differences).

---

## 7. Inherited backlog (still relevant)

From the previous ROADMAP:

- **Wire CrispEmbed sparse encoding into search pipeline** — BGE-M3/SPLADE sparse vectors via GGUF backend
- **Add more GGUF-backed models to UI** — CrispEmbed supports 43+; CrispSorter exposes ~12. Need ONNX `EmbedderModel` enum variants for the rest
- **CrispEmbed reranking in search** — APIs are wired, not yet used by the search pipeline
- **Custom output path template** — already configurable; surface the `{Filename}` token everywhere
- **BibTeX / Zotero export** from batch metadata
- **Read PDF metadata before LLM** — pre-fill Title/Author/Year from XMP/DocInfo (overlaps with L2 above; do once)
- **Folder Watcher** — auto-ingest from a watched directory
- **Reranking pipeline stage** — cross-encoder rerank top-N after RRF
- **Matryoshka dimension selection** — expose CrispEmbed `set_dim()` in Settings
- **Query/passage prefix selection** — auto-apply `query: ` / `search_query: ` prefixes
- **PWA demo** — `.sh`/`.bat` sorting scripts or browser-based sorting via File System Access API
- **`crisp-index-server` real handlers** — replace stub Axum routes with actual LanceDB+Tantivy logic

---

## Execution order

### Done in this session (2026-05-05)

1. ✅ PLAN.md + HISTORY.md split
2. ✅ i18n nav labels — `nav.catalog`/`nav.ingest`/`nav.logs` keys + DE strings
3. ✅ About panel — version pill (Vite-injected from `package.json`) + license loading/error/empty states
4. ✅ Tesseract refresh button — probes IndexedDB + Cache Storage for cached `.traineddata`
5. ✅ Keyboard shortcuts — Delete/Backspace removes selection in Stapel; Enter saves Metadaten, Esc reverts
6. ✅ Extractors — HTML/HTM (DOMParser, charset-aware), WebP/PNG/JPG/BMP/TIFF (Tesseract OCR), legacy DOC stub with friendly error
7. ✅ Backend selector clarification — toggle now labelled "FastEmbed (ONNX)" vs "CrispEmbed (GGUF)" with engine-specific hint text
8. ✅ L1 quick-scan command + UI — Rust `index_ingest_l1` writes filesystem-only rows via `metadata_json` escape hatch; Ingest tab has L1/L3 picker
9. ✅ Catalog filters in the contents tab — depth (L1/L3), extension chips, completeness select; level badge per row

### Conventions (do not break these)

- **All user-facing strings go through `i18n.t.*`.** Both `en` and `de`
  must be filled in `src/lib/i18n.svelte.ts`. No inline German (or
  English) literals in `.svelte` templates or in Tauri-emitted strings
  the UI displays. The audit pass for this is open below.
- **Log messages stay in English.** They land in `app_log!` /
  `logInfo` / `logWarn` / `logError` and feed the Logs panel + stderr;
  consistency matters more than localisation. A handful of `init_index`
  events currently leak German status text — clean them up.
- **Catalog data-dir defaults to the app data dir,** not a fresh
  per-session path. Multi-catalog management overrides this per-entry.

### Still open

- **Image EXIF metadata at L2** — current L2 covers PDF / DOCX / EPUB. Adding `kamadak-exif` for image EXIF is straightforward, ~30 lines.
- **Promote selected to L3** action — L1→L2 ships in this session. L1→L3 needs a UI button that fans out to `index_ingest_document` for each selected row.
- **Folder/subtree filter** — currently the Catalog has name / ext / level / completeness filters; folder-tree view is the missing chip.
- **Build script** — `scripts/generate-licenses.js` was made permissive when `cargo-license` is missing; CI that wants strict licenses can set `LICENSES_REQUIRE=1`.
- **Inherited backlog** in §7 — unchanged.
- **i18n / log audit** — recent commits added inline German strings in
  several `.svelte` templates (Hinzufügen depth chips, Settings →
  Kataloge panel, CAF import/export buttons, etc.) and a few German
  Tauri-emitted status messages from `init_index` (e.g. "Lade
  Embedder-Modell …", "Embedder geladen", "Embedder übersprungen …").
  All of those need to: (a) move into `i18n.t.*` with EN+DE entries
  (.svelte templates), or (b) be re-written in English (Rust log/
  status strings emitted to the frontend).
- **Expand GGUF model registry** — CrispEmbed currently supports 24 models. CrispSorter today surfaces ~12 of them. Add the remaining 12 (bge-small/base/large-en-v1.5, multilingual-e5-base/large, mxbai-embed-large-v1, gte-small, all-MiniLM-L6-v2, all-mpnet-base-v2, nomic-embed-text-v1.5, granite-embedding-278m/107m, F2LLM-v2-0.6B, Harrier-OSS-v1-0.6B/270M, arctic-embed-xs) — each needs an `EmbedderModel` variant, display/dims/ctx, `to_model_spec` ONNX repo, dropdown option + i18n.
- **Default-on CrispEmbed for dev builds** — `npm run tauri dev` runs `cargo run --no-default-features`, which excludes the `crispembed*` features. Mitigated this session by adding `enable-crispembed.ps1` / `enable-crispembed.sh` that auto-clone the sibling repo, download the prebuilt CrispEmbed C++ library from the GH release, set `CRISPEMBED_SYS_LIB_DIR`, and run dev/build with the right Cargo feature flag. `recompile.ps1` / `recompile-exe.ps1` now auto-detect a staged prebuilt and delegate to `enable-crispembed.ps1`, so a developer who has run the enable script once gets CrispEmbed in every subsequent dev/build run with no extra commands. `--no-crispembed` opts out per-run. The Settings panel hint and `README.md` § "Optional: CrispEmbed (GGUF) backend" both document the flow.

- **CrispEmbed CI: per-target lib tarballs (cross-repo)** — the upstream
  `CrispEmbed` release workflow currently produces one CPU tarball per OS.
  CrispASR's workflow already does the right thing (Vulkan / CUDA / Metal
  variants, standardised `<bundle>/src/`, `<bundle>/src/Release/`,
  `<bundle>/ggml/src/`, `<bundle>/include/`, `<bundle>/ggml/include/`
  layout that `crispasr-sys/build.rs` probes for). Concrete delta:
    1. Mirror CrispASR's matrix in `CrispEmbed/.github/workflows/release.yml`
       — add CUDA-Linux/CUDA-Win/Vulkan-Win/Metal-mac jobs.
    2. Switch the tarball layout from flat `pkg/` to the standardised
       sub-tree.
    3. Update `crispembed-sys/build.rs` `has_prebuilt()` to probe
       `src`, `src/Release`, `ggml/src`, `ggml/src/Release`.
    4. Add `emit_runtime_rpath` to `crispembed-sys/build.rs` (mirror
       `crispasr-sys/build.rs:79–102`) so downstream apps don't need to
       chain `DEP_CRISPEMBED_LIB_DIR` rpath flags.
    5. Publish `crispembed-sys` to crates.io (currently has no `description`
       / `license` / `repository` so it's git-only).
  Once these land, CrispSorter can switch its `Cargo.toml` from path-dep
  to a versioned dep, and `enable-crispembed.ps1` can drop the sibling-repo
  clone step.
