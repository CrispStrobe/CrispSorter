# Workflow UX improvements

Review date: 10 November 2026. Baseline: `0fa27178` on `main`.
Implementation branch: `feat/ux-workflow-essentials`.

## Findings and delivery scope

CrispSorter already has a broad document toolset. The priority is to make the
existing daily workflows reliable and easy to discover.

| Priority | Observed issue | Planned improvement |
| --- | --- | --- |
| P0 | The batch duplicate panel groups files by size (or size and name), then offers deletion. The separate hash-based detection is not used by that panel. | Verify content hashes, report unverified files, explicitly distinguish removing batch entries from deleting files, recheck copies before deletion, and retain failed operations. |
| P1 | Several Library filters are shown but omitted from the local search command; saved searches restore only part of the search state. | Apply supported filters consistently through retrieval, save and restore complete search recipes, and expose active filters with a clear reset. |
| P1 | Select-all compares counts rather than membership; bulk actions can include selected files hidden by a filter. | Make selection and action scope explicit, use actual visible membership, and prevent hidden rows from being changed accidentally. |
| P1 | Changing the focused document discards unsaved metadata edits; empty filtered results look like an empty batch. | Preserve metadata drafts while navigating, show draft state and explicit save/discard, and provide stage-aware filters and useful empty states. |
| P2 | Numerous destinations are hard to discover; short windows can clip sidebar actions and collapsed navigation lacks accessible names. | Searchable keyboard navigation, capability-aware destinations, focus management, accessible active states, and scrolling navigation. |

The scope preserves explicit review before sorting and existing local-first
behavior. Active OCR development is covered by separate branches and is outside
this increment.

## Validation and delivery

- Keep changes in small commits on the remote feature branch as each coherent
  part is ready.
- Run TypeScript/Svelte checks, relevant pure unit tests, and fixture-backed
  browser regressions for desktop and mobile behavior.
- Exercise content mismatch, failed hashing/deletion, hidden selections, draft
  navigation, filter reset, restored searches, and keyboard/focus behavior.
- Check native search filters with focused tests where the native build
  environment is available; record any unverified native execution explicitly.
- Treat browser IPC fixtures as UI/integration evidence, not as native filesystem
  or model-accuracy qualification.

The baseline Svelte check has zero errors and 54 existing warnings. Final results
and any remaining limitations will be recorded here when implementation is
complete.
