# Session prompt — implement P13 Bilder vertical (Photos / images)

Use this verbatim as the opening prompt for a fresh session.  The plan
this implements is **already written**; your job is to execute it
slice-by-slice, stopping between slices for user approval.

---

## What this session is

Implementation of [`docs/P13_Bilder_integration.md`](P13_Bilder_integration.md) in
the `CrispSorter` repo.  The plan was written in a prior session
(commit `aaaf22c`, 2026-05-10).  **Read that doc end-to-end before
writing any code.**  It has:

- Three-tier architecture (Tier 0 = nothing, Tier 1 = local-only,
  Tier 2 = CrispLens HTTP enhancement)
- Endpoint map of which CrispLens routes you consume + explicit list
  of routes you do NOT consume (BFL, editing, etc.)
- `crisplens-protocol` workspace crate type sketch
- Settings table including **Keychain-backed token storage** (NOT
  tauri-plugin-store JSON — credentials would leak via backup/sync)
- 10-slice breakdown with hours: A1–A4 = Tier 1 (~25 h); B1–B5 =
  Tier 2 (~33 h)
- Risk register including the EXIF-GPS leak via `SyncManager` (must
  strip `gps_lat`/`gps_lon` before any push)
- Implementation skeleton with trait signatures, no impl
- "How to start a fresh session" block at the end (you're reading it)

## Working environment

- **Repo**: `/Users/<user>/code/CrispSorter`
  - Main branch is `main`; the prior session left it green at commit
    `aaaf22c` (`git log --oneline | head -3` confirms)
  - svelte-check: 0 errors, 38 unrelated warnings as of `aaaf22c`
  - Workspace tests (`cargo test --workspace`): 232 tauri-app + 20
    crispcat = 252 passing, 0 failed, 2 ignored
- **CrispLens (Tier 2 target)**: cloned at
  `/Users/<user>/code/CrispLens`.
  Authoritative endpoint reference for Tier 2 work is
  `electron-app-v4/server/routes/*.js` (v4 is the production
  target per the CrispLens README).  v2 (`routers/*.py`) is the
  Python FastAPI variant with equivalent surface; consult only if v4
  is missing something.
- **Cargo target dir**: handled automatically by a zsh wrapper in
  `~/.zshenv` that routes every build to
  `<external-volume>/code/cargo-target/<reponame>/`.  Do NOT set
  `CARGO_TARGET_DIR` manually — the wrapper handles it.  Confirm
  with `type cargo` from a fresh shell.
- **Disk state at handover**: boot disk has ~13 GB free, external
  ~15 GB free.  Each full Tauri rebuild is ~5-10 GB.  Run
  `cargo clean -p tauri-app` if you hit ENOSPC on `<external-volume>`.

## Conventions you must honour

These are persistent rules saved in the memory system; the previous
session learned them the hard way.

1. **Use `python`, not `python3`** for any spawn-Python paths.  On
   this machine `python` is Miniconda's interpreter with the project
   deps; `python3` does not have them.  See
   `~/.claude/projects/-Users-<user>-code-CrispSorter/memory/python_interpreter.md`.
2. **Don't poll `gh` aggressively.**  Past sessions hit GitHub rate
   limits during release watching.  When monitoring CI: default
   sleep ≥ 60 s; for one-shot waits use Bash with `run_in_background`
   and an `until` loop, not Monitor's tail-f loop.
3. **No emojis in any source file** unless the user explicitly asks.
4. **Don't create `.md` docs unless the user asks**, except: this
   prompt was authored as a doc on the user's explicit request, and
   P13_Bilder_integration.md was likewise requested.

## Scope of this session

**Slice A1 only**, unless the user explicitly approves continuing.

> **Slice A1 (~10 h)** — Bilder tab UI scaffold + image-row filter on the
> existing LanceDB index.
>
> Acceptance:
> - New tab "Bilder" between "Übersicht" and "Archiv" in the main
>   IndexIngest layout
> - Tab renders a grid (CSS grid, no virtual scrolling yet — defer
>   that to A2) of the same indexed rows currently shown in Übersicht,
>   filtered to `ext IN ('jpg','jpeg','png','webp','heic','heif','tiff','bmp')`
> - No thumbnails yet (deferred to A2); show a placeholder tile with
>   filename + extension badge
> - Tab loads in <500 ms on the existing 252-test corpus
> - `cargo test --workspace` and `npm run check` both clean
> - Commit, push, **stop**, ask user before starting A2

After A1 lands and the user approves: continue with A2
(`docs/P13_Bilder_integration.md` → "Slice breakdown" table).  Each
slice gets its own commit; never bundle.

## Files most likely to need touching for A1

These are reads-first targets; don't pre-create anything.  Use the
`Explore` agent (`subagent_type: "Explore"`) if you need to map
broader regions of the codebase.

- `src/lib/components/IndexIngest.svelte` — main tabbed UI
  - `type Tab = 'overview' | 'search' | 'add' | 'sources' |
    'cafCatalog' | 'duplicates' | 'cidxArchive'` near line ~72.  You
    add `'bilder'` to this union.  Then add a tab button +
    `{#if activeTab === 'bilder'}` block following the existing pattern
    (the `'sources'` block is a good template — it's around line 1995
    in the post-`aaaf22c` file, but verify with grep)
- `src-tauri/src/lib.rs` — Tauri command registration
  - You'll add `bilder::tauri_commands::bilder_list` here once you
    define it; mirror how `drives::tauri_commands::drive_list` is
    registered.
- `src-tauri/src/bilder/mod.rs` — new module, define the trait + the
  `LocalBilder` impl per the spec's "Implementation skeleton".  For
  A1 you only need `list(filters)` working; everything else returns
  `unimplemented!()` or empty defaults.
- `src-tauri/src/bilder/tauri_commands.rs` — new, single command
  `bilder_list(state, page, ext_filter)` that delegates to
  `LocalBilder::list` and returns `ImagesPage` (defined in
  `crisplens-protocol`).

## Things that will probably trip you up

1. **Svelte 5 runes**: this codebase uses `$state` for reactive state.
   Read `IndexIngest.svelte` lines 87–106 for the pattern before
   introducing new state.
2. **i18n strings**: there's an `i18n.svelte` import.  Check
   `src/lib/i18n.svelte.ts` for the key-naming pattern; new tab
   labels go in the language tables there.
3. **`activeTab` persistence**: existing tab state is persisted via
   `tauri-plugin-store`; check `loadFolders`/`saveFolders` for the
   pattern.  Bilder may or may not want to persist; ask if unclear.
4. **`crisplens-protocol` crate**: don't create it for A1 — Tier 1
   doesn't need shared wire types.  Defer to B1.  For A1, define
   `ImagesPage` / `Image` directly in `src-tauri/src/bilder/types.rs`
   and move them to a workspace crate later when Tier 2 lands.
5. **Image extension filter**: Tantivy + LanceDB.  The filter goes on
   the LanceDB scan path (where the existing `loadContents` flow
   queries), not on the Tantivy FTS path.  Look at the existing
   `contents = $state<any[]>([])` flow in `IndexIngest.svelte` and
   the `index_list_documents` Tauri command (or whatever the current
   name is — grep `pub async fn index_list`).
6. **Build-disk pressure**: if you hit `No space left on device`,
   `rm -rf <external-volume>/code/cargo-target/CrispSorter/debug/incremental`
   buys ~1.5 GB without nuking compilation results.

## When you finish A1

```text
git status --short                    # should be clean except untracked .lock/.sh
cargo test -p tauri-app --lib bilder  # new module passes
cargo test --workspace --lib          # nothing else regressed
npm run check                         # 0 errors, no NEW warnings
git log --oneline -3                  # your A1 commit at HEAD
```

Then post a status message in the format:

```
Slice A1 done — commit <sha>.  Acceptance criteria met:
  - tab renders
  - N image rows shown from the test corpus
  - tests pass
Ready for A2?  Or pivot?
```

…and **stop**.  Do not start A2 without explicit user approval.

## Project history that's load-bearing for this work

Quick orientation for context that's not in PLAN.md:

- Prior session (2026-05-09/10) shipped the full P11 cloud-drives
  pillar end-to-end across 3 repos (CrispSorter + filen-python +
  internxt-python), including upstream server-side bug fixes.  See
  HISTORY.md → "2026-05-09/10 — P11 cloud drives end-to-end".  Pattern
  to emulate: HTTP-backed sibling service, protocol crate, runtime
  mode in `IndexConfig`, settings UI.  Bilder Tier 2 is the same
  shape.
- The user prefers ENV-var-driven configuration over symlinks and
  per-repo config files; you've already inherited the `cargo()`
  wrapper via `~/.zshenv` (verify `type cargo`).
- Existing OCR pipelines already touch image files (`.jpg/.png/.webp/.heic`)
  for text extraction.  Those rows will have `ext` set to the image
  extension already — your filter just selects them.

## Out-of-scope reminders (so you don't drift)

- No CLIP integration in CrispSorter (semantic search is delegated
  to CrispLens in Tier 2)
- No face detection in CrispSorter (same — delegate to CrispLens)
- No image editing in CrispSorter (open-in-CrispLens for crop /
  convert / etc.)
- No BFL/FLUX generation
- Don't refactor IndexIngest.svelte — it's huge and pre-existing.
  Add the tab, don't restructure the file.

## If the user asks for something not in P13_Bilder_integration.md

That spec is intentionally a contract.  Before deviating:
- Quote the relevant section of the doc
- State what's being changed and why
- Ask for confirmation before writing code

The "explicit out-of-scope" list at the bottom of the doc is the
strongest version of this — refusing to add BFL or editing routes
even if asked, because the spec says so.  If the user overrides
that list, fine — but flag it as an explicit scope expansion.
