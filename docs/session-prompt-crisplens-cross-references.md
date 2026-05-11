# Session prompt — add the two CrispLens endpoints that unblock CrispSorter P13 follow-ups

Use this verbatim as the opening prompt for a fresh session **in the
CrispLens repo** (`/Users/<user>/code/CrispLens`).  The
endpoints below were identified during the live cross-check of
CrispSorter's P13 Tier 2 work against `https://<crisplens-host>`
(documented in `../CrispSorter/docs/P13_Bilder_integration.md` →
"Open follow-ups").

Two endpoints in scope.  Both are additions, no breaking changes.
Both backends (v2 FastAPI + v4 Express) need to grow them in
lockstep.  Wire shapes are pinned by the sibling
`crisplens-protocol` Rust crate in CrispSorter — match those so the
client side lands without changes.

---

## What this session is

You're adding two GET endpoints to CrispLens:

1. **`GET /api/images/by-hash/{sha256}`** — look up an image by its
   SHA-256 file hash.  Used by CrispSorter to bridge from a local
   image row (it computes sha256 client-side) to a CrispLens
   `image_id` so the Bilder preview pane can overlay face bounding
   boxes on the displayed image.

2. **`GET /api/search/semantic?q=…&limit=…`** — embedding-based
   image search.  Used by CrispSorter's Bilder tab to promote the
   existing remote-search box from substring-only to actual semantic
   when this endpoint exists.  Recommended approach: embed the
   already-populated `ai_description` text column with a small
   sentence-transformers model and query by cosine similarity.
   Avoids adding a full CLIP image-embedding stack.

After both ship + a tagged release, the matching CrispSorter
follow-ups are one-liners (one URL swap each, no schema changes).

---

## Working environment

- **Repo**: `/Users/<user>/code/CrispLens`
  - Main branch is `main`.  Two backends share the same SQLite
    schema (`schema_complete.sql`):
    - **v2** (production today): FastAPI in `fastapi_app.py` +
      `routers/*.py`.  Run via `uvicorn`.  This is what's deployed
      at `https://<crisplens-host>`.
    - **v4**: Express in `electron-app-v4/server/routes/*.js`.
      Same schema, JS port.
  - Live production server: `https://<crisplens-host>`
    (FastAPI v2 / uvicorn — confirmed via Server response header).
  - Credentials for live testing live in
    `/Users/<user>/code/.env`
    (`CRISPLENS_REMOTE_USER=<admin-user>`, `CRISPLENS_REMOTE_PW=…`).
    Use `$CRISPLENS_REMOTE_USER` and `$CRISPLENS_REMOTE_PW` in
    scripts, never paste the literal password.

