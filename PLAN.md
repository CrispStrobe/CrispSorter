# CrispSorter — Development Plan

## Open TODOs

### P0 — Correctness / UX blockers

- [ ] **Stuck "extracting/analyzing" items on resume** — When the app resumes a saved session,
  items that were mid-extraction/analysis (status `extracting` | `analyzing`) from a previous run
  are shown as if they are actively processing, which is incorrect.  
  **Fix:** In `BatchManager.resumeLastSession()` reset any `extracting`/`analyzing` items to a
  new `unfinished` status (distinct from `queued`) so the user knows they were interrupted.
  Add an "unfinished" badge in the UI.  The status counter in the footer must exclude unfinished
  items from the "extracting N / analyzing N" live counters.

- [ ] **Per-page extraction timeout** — 5-minute flat timeout per file is too coarse.
  Large scanned PDFs can legitimately take many minutes; small ones that hang after 2 pages
  should be killed much earlier.  
  **Fix:** Wire the `onProgress` page callback to a per-page watchdog: if no progress event
  arrives within 30 s, abort the extraction.  Reset the watchdog timer on every page callback.

- [ ] **Decouple extraction from LLM analysis** — Currently `processAll` processes items one by
  one (extract → analyze → next).  If the LLM queue stalls (rate limit, timeout), extraction
  for all remaining items is blocked too.  
  **Fix:** Two-phase approach: first pass extracts all items (text only, no LLM), second pass
  runs LLM analysis on all items with `extractedText`.  Both passes respect `stopRequested`.

### P1 — Rate limits & provider round-robin

- [ ] **Provider round-robin for rate limits** — When a remote provider hits 429 / exhausts
  retries, automatically fall back to the next configured provider in a user-defined list.  
  **Design:** New setting `roundRobinProviders: string[]` (ordered list of provider IDs).
  `LLMClient` keeps a `currentProviderIdx` and advances it on unrecoverable errors.
  Resets at the start of each `processAll`.  
  **UI:** Settings section "Rate-limit fallback" with a drag-reorderable list of enabled providers.

### P2 — Missing i18n strings

- [ ] Add the following keys (EN + DE) to `i18n.svelte.ts`:
  - `batch.status_queued` — "Queued" / "Wartend"
  - `batch.status_extracting` — "Extracting" / "Extrahieren"
  - `batch.status_analyzing` — "Analyzing" / "Analysieren"
  - `batch.status_review` — "Review" / "Prüfen"
  - `batch.status_unfinished` — "Unfinished" / "Unterbrochen"
  - `batch.status_done` — "Done" / "Erledigt"
  - `batch.status_error` — "Error" / "Fehler"
  - `batch.reset_stuck` — "Reset stuck" / "Hänger zurücksetzen"
  - `batch.processing_stats` — "{done}/{total} done · extracting {ext} · analyzing {llm}"
    / "{done}/{total} erledigt · extrahiere {ext} · analysiere {llm}"
  - `settings.roundrobin_title` — "Rate-limit Fallback" / "Rate-Limit Ausweich-Anbieter"
  - `settings.roundrobin_hint` — "If the active provider hits its rate limit, fall back to
    providers in this order." / "Falls der aktive Anbieter das Rate-Limit erreicht, wird auf
    diese Anbieter ausgewichen."

### P3 — Chat context panel

- [ ] **Show title + author in Chat context list** — If a document has been analyzed
  (`suggestedTitle` / `suggestedAuthor` set), display them beneath the filename in the context
  sidebar.  Fall back to filename only for unanalyzed items.

### P4 — Code quality / maintenance

- [ ] Audit all remaining hardcoded UI strings in `BatchReview.svelte`, `Settings.svelte`,
  `Chat.svelte`, `LogPanel.svelte` and move them to `i18n.svelte.ts`.

- [ ] `BatchManager.executeBatch` does not pass `doc_id` / `new_location_uri` to the Rust
  `execute_batch` command (they exist in the Rust struct but the TS side omits them).  Wire up
  index location updates for moved documents.

---

## Done

- [x] Stop button — wires `AbortController` through extraction and LLM queries (v0.1.22)
- [x] Per-request LLM timeout — 3 min local / 60 s remote via `Promise.race` (v0.1.22)
- [x] Extraction hang timeout — 5 min auto-abort on `extractionAbort` controller (v0.1.22)
- [x] Frontend log panel — `flog()` store, merged with Rust `app-log` events in LogPanel (v0.1.22)
- [x] Live processing stats in footer — N/total done · extracting X · analyzing Y (v0.1.22)
- [x] Release workflow — auto-publish draft after matrix even if one platform runner is slow (v0.1.22)
- [x] macOS 13 / `crispembed` stub — created minimal stub so CI/dev builds resolve the optional dep
