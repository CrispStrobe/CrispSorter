# CrispSorter — Development Plan

## Open TODOs

### P4 — Code quality / maintenance

- [ ] Audit remaining hardcoded UI strings in `Settings.svelte` (model manager sections)
  and `LogPanel.svelte` and move them to `i18n.svelte.ts`.

---

## Done

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