- **Schema state (you've already verified the DB has what you need)**:
  - `images.file_hash` column exists, indexed (`idx_images_file_hash`
    and `idx_images_file_hash_owner UNIQUE`).
  - `compute_sha256()` is implemented in `local_processor.py` and
    populates `file_hash` on ingest.
  - `ai_description` (LLM-generated scene description, German on
    the production instance) is populated for processed images
    via the existing extraction pipeline.
  - **NO image-level embeddings table exists yet.**  Face
    embeddings live in `face_embeddings` (512-D ArcFace) — those
    are NOT what semantic search wants.

- **CrispSorter side of the wire** (the consumer you're matching):
  - Repo: `/Users/<user>/code/CrispSorter`
  - Protocol types: `crates/crisplens-protocol/src/lib.rs`
    - `Image` is what `/api/images/by-hash/{sha}` must return.
      Match the existing `Image` struct shape (already pinned by
      multiple live-payload regression tests).
    - `SearchHit` is what `/api/search/semantic` must return.
      Same shape as the existing `/api/search` returns today,
      MINUS `recognition_confidence`, PLUS a `score: f32` (cosine
      similarity 0..1, higher = better).  See the protocol type
      doc-comment for the canonical field names.
  - **Don't change CrispSorter** — that's a separate follow-up
    session.  The protocol types you're matching are already
    final from CrispSorter's side.

## Conventions you must honour

These are persistent rules from prior sessions across this user's
projects.  Memory pointers at
`~/.claude/projects/-Users-<user>-code-CrispLens/memory/`
(create if absent).

1. **Use `python`, not `python3`.**  On this machine `python` is
   Miniconda's interpreter with the project deps; `python3` does
   not have them.  Apply to spawn-Python paths, helper scripts,
   and the `uvicorn` launcher.
2. **No emojis in any source file** unless the user explicitly
   asks.  Doc-comments included.
3. **No Denglish in identifiers.**  This is a multi-language UI
   (German user-facing strings; English code).  Endpoint names,
   Pydantic/JS field names, Rust crate identifiers stay English.
   German lands only in i18n value strings and German UI copy.
4. **Don't poll `gh` aggressively** — past sessions hit GitHub
   rate limits.  60 s+ between status polls.
5. **Don't create `.md` docs unless asked**, except: a session
   summary at the end of the work if it's earned one (per the
   precedent set by `../CrispSorter/HISTORY.md`).

## Scope of this session

**Two slices, in order.**  Stop between them and ask the user
before starting the second — they may want to ship the first
independently.

### Slice 1: `GET /api/images/by-hash/{sha256}` (~2 h)

> Returns the single image row whose `file_hash` matches the
> path-parameter sha256.  Wire shape identical to one element of
> the `GET /api/images` array response (so CrispSorter's existing
> `Image` Rust type deserialises both endpoints).

Acceptance:

- Both v2 (`routers/images.py`) and v4
  (`electron-app-v4/server/routes/images.js`) grow the route.
- Path parameter is a 64-char lowercase hex string.  Reject
  malformed input with 400 + the standard `{ "detail": "..." }`
  error envelope.
- 404 when no row matches.  Don't 500 on collisions (the unique
  index guarantees at most one row per `(file_hash, owner_id)`,
  but the route should still pick a deterministic winner — lowest
  `id` — if a future migration relaxes that).
- Honours the same auth + per-user visibility rules as
  `/api/images/{image_id}` (use `can_access_image` on v2; check
  `requireAuth` + visibility on v4).  An admin should see all
  rows; a regular user should only see their own + shared rows.
- Response shape: the same JSON the v4 `rowToApi` mapper emits
  today (verified against
  `electron-app-v4/server/routes/images.js`'s function in the
  `0..200` line range).  v2's `image_ops.browse_images_filtered`
  output is the canonical reference; this endpoint should reuse
  the same row-formatter so a future schema change touches one
  place.
- New `tests/` row for both backends — pytest for v2, jest/node
  test for v4 (whichever the repo already uses).  Verify:
    - 200 + correct row for an existing hash
    - 404 for an unknown hash
    - 400 for malformed hash (e.g. "deadbeef" — too short,
      lowercase-only)
    - 403 for an authenticated non-owner against a `private` row
- Live-test the endpoint against the local dev server, then
  commit.

After slice 1 ships, the matching CrispSorter follow-up
("image-overlay face boxes in the preview pane") becomes a
one-screen Svelte patch: hash the local file with `sha2` (already
a workspace dep), call `/api/images/by-hash/<hash>` to resolve
the `image_id`, then call the existing
`images_crisplens_image_faces(image_id)` and draw the bbox
rectangles as absolute-positioned divs on the preview image.

### Slice 2: `GET /api/search/semantic?q=…&limit=…` (~6 h)

> Real semantic search over the AI-generated `ai_description`
> texts.  Don't add a full CLIP image-embedding pipeline yet —
> use a small text-embedding model on the description column
> instead.  That keeps the model size and runtime cost low while
> producing genuinely semantic results.

Recommended approach (you may overrule if you have a better
read):

1. New table `description_embeddings`:
   ```sql
   CREATE TABLE description_embeddings (
       image_id     INTEGER PRIMARY KEY REFERENCES images(id) ON DELETE CASCADE,
       embedding    BLOB    NOT NULL,    -- f32 little-endian, N-D
       dimension    INTEGER NOT NULL,
       model_name   TEXT    NOT NULL,
       model_revision TEXT,
       computed_at  TIMESTAMP DEFAULT CURRENT_TIMESTAMP
   );
   ```
2. Embedding model: pick a small multilingual sentence-
   transformers model (the live server has German + English
   descriptions; `paraphrase-multilingual-MiniLM-L12-v2` is
   384-D and ~118 MB).  Cache locally; download once on first
   use.
