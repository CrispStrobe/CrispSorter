# CrispSorter v0.3.0

## Highlights

**One search box for everything.** A new top-level `crispsorter search "query"`
queries your local index *and* the configured cloud-backup corpus in a single
call, RRF-merges the two result sets, and badges each hit with its source. No
more guessing which of three search verbs to reach for.

**Search results that feel like a search engine.** Hits now render a
`<mark>`-highlighted ~300-character snippet centred on the matched term instead
of a wall of body text, and any result with a source URL gets a one-click
"Open original" action straight to your browser.

**Pulled cloud rows are searchable offline.** `sync cloud-backup pull` now
writes each pulled L1 row into the local Tantivy index as it lands, so a pulled
corpus (e.g. 50K wallabag articles) is findable via local `index search` with
no network round-trip and no federated flag.

---

## Unified `search` verb

```
crispsorter search "schimmelpilz"
```

- **Always queries local.** If a cloud-backup URL is configured, pull is
  enabled, and a token is stored, it *also* queries the cb-api corpus over the
  v2 hybrid path and RRF-merges the two legs.
- **`--local-only` / `--cloud-only`** force a single leg.
- **Shared filters** apply to whichever legs run: `--ext`, `--lang`,
  `--folder-prefix`, `--year-min` / `--year-max`, `--url-domain`, `--tag`. On
  the cb-api leg these push down as LanceDB scalar SQL (`url LIKE '%…%'`,
  `array_has(tags, '…')`) and `url` + `tags` echo back on every hit so the
  result row renders "Open original" and tag chips without a second round-trip.
- Each hit carries a source badge (local vs cloud).

The federated RRF + per-backend wiring already existed for the Cloud-backup
panel; this release inverts the default — federated is now the baseline, opt
out with `--local-only`.

---

## Highlighted snippets

`index/snippet.rs` adds `highlight_snippet` — an HTML-escaped, Unicode-safe
~300-character window centred on the first query-term match, with each matched
term wrapped in `<mark>`. `FederatedHit` and `SearchResult` gain `url` + `tags`
fields; the LanceDB result builders read both (a new `list_str_col_val` reader
handles the `List<Utf8>` tags column).

In the desktop app, `IndexSearch.svelte` adds an **"Open original"** globe
button that calls `openUrl(r.url)` for any hit that carries a source URL.

---

## L1-aware local search

The central UX gap surfaced by the wallabag end-to-end verification: pulled L1
chunks skipped the extract-and-embed pipeline that populates Tantivy, so
`crispsorter index search` errored with *"FTS index not found"* on freshly
pulled rows.

`sync cloud-backup pull` now writes each pulled L1 chunk into the local Tantivy
FTS in the same pass it writes to LanceDB — delete-then-add by `doc_id` so
re-pulls don't double-index, soft-failing if the FTS dir is unwritable. On
success it prints `[sync] indexed N L1 row(s) into local Tantivy`.

Pulled rows are ingested with `chunk_index = 0, chunk_total = 1` (not the
manifest-only `-1`), because `fetch_by_doc_ids` filters out `-1` rows — they'd
survive in LanceDB but stay invisible to local search. See LEARNINGS.md.

---

## Local `--tag` filter

`crispsorter index search --tag pocket-import` now emits
`array_has(tags, 'pocket-import')` into LanceDB's scalar SQL, closing the
asymmetry where only the federated `sync cloud-backup hybrid-search` path had a
`--tag` flag.

---

## What ships in this release

Same platform matrix as v0.2.0:

- **macOS arm64 (Apple Silicon)** — full feature set.
- **Linux x86_64** — same, in `.deb` form.
- **Windows** — feature-less (offline NMT / format preservation deferred;
  tracked in PLAN.md).

---

## Upgrading

Drop-in. Settings, batch sessions, catalogs, and indexes all migrate. Existing
local indexes pick up the v106 `url` column automatically (the migration ledger
runs `all()` through v100..=v106). No user action required.