3. Backfill task: a CLI subcommand or a background tick that
   walks images missing an embedding row, computes one,
   commits.  Idempotent — safe to re-run.
4. Query path:
   - POST not GET if the query is long — but the spec says GET,
     and queries should be short.  Stick with GET, document the
     URL-length cap.
   - Embed the query string with the same model.
   - Cosine similarity against every row in `description_embeddings`.
     For the production volume (~thousands of rows) a linear
     scan in numpy is fine; document the threshold (say, 50k
     rows) past which an HNSW index would be worth adding.
   - Return top-`limit` rows ordered by score desc.
5. Wire shape: same as the existing `/api/search` (filename,
   filepath, taken_at, face_count, description, tags) **plus** a
   `score: number` (0..1, higher = better).  CrispSorter's
   `SearchHit` is already permissive enough — just add the
   `score` field to the protocol type when the follow-up
   CrispSorter session lands.

Acceptance:

- Endpoint reachable on both v2 and v4.  v4 may shell out to
  a Python helper for the embedding step rather than implement
  the model in JS — that's fine, just keep startup overhead
  bounded.
- Backfill subcommand that's idempotent + reports progress.
- Tests:
    - Backfill computes embeddings for fresh rows
    - Query returns top-N ordered by descending score
    - Score is bounded 0..1
    - Empty query returns empty list (mirrors current
      `/api/search` behaviour for empty `q`)
    - Unknown / mis-spelled words still return *something*
      (semantic matching tolerates fuzz) — that's the point
- Live-test against the production data on the dev server (or
  a copy of it), commit, and tag a release.

After slice 2 ships, the matching CrispSorter follow-up
("true semantic search in the Bilder tab") is one line: swap
`/api/search` for `/api/search/semantic` in
`src-tauri/src/images/crisplens/tauri_commands.rs::images_crisplens_search`
and update the UI label.

## Files most likely to need touching

These are reads-first targets; don't pre-create anything.

### Slice 1

- `routers/images.py` — v2 route.  Pattern: mirror the existing
  `@router.get("/{image_id}")` handler (line ~195) but route on
  `file_hash` with the path-param validation prepended.
- `electron-app-v4/server/routes/images.js` — v4 route.  Mirror
  the `router.get('/:id', …)` handler at the top.  Reuse
  `rowToApi`.
- `image_ops.py` — possibly add a helper `get_image_by_hash(db_path,
  hash, owner_id, is_admin)` that the v2 route calls; keeps the
  visibility logic centralised.
- `tests/` (whatever the repo's actual test layout is — discover
  on first read).

### Slice 2

- `routers/search.py` — v2.  The existing `@router.get("")`
  (line 14) is the name-search.  Add `@router.get("/semantic")`
  next to it.
- `electron-app-v4/server/routes/search.js` — v4.  Two routes
  already (`GET /` text, `POST /face` face-similarity).  Add
  `GET /semantic`.
- `schema_complete.sql` — add the `description_embeddings`
  table definition.  Migration: existing deployments need a
  `CREATE TABLE IF NOT EXISTS` so a no-op for the running
  server until the backfill is invoked.
- New module — probably `text_embeddings.py` (v2 source of
  truth) + a CLI subcommand for the backfill.  v4 can either
  shell out to that Python or duplicate the embedding logic
  with `@xenova/transformers`.  The user's call.

## Things that will probably trip you up

1. **Existing live server data** — the production instance has
   real photos with German descriptions of the user's family,
   colleagues, etc.  Don't print descriptions verbatim in CI
   logs / commit messages.  Use sample IDs / hashes instead.
   The `.env`-driven `CRISPLENS_REMOTE_USER=<admin-user>` is the
   correct credential for live testing; the other pair
   (`CRISPLENS_LOGIN=root` / `…PW=DialogSurvey!`) does NOT
   authenticate against the production API (verified during
   the prior session — returns 401).
2. **`auth.py` is split between v2 and v4** with deliberate
   v2-compat aliases.  v2 emits the session cookie value in the
   body AND via `Set-Cookie`; v4 cookie-only.  Don't paper over
   that distinction in new endpoints — return the same shape
   both versions already use for `/api/auth/me`.
3. **`schema_complete.sql` is the canonical reference** — but
   individual migrations live in `*.py` ALTER paths in the
   ingest code.  Adding a new table needs both:
   - The CREATE in `schema_complete.sql`
   - An `ensure_*` function called on first use (mirrors
     `ensureWatchTable` in v4 + `ensure_table` in
     `routers/watchfolders.py`).
4. **Live testing requires the dev server, not the production
   instance** — schema migrations + backfill on production
   need the user's say-so.  Develop against `localhost:8000`
   (uvicorn) or `localhost:3001` (electron-app-v4 dev mode)
   first.
5. **`fastapi_app.py` mounts routers with `/api` prefix** —
   don't double-prefix in the router definitions.

## When you finish slice 1

```text
git status --short
pytest tests/ -k by_hash      # whichever test runner is in use
# Live test:
curl -X POST -H "Content-Type: application/json" \
     -d '{"username":"'"$CRISPLENS_REMOTE_USER"'","password":"'"$CRISPLENS_REMOTE_PW"'"}' \
     -c /tmp/c.txt http://localhost:8000/api/auth/login
# Pick a known hash from the dev DB:
HASH=$(sqlite3 .../images.db "SELECT file_hash FROM images WHERE file_hash IS NOT NULL LIMIT 1")
curl -b /tmp/c.txt "http://localhost:8000/api/images/by-hash/$HASH" | python -m json.tool
# Should return one Image row.

git log --oneline -3
```

Then post a status message in the format:

```
Slice 1 done — commit <sha>.  Acceptance criteria met:
  - GET /api/images/by-hash/<sha256> shipped on both v2 and v4
  - 200/404/400/403 paths covered by tests
  - Live-verified against localhost dev server with hash N
  - Wire shape matches CrispSorter's existing crisplens-protocol Image type
Ready for slice 2?  Or pivot?
```

…and **stop**.  Do not start slice 2 without explicit user approval.

## After both slices land

A coordination message back to the CrispSorter side:

```
CrispLens has shipped two new endpoints:

  GET /api/images/by-hash/{sha256}      (tagged crisplens-vX.Y.Z)
  GET /api/search/semantic?q=…&limit=…  (same tag)

Both available in v2 and v4.  Live-verified against the dev server.
Production deployment + the description-embedding backfill is a
separate operational step (not in this session's scope).

CrispSorter follow-up cleanup pickable now:
  1. Add a `score: f32` field to crisplens_protocol::SearchHit
     (the only protocol-crate change).
  2. Swap `/api/search` for `/api/search/semantic` in
     src-tauri/src/images/crisplens/tauri_commands.rs::images_crisplens_search.
     Update the UI label in src/lib/i18n.svelte.ts:
        crisplens_search_button: 'Search' → 'Semantic search'
        crisplens_search_hint:   '…NOT semantic.' → '…semantic, embedding-based.'
  3. Wire up the image-overlay face boxes in IndexIngest.svelte's
     preview pane:
        a. Hash the previewed local file with `sha2`.
        b. invoke('images_crisplens_image_by_hash', { sha }) — new
           Tauri command that calls /api/images/by-hash/{sha}.
        c. invoke('images_crisplens_image_faces', { imageId: that.id }).
        d. Render <div class="face-bbox"> overlays positioned from
           bbox.top/left/right/bottom (normalised) over the
           <img class="images-preview-image">.
```

That CrispSorter PR is ~150 lines of code + a few unit tests.

## Out-of-scope reminders (so you don't drift)

- No CLIP integration (image-level embeddings).  Description text
  embeddings only.  CLIP can be added in a future session when
  the model + storage cost is justified by the use case.
- No changes to authentication.  Cookie-based session auth
  remains; both new endpoints honour `requireAuth` / FastAPI's
  `Depends(get_current_user)`.
- No changes to the existing `/api/search` (filename / person-
  name substring).  It stays as-is.  The new
  `/api/search/semantic` is additive.
- No changes to the face_embeddings table.  Description
  embeddings live in a separate table.  Don't share an embedding
  column between fundamentally different content types.
- No emoji.  No Denglish.  No PII in commit messages.

## If the user asks for something not in this prompt

Quote the relevant section, state what's being changed and why,
ask for confirmation before writing code.  The two endpoints above
are the contract.  If the user wants a third (e.g. true CLIP
image embeddings), flag it as a scope expansion and confirm.
