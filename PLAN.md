# CrispSorter — Development Plan

> **Full specs for completed phases** → [HISTORY.md](HISTORY.md)
> **Technical patterns / pitfalls** → [LEARNINGS.md](LEARNINGS.md)
> **In-flight integration designs** → [docs/](docs/)

---

## Capabilities (shipped)

- LanceDB + Tantivy hybrid search, RRF fusion, sparse BGE-M3/SPLADE channel
- ONNX/CoreML + CrispEmbed GGUF backends, 36-model registry
- Batch AI sort (Stapel): extraction → LLM metadata → sort-path → move/copy/script
- P6 Catalog: `.caf` I/O, parallel scanner, duplicate engine, Übersicht columnar browse
- P7 Desktop search parity: folder tree, million-row pagination, preview pane, bg ingest
- P8 CLI: `version / doctor / catalog / index / batch / chat / completion / manpage`
- P9 Übersicht scale: DB-side ORDER BY (lance::Scanner), scalar indexes, volume filter
- P10 Robust ingest: TaskFailureReason, 300 s timeout, L2 fallback, DRM detection, skip-failed CLI
- P11 Remote server: `crisp-index-server` (Axum + LanceDB + Tantivy), durable job queue, server-side embedding
- P11 Cloud drives: `LocalDrive` + `InternxtDrive` + `FilenDrive` + `WebDavDrive` (live-verified against both Filen + Internxt local WebDAV servers); registry with create/edit/delete UI; `crisp+drive://` URIs; manifest-only L1 ingest + on-demand L3 promote
- P11 SyncManager: pull-apply loop closed (writes pulled rows as L1 metadata in local LanceDB)
- P12 cloud-backup: L1 manifest import (`source_files` → LanceDB), L3 via `retrieve.py`, reverse lookup, VPS-trigger indexing
- P13 Bilder vertical: image-row filtered Übersicht tab, lazy thumbnails, EXIF preview pane, SHA-256 + perceptual-hash dup grouping, **CrispLens Tier 2** connector (Keychain-stored session, 4-state health banner, People + watchfolder + by-hash + semantic-search v4 endpoints live-verified against `https://<crisplens-host>`)
- P13.5 Audio + Translation vertical: symphonia + ffmpeg decode, 24 ASR / 5 TTS / 4 MT / 4 LID backends through the `crispasr` Rust crate, `chat transcribe` + `chat tts` CLI, index-time audio/video extraction (22 file types become searchable), audio-LID routing (`BackendFallback` policy switches backend on language mismatch), text-LID at index time populates `language` LanceDB column, on-demand translation (`translate_text` Tauri command, SQLite-cached), index-time batch translation (`text_translated` + `text_translated_lang` columns added via the migration framework)
- P13.6 Multimodal UX + L1/L2/L3 audio: Stapel + Kataloge accept all 22 audio/video extensions; "Transcribing" status badge; detected source-language + duration columns in BatchReview; Settings → Multimodal panel (master switch + ASR backend + LID method + L1/L2/L3 ingest depth); audio L2 metadata in dedicated LanceDB columns via schema migration v101 (audio_duration_seconds / codec / sample_rate_hz / channels / bitrate_kbps); `index_audio_promote_l3` Tauri command + per-row "Transcribe" search-result action
- P13.7 Image L1/L2/L3 + search CLI + CrispLens push: image-side L1/L2/L3 enum + master-switch + "Re-OCR" search-result action mirroring the audio path; image L2 (EXIF) metadata in 5 dedicated LanceDB columns via schema migration v102 (camera_make / camera_model / lens_model / taken_at_unix / iso); `crispsorter index search` CLI with cloud-backup-parity filter set (--ext / --hash / --folder-prefix / --owner / --lang / --translated-to / --year-* / --min-size / --max-size / --after / --before / --audio-duration-* / --image-camera-*); CrispLens image push (`images_crisplens_image_push`) — multipart POST `/api/ingest/upload-local` with by-hash dedup precheck
- P15 Batch pre-processing: content-dedup (SHA-256), book-chapter grouping (ISBN-13)
- OCR: Tier 1 Tesseract, Tier 2 ocrs, Tier 3 PaddleOCR (`--features paddle-ocr`)
- `.cidx` offline archives: LanceDB + Tantivy FTS export/mount, Archiv tab in Übersicht, background-promote per row
- Schema-migration framework: versioned `Migration` trait with SQLite ledger at `<data_dir>/.crispsorter_migrations.db`, gap/duplicate detection, idempotent reruns; three consumers landed — `AddTextTranslatedColumns` (v100), `AddAudioMetadataColumns` (v101), `AddImageMetadataColumns` (v102)
- `crisp+cb-archive://` URI scheme for cloud-backup archived files
- `crisp+drive://` URI scheme for any registered CloudDrive (Local / Filen / Internxt / WebDAV)
- macOS arm64 packaging: `scripts/bundle_macos_native_libs.sh` co-bundles `libcrispasr.dylib` + `libcrispembed.dylib` + ggml backends + homebrew transitives into `.app/Contents/Frameworks/` with rewritten LC_RPATH entries

For per-feature deep-dives, see [HISTORY.md → "Phase ship index"](HISTORY.md).

---

## In Progress

P13.7 Cloud sync — all 8 steps shipped 2026-05-13.  Stages
E/F/G/H followed in the same session as additive infrastructure:

  - **E** — content-addressed byte upload/download
    (`POST/GET /api/files/by-hash/<sha>`).  Closes the last
    not-pure-Rust path on the client side — reqwest streams
    instead of SSH shell-out.
  - **F** — durable retry via SyncManager outbox.  bg_ingest
    enqueues `cb_manifest_push` ops; background drain ships
    them.  Survives crashes / network outages.
  - **G** — optional 256-way sharding by sha-prefix
    (`CB_API_SHARD_ROOT` env-gate).  Legacy single-DB mode is
    the default + the production VPS.
  - **H** — server-side CPU embedding inference
    (`GET /api/index/embed-query?text=…&model=…`) via
    fastembed.  Same model registry as `fastembed-rs` on the
    client → vectors interchangeable for cosine search.

All 8 live tests pass against the production VPS.

**Test coverage:** ~470 unit tests in `tauri-app` (+8
`#[ignore]`'d env-gated live tests against the cloud-backup VPS:
`cb_sync_live_{health,manifest_push_pull,outbox_drain,
byte_upload_download,full_text_push_and_search,
end_to_end_index_push_pull_search,embed_query,
embedding_push_rejects_empty}` gated by `CB_SYNC_TEST_URL` /
`CB_SYNC_TEST_API_KEY`), 20 in `crispcat`, 29 in
`crisplens-protocol`, 5 in `crisp-index-protocol`, 48 pytest
cases in `../cloud-backup/tests/`.  Run
`cargo test --workspace --lib` for the exact Rust count;
`python -m pytest tests/` inside `../cloud-backup/` for the
FastAPI suite.

---

## Open TODOs

Only `[ ]` items live here.  Shipped items are in HISTORY.md.

### P13.7 — Cloud-sync APIs + CLI search filters + image L1/L2/L3

After the 2026-05-13 P13.6 audio-vertical wins (Settings panel,
audio L1/L2/L3 enum, `index_audio_promote_l3` action, schema
migration v101) and the parallel image L2 (EXIF) plumbing in
Step 9, the user-quoted follow-ups split into four threads:

1. **Image L1/L2/L3 progression** mirroring the audio one, plus
   making OCR opt-in via Settings.
2. **CrispLens push** — wire CrispSorter's ingest pipeline to
   `POST /api/ingest/import-processed` (v2 FastAPI / v4 Express)
   so images flow into the CrispLens person + semantic index as
   they're indexed locally.
3. **Cloud-backup manifest + index sync** — extend the existing
   `source_files` import (P12) so manifest deltas + optional
   text/vector embeddings flow up to the VPS.  Today
   cloud-backup is CLI-only; the smallest viable shape is a
   thin FastAPI route in cloud-backup (`POST /manifest/push`,
   `POST /embeddings/push`) that CrispSorter posts to, plus
   `GET /manifest/pull` for the reverse direction.  Builds on
   the existing SSH/rsync L3 retrieve path.
4. **CLI search command** with the same filter set
   cloud-backup's `search.py` exposes — free-text + size range +
   date range + ext + owner + hash prefix + parent-dir prefix +
   audio duration range + image camera make/model — mapping
   onto the existing `SearchFilters` LanceDB-side scalar
   predicate builder.

Survey of existing infrastructure:

- **cloud-backup** (`../cloud-backup`): CLI-only today
  (no HTTP API).  Manifest format = SQLite `source_files` +
  `archives` + `file_manifest` tables.  Tantivy shard
  master_index holds full text only, no vector embeddings.
  Retrieve is SSH + `7z` extraction, batch-returning a temp
  file path.  CrispSorter already imports the manifest →
  LanceDB L1 (P12) and shells out to `retrieve.py` for L3.
- **CrispLens** (`../CrispLens`): v2 FastAPI + v4 Express both
  expose `POST /api/ingest/import-processed` accepting
  `{local_path, filename, thumbnail_b64, file_hash, file_size,
  exif_data, faces: [{bbox, embedding[512], age, gender}]}` —
  server runs FAISS person-matching only, no re-detection.
  Plus `POST /api/ingest/upload-local` (multipart, server
  detects + embeds).  Auth = session cookie set by
  `POST /api/auth/login`.  Hash lookup via
  `GET /api/images/by-hash/{sha256}` (already consumed by
  CrispSorter's Tier-2 connector).

#### Shipped 2026-05-13: all 8 steps — see HISTORY.md

  - Steps 1+2+3+4+6 (image L1/L2/L3 + CrispLens push + search CLI)
    landed in the morning batch.
  - Steps 5+7+8 (cloud-backup HTTP API + CrispSorter SyncManager
    bridge + Settings UI + sync CLI + 3 CLI gap-fills + mockito
    unit tests + env-gated live tests + README/HISTORY + v0.1.41
    tag) landed in the evening batch and were live-verified
    against the production VPS.

Both session logs in [HISTORY.md → 2026-05-13](HISTORY.md).
This PLAN section is now closed; new sync-protocol work would
open a fresh entry.

---

#### Step 5 — Cloud-backup HTTP API + CrispSorter SyncManager bridge — SHIPPED 2026-05-13

> This design section is retained for posterity — the live
> implementation matches it closely, with one substantive deviation:
> the FastAPI subpackage lives on the VPS alongside `vps_worker.py`
> (sharing `/root/cloudworker_state/<catalog-db>`), rather than
> on the local controller box.  Per the production deployment notes,
> this lets CrispSorter clients talk to the VPS over HTTP without
> rsync'ing the SQLite catalog through controller.py.

##### Goals

1. **Bidirectional manifest sync** — CrispSorter pushes file-
   metadata deltas to cloud-backup; cloud-backup serves the
   union of every connected client back to any other client.
   Today the existing `index_ingest_cb_manifest` Tauri command
   imports the cloud-backup SQLite `source_files` table into
   LanceDB *as a one-shot read off a copied DB file*; we want
   incremental over HTTP.
2. **Optional text + vector embedding push** — CrispSorter has
   already-computed embeddings (dense + sparse) in its LanceDB
   chunks; cloud-backup's master Tantivy index holds full text
   but no vectors today.  Pushing the embeddings makes
   cloud-backup the server-of-record for vector search across
   clients (so a phone client without a local index can hit
   the VPS).  Server-side embedding storage is a new SQLite
   table on the cloud-backup side.
3. **Manifest pull** — CrispSorter pulls deltas the way the
   existing `sync_pull` Tauri command (P11) does against
   `crisp-index-server`, but against cloud-backup's URL +
   bearer auth.  L1-only writes; L3 promote still goes
   through `retrieve.py`'s SSH path because that's the
   existing source of truth for byte content.

Non-goals — defer to a future slice:

- L3 byte transfer over HTTP (use the existing SSH/rsync path).
- Server-side embedding computation (server only stores what
  the client pushes; no GPU on the VPS today).
- Image embeddings (CrispLens already covers face embeddings;
  general image embeddings via CrispEmbed are a P13.6
  follow-up tracked separately under "Registry-driven
  embedder selection").

##### cloud-backup side — new FastAPI module

A new file `../cloud-backup/api/app.py` exposing:

  - **`POST /api/manifest/push`** — body
    `{ "rows": [{ "path": str, "size_bytes": int, "sha256": str,
                  "mtime_unix": int, "owner_id": str,
                  "filename": str, "ext": str, "parent_dir": str }],
       "cursor": Optional<str> }`
    Upserts into `source_files` (uses sha256 as the natural
    key; conflict-on-conflict = update mtime + size).  Returns
    `{ "accepted": int, "next_cursor": str }`.  Cursor is
    opaque to the client (cloud-backup uses it for
    server-side resume).

  - **`POST /api/index/push-embeddings`** — body
    `{ "rows": [{ "doc_id": str, "chunk_index": int,
                  "embedding": [f32; D],
                  "sparse_json": Optional<str>,
                  "model_id": str }],
       "cursor": Optional<str> }`
    Stores into a new SQLite table on the cloud-backup side:

        chunk_embeddings(
          doc_id TEXT NOT NULL,
          chunk_index INTEGER NOT NULL,
          model_id TEXT NOT NULL,
          embedding BLOB NOT NULL,        -- little-endian f32 array
          sparse_json TEXT,
          updated_at INTEGER NOT NULL,
          PRIMARY KEY (doc_id, chunk_index, model_id)
        )

    Schema migration runs in `../cloud-backup` (one-time, on
    server start); CrispSorter just consumes the endpoint.
    Returns `{ "accepted": int, "next_cursor": str }`.  Server
    rejects embeddings whose `len(embedding) != model_dim` (a
    `models` table records the per-model dim so the server
    knows what to validate against).

  - **`GET /api/manifest/pull?since={timestamp}&limit=200`** —
    returns `{ "rows": [SearchHit-like rows from `source_files`],
                "max_indexed_at": int }`.  Mirrors the
    existing crisp-index-server contract so CrispSorter's
    SyncManager can swap backends with just a URL change.

  - **`GET /api/index/by-embedding?vec=...&k=20`** — search
    by vector (only meaningful when embeddings have been
    pushed).  Server runs a brute-force k-NN against the
    `chunk_embeddings` table for now (LanceDB on the VPS is
    a P13.8 follow-up — Postgres + pgvector also viable, but
    LanceDB matches the client side).  Body returns
    `{ "rows": [{doc_id, chunk_index, distance, model_id}] }`.

Auth: bearer token via `Authorization: Bearer {api_key}`
header.  API keys managed server-side via a new
`api_keys(id, name, hash, created_at)` table — admin creates
keys via a CLI flag on cloud-backup startup, hands them out
to clients, and revokes by `--revoke-key NAME`.

Deployment: cloud-backup gains a `uvicorn api.app:app` line
in the existing systemd unit (or a separate unit if the
admin wants to keep the HTTP service on a different port
from the SSH ingest path).

##### CrispSorter side — SyncManager `cloud_backup` mode

The P11 `SyncManager` currently targets `crisp-index-server`.
Generalise:

  - `SyncTarget { CrispIndexServer, CloudBackup }` enum.
  - Each target gets its own set of `push_*` / `pull_*` methods
    against its URL schema.  `CloudBackup::manifest_push` walks
    the LanceDB `documents` table for rows newer than the
    `last_manifest_push_ts` watermark in the outbox SQLite,
    batches into 200-row chunks, posts each, advances the
    watermark on the response cursor.
  - `CloudBackup::embeddings_push` is gated by a new
    `IndexConfig.cloud_backup_push_embeddings_enabled` flag
    (default false — costly bandwidth; user opts in).  Same
    batching pattern.
  - `CloudBackup::manifest_pull` writes the returned rows as
    L1 metadata in the local LanceDB (chunk_index = -1 sentinel,
    matching the existing P11 `sync_pull` shape; promote-to-L3
    still goes via `retrieve.py`).

Settings UI:

  - New "Cloud-backup sync" sub-panel beneath the existing
    "Search Index" panel.  Fields: URL (read-only display
    once the user has set it via CLI / env var; CrispLens
    pattern), API key (write-only — value stored in OS
    keychain, never in `index_config.json`), three
    checkboxes: "Push manifests", "Push embeddings", "Pull
    manifests".

CLI:

  - `crispsorter sync cloud-backup push-manifest [--limit N]`
  - `crispsorter sync cloud-backup push-embeddings [--limit N]`
  - `crispsorter sync cloud-backup pull [--limit N]`
  - `crispsorter sync cloud-backup status` — outbox depth +
    last push/pull timestamps.

##### Open design questions

- **Embedding dimensionality drift** — when the client
  re-ingests with a different model, the server must accept
  both (column key is `(doc_id, chunk_index, model_id)`) but
  the by-embedding query has to know which model the caller
  wants to search against.  Probably surface as a query
  parameter `?model=bge-m3` with a server-side default to
  the most-pushed model.
- **Owner scoping** — does the server enforce that only the
  pushing client can see its own pushed rows, or is the
  manifest a shared union across clients?  Default to
  per-owner scoping (the existing `owner_id` column is the
  natural fence) with an admin override for shared catalogs.
- **Rate limits** — embedding pushes can be sizable (1024d ×
  f32 = 4 KB per chunk × 100k chunks = 400 MB).  Default
  batch size 200 rows = 800 KB per POST; add an explicit
  rate-limit at the server's reverse proxy.

##### Implementation order

1. **cloud-backup PR (Python)** — new `api/app.py`, schema
   migrations for `chunk_embeddings` + `api_keys`, systemd
   unit update.  Land first so CrispSorter has a real
   server to test against.
2. **SyncManager refactor (Rust)** — split the existing
   `SyncManager` into `SyncTarget`-keyed implementations.
   Mock-server tests via the existing `mockito` pattern.
3. **Settings UI + CLI** (Svelte + Rust).
4. **Live tests** — env-gated against a real cloud-backup
   instance (`CB_SYNC_TEST_URL` / `CB_SYNC_TEST_API_KEY`).
   `#[ignore]` by default; CI doesn't run them.
5. **README + HISTORY update + tag**.

Estimated effort: **4-6 h cloud-backup side + 4-6 h
CrispSorter side**, spread across one to two sessions.
This is the sole gate on the v0.1.41 (or v0.2.0) tag.

#### Step 7 — Tests for the sync routes — SHIPPED 2026-05-13

  - [x] Mock-server unit tests via `mockito` (new dev-dep):
    17 cases in `src-tauri/src/sync/cloud_backup.rs::tests`
    cover the 200/cursor/no-cursor/400/401/500/503 matrix
    across every push/pull/query route + the builder.
  - [x] Env-gated live tests in
    `src-tauri/src/sync/cloud_backup.rs::live_tests`:
    `cb_sync_live_{health_round_trip, manifest_push_pull_round_trip,
     embedding_push_rejects_empty}`.  `#[ignore]`'d by default;
    run with `CB_SYNC_TEST_URL` + `CB_SYNC_TEST_API_KEY` set.
    All 3 green against the live VPS during the closeout.
  - [x] Cloud-backup pytest suite (21 cases) covers the FastAPI
    surface end-to-end: auth, manifest round-trip, pagination,
    owner-scoping, embeddings push/query.

#### Step 8 — Docs + tagging — SHIPPED 2026-05-13

  - [x] README capabilities table appended with the new
    cloud-backup HTTP sync line.
  - [x] HISTORY.md session log entry for Step 5+7+8 lands
    above the morning-batch entry.
  - [x] v0.1.41 tag cut (minor: the cloud-backup HTTP protocol
    is additive, not a breaking shift — see the architectural-
    decisions summary in the HISTORY entry).

### P13.7.x — Cloud-sync follow-ups (post Stages E–N)

Stages J / K / L / M / N landed in the 2026-05-14 batch — see HISTORY.md for the session log.  What remains, ordered by the user's priority sweep on 2026-05-14:

#### Stage O — Small UX completeness — SHIPPED 2026-05-14

- [x] **"Sync now" GUI button** in the Cloud-backup Settings panel that hits `sync_cb_drain` + `sync_cb_manifest_pull` in sequence.  Today's drain is auto every 30 s (Stage J) OR manual via CLI — no GUI button.
- [x] **`--include-full-text` flag** on `crispsorter sync cloud-backup pull` so headless flows can opt into body sync without flipping the Settings checkbox.  XS effort.
- [x] **`sync_status_all` Tauri command** — polls all three backends (crisp-index-server / CrispLens / cb-api) in parallel via `tokio::join!`, returns combined JSON with per-backend reachability + auth-state + last-sync-ts.

#### Stage P — Local DB size cap + LRU pruning (~3–4 h)

Today the local LanceDB grows unbounded.  At terabyte-scale corpora the user wants a hard cap: keep recent rows in full (metadata + body + embedding), older rows trimmed to metadata-only, oldest rows evicted entirely.

- [x] **`IndexConfig.local_max_size_bytes`** — new field (default `None` = unbounded).  Settings UI gets a slider 0–1000 GB (0 = unlimited).  SHIPPED 2026-05-15
- [x] **`crispsorter index purge --max-size N`** CLI — walks LanceDB by `indexed_at` asc (oldest first), drops `full_text`/`full_text_md`/`embedding`/`embedding_sparse` cols first, then evicts rows entirely until on-disk ≤ N.  Supports SI suffixes (K/M/G/T).  SHIPPED 2026-05-15
- [x] **Background purge worker** — 1-hour tokio interval; no-op when cap unset or index already within bounds.  SHIPPED 2026-05-15
- [ ] **Skeleton index preservation** (extreme case for Stage W) — deferred to Stage W.
- [x] **Rust unit tests**: `purge_noop_when_within_cap` + `purge_strips_heavy_columns_and_evicts`.  SHIPPED 2026-05-15

#### Stage Q — Backup shards to cloud drives (~2–3 h) — SHIPPED 2026-05-15

VPS shards live on the Hetzner storage box.  Need offsite mirror via the existing CloudDrive abstraction (Filen / Internxt / WebDAV).

- [x] **`crispsorter sync cloud-backup backup-shards --drive <id> [--shard <prefix>]`** — exports shard tarballs from `/api/shard/export/{prefix}`, uploads to drive at `cb-backups/<date>/<prefix>.tar.gz`.  SHIPPED 2026-05-15
- [x] **Per-shard incremental backup** — only re-uploads shards whose `max_indexed_at` watermark advanced since last backup.  Tracked in new `backup_state.db` SQLite (`shard_backups` table).  SHIPPED 2026-05-15
- [x] **`crispsorter sync cloud-backup restore-shard <prefix> --from-drive <id>`** — downloads tarball from drive, imports via `/api/shard/import/{prefix}`.  SHIPPED 2026-05-15
- [x] **Retention (`--keep-daily N`)** — deletes older daily dirs from the drive, keeps last N.  SHIPPED 2026-05-15
- [x] **GUI surface** — "Cloud drive backup" panel in Settings → Cloud-backup: drive-id input, keep-daily counter, "Backup now" button.  Load/save wired to `IndexConfig`.  SHIPPED 2026-05-15
- [x] **VPS API** — `GET /api/shard/list`, `GET /api/shard/export/{prefix}`, `POST /api/shard/import/{prefix}` added to `../cloud-backup/api/app.py`.  SHIPPED 2026-05-15
- [x] **`sync_cb_backup_shards` Tauri command** — mirrors CLI handler via AppState; registered in invoke_handler.  SHIPPED 2026-05-15
- [x] **`BackupState` unit test** — `round_trip_backup_record` in `backup_state.rs`.  SHIPPED 2026-05-15
- [ ] **Live test**: backup to a tempfile WebDAV server, verify integrity via sha256 of unpacked shard.  (deferred — requires live drive)

#### Stage R — Manifests-DB import bridge (~2 h) — SHIPPED 2026-05-15

controller.py owns `index_manifest.db` (the legacy SQLite that aggregates every host's manifest via SYNC_MANIFESTS).  Today cb-api reads from `<catalog-db>` directly; controller.py's SQLite isn't ingested over HTTP.  Close the loop so a one-shot import populates cb-api from a controller-box.

- [x] **`crispsorter sync cloud-backup import-from-manifest-db PATH`** — reads the source `source_files` / `file_manifest` tables, POSTs every row through `/api/manifest/push` in 200-row batches.  SHIPPED 2026-05-15
- [x] **Server endpoint optionally accepts already-archived rows** — `ManifestRow.archived_in: Optional<batch_id>` so the controller.py state ("this file is in 7z archive #42") survives the round-trip.  SHIPPED 2026-05-15
- [x] **Resumable** — keeps a watermark in `manifest_import_state.db` so re-runs skip already-imported rows.  SHIPPED 2026-05-15
- [x] **GUI**: a one-shot import button in Settings → Cloud-backup → "Import from controller.py manifest".  SHIPPED 2026-05-15
- [x] **Pytest**: synthetic SQLite with 100 source_files rows → import → verify identical rows visible via `/api/manifest/pull`.  SHIPPED 2026-05-15

#### Stage S — Federated search across all backends (~5–6 h) — SHIPPED 2026-05-15

Today the user picks one backend for search.  A unified "search everywhere" panel queries local + cb-api + CrispLens in parallel, RRF-merges results, shows source-of-truth badges per hit.

- [x] **`sync_federated_search(query, filters)`** Tauri command that fans out across all three backends via `tokio::join!`, normalises payloads to a shared `FederatedHit` shape, RRF-merges by per-backend rank, returns the union.  SHIPPED 2026-05-15
- [x] **GUI panel** in IndexSearch.svelte: "🔀 Alle" button + backend filter checkboxes (local / cloud_backup / crisplens) defaulting to all-on.  Result rows badge their source backend with icon + rrf_rank.  SHIPPED 2026-05-15
- [x] **CLI**: `crispsorter sync cloud-backup federated-search "query" [--backends local,cloud_backup,crisplens]`.  SHIPPED 2026-05-15
- [x] **Tests**: `rrf_merge_deduplicates_and_ranks` + `rrf_merge_respects_limit` + `rrf_merge_empty_lists` unit tests in `tauri_commands.rs`.  SHIPPED 2026-05-15

#### Stage T — cb-api key minting from the GUI (~2–3 h)

Today key mint requires SSH'ing to the VPS and running `python -m api.admin mint`.  Settings should expose this for ops convenience — but with a hard security boundary.

- [x] **Server-side admin token** — distinct from regular bearer tokens.  Minted once on cb-api install via `python -m api.admin mint-admin`; stored in `/etc/cb-api.env` as `CB_API_ADMIN_TOKEN`.  **SHIPPED 2026-05-15**
- [x] **`POST /api/admin/keys/mint`** + `revoke` + `list` routes, all gated on the admin token.  **SHIPPED 2026-05-15**
- [x] **Settings UI**: collapsible "Admin — API key management" sub-section in Cloud-backup Settings; user pastes admin token; can mint / revoke / list regular keys.  **SHIPPED 2026-05-15**
- [x] **CLI**: `crispsorter sync cloud-backup admin mint <NAME>` + `revoke` + `list --json`.  **SHIPPED 2026-05-15**

#### Stage U — L1-only local + zip-batch handoff to VPS (~8–10 h, the user's "thin client" mode)

The bigger architectural shift: when the local catalog is huge, the user doesn't want CrispSorter to do extraction locally at all.  Instead:

1. Local walks the filesystem at L1 only (paths + sizes + mtime + sha256).
2. Local zips files in batches (size or count threshold) and ships them to the VPS via the existing rsync/SCP/SCP-fallback machinery (`controller.py`-style).
3. VPS-side **vps_worker** unzips + runs extraction for every supported type — text, audio, images.
4. VPS pushes the resulting manifests + body + embeddings into `<catalog-db>` + Lance shards.
5. VPS also forwards the encrypted-or-plain blob to Internxt for cold storage (same as today's vps_worker).
6. CrispSorter client never holds full extraction state; it sees the corpus only through `/api/v2/index/search`.

- [ ] **`crispsorter index l1-only`** CLI mode — runs scan + zip + upload without local extraction.
- [ ] **`IndexConfig.local_extraction_enabled`** master switch (default `true`).  When `false`, bg_ingest writes L1 rows only.
- [ ] **vps_worker extension** — currently it just decrypts + uploads.  Add per-extension extraction:
    - Text: PyMuPDF / pypdf / python-docx (already in requirements.txt for `search_engine.py`).
    - Audio: CrispASR via the `crispasr-cli` Rust binary OR a Python wrapper (faster-whisper).
    - Images: forward to **CrispLens** (already on the VPS) via its `/api/ingest/upload-local` route.
- [ ] **Job state** — extend `worker_state.db` with a `pending_extractions` table; vps_worker processes one extraction at a time off the queue.
- [ ] **Backpressure / progress** — controller-style status reports back over `/api/v2/extract/status`.
- [ ] **Live tests**: ship a small zipped batch end-to-end; verify the rows show up in `/api/v2/index/search` with the expected `full_text` + `embedding`.

#### Stage V — vps_worker leverages CrispLens + CrispASR for full-spectrum extraction (~6–8 h, child of Stage U)

Self-contained because the cross-service plumbing has its own surface area:

- [ ] **vps_worker → CrispLens bridge**: image files routed via a new internal `crisplens_image_push()` helper.  CrispLens already lives on the VPS; vps_worker hits its loopback URL.  Captures face count + ArcFace embedding back into `<catalog-db>.file_references`.
- [ ] **vps_worker → CrispASR bridge**: audio/video files transcribed via a `crispasr` CLI subprocess.  The Rust binary already exists in CrispSorter's tree; build a slim VPS-only variant + ship it to `/opt/cb-api/bin/crispasr`.  Output goes into `full_text`.  Decoded via symphonia → 16 kHz mono → whisper-base ggml weights cached on the storage box.
- [ ] **vps_worker → text extractors**: already imports pypdf / python-docx via `search_engine.py`.  Wire a slim `extract_text(path)` helper, reuse `ContentExtractor`.
- [ ] **Job dispatching**: a single `vps_worker.py:extract_one(job)` switch on extension that picks the right extractor.

#### Stage W — Skeleton local index + remote-only search fallback (~5–7 h, child of Stage U + P)

The extreme tiered-cache mode: when the user has TB on the VPS but wants their laptop to use ~100 MB, keep ONLY:

- A bounded `author_index` SQLite KV: `{author_name → (doc_count, last_seen_at)}`.  Thousands of rows, megabytes.
- A bounded `parent_dir_index` SQLite KV: `{parent_dir → (doc_count, last_seen_at)}`.  Same.
- The SyncManager outbox.

Everything else lives on the VPS.  Search flow:

1. User types "kant".
2. Skeleton local index recognises "kant" as a known author → shows count locally (instant).
3. CrispSorter fetches `GET /api/v2/index/search?q=kant&limit=50` → renders the hits.
4. Selected rows can be cached in local LanceDB for re-use (LRU cap from Stage P).

- [ ] **`IndexConfig.local_skeleton_only`** boolean.  When true, bg_ingest writes ONLY the skeleton indices (not LanceDB rows).
- [ ] **`SkeletonIndex` SQLite** at `<data-dir>/skeleton_index.db` with the two KV tables.  Populated at bg_ingest write time + at every `/api/v2/index/search` hit (so frequently-queried rows accumulate in the skeleton too).
- [ ] **GUI**: search panel shows skeleton hits first (instant ✦ badge), then merges in cb-api hits.
- [ ] **Pytest + Rust unit tests** for the skeleton-only mode.

---

**Out of scope for this batch** (tracked but deferred):

- **LanceDB IVF-PQ vector index** on the VPS — for shards crossing ~100k embeddings.  Today's brute-force k-NN holds at 10k–50k chunks per shard.
- **FTS5 ↔ Tantivy convergence** — two parallel engines (cb-api FTS5 over client-pushed bodies + `search_engine.py` Tantivy over the 7z-extract flow).  Decision deferred until search-perf profile demands the change.
- **`shard_rebalance` admin tool** — for migrating between sharding keys after a Stage K config change.  Atomic per-row move; resumable.

### P3.5 — CrispEmbed / CrispASR bundling

- [x] Phase 1 — macOS arm64 (see HISTORY.md)
- [ ] **Phase 2 — Linux + Windows** (~8-12 h, separate session)
      RPATH / DLL colocation; each platform needs 1-2 release iterations.
      Opening prompt: `handover-prompts/session-prompt-crispembed-ci-matrix.md`
      (local-only — see .gitignore).
- [ ] **Phase 3 — mobile** (deferred)

### P5 — Future / planned

- [ ] **Auto-process toggle on watch detection** — risky, needs UX
      design pass before any code
- [ ] **PWA demo via File System Access API** — speculative

### P7.8 — OCR Tier 3 polish + Tier 4

- [ ] **SLANet table extraction** on top of Tier 3 PaddleOCR — adds
      structured table output for invoices / bank statements / grids.
      The `usls` crate already hosts a SLANet model.  ~3-5 h.
- [ ] **Tier 4 — VLM OCR** (~1 wk) — `deepseek-ocr.rs`-style via
      Candle (not ort).  DeepSeek-OCR / PaddleOCR-VL, Q4_K–Q8_0
      quantisation, 4.7-9 GB models, macOS Metal target.

### P8.2 — CLI polish remaining

- [ ] **`cargo install crispsorter`** for the Tauri-app binary — needs
      binstall recipe + signing (macOS Developer ID, Windows
      Authenticode).  `cargo install --path crates/crispcat-cli` already
      ships.  ~2-4 h once a signing identity is in hand.

### CrispEmbed — leverage unused capabilities (survey 2026-05-13)

CrispEmbed (sibling repo, v0.3.2 as of 2026-05-13) exposes several
features CrispSorter doesn't yet consume.  The on-disk model
collection has gained reranker entries (bge-reranker-v2-m3,
jina-reranker, gte-base/large-en-v1.5).

**Already wired (this session)**:
- CrispEmbed sparse encoding for GGUF backend (5e0eab1) — closes
  the gap where GGUF users lost the RRF sparse channel.
- Embedder-as-bi-encoder reranker (6bfedbe) — re-scores top-N
  hybrid candidates by cosine similarity against the query, using
  the already-loaded dense embedder.  Activates when
  `IndexConfig.use_embedder_as_reranker = true` and no dedicated
  cross-encoder is configured.  Settings UI checkbox lands in
  the same commit.
- `index/reranker.rs` routed through `CrispEmbedBackend`
  (ebd511f) — no more direct `crispembed::CrispEmbed::new`
  import outside `index::embedder`; opens the door to future
  shared knobs (Matryoshka / prefix / cache_dir).
- `crispembed::list_models()` registry helper surfaced via the
  `embedder_registry_list` Tauri command + a disclosure panel in
  Settings (b0ebc23).  Informational only for now: selecting a
  non-`EmbedderModel`-enum entry still needs the String-keyed
  selection refactor below.

**Still unused**:

- [ ] **ColBERT multi-vector retrieval** (`encode_multivec`)
      (~1 session) — per-token L2-normalised embeddings (BGE-M3
      ColBERT head).  Needs a new LanceDB column for the
      per-token vectors (FixedSizeList of variable length is
      awkward; might need a separate `chunk_multivec` table joined
      by `id`) + a late-interaction MaxSim scorer in the search
      pipeline.
- [ ] **Omnimodal cross-modal search** (`encode_audio` /
      `encode_image`) (~2 sessions) — BidirLM-Omni encodes text,
      audio, and images into a shared 2048-d space.  Unlocks:
      type "photo of a sunset" → image hits without OCR; type
      "podcast about Bosnia" → audio file hits without
      transcription required.  Needs a new model class
      (BidirLM-Omni isn't in the existing `EmbedderModel` enum), an
      image-patch preprocessing pipeline (pixel patches +
      grid_thw), and a decision about how the 2048-d cross-modal
      vector coexists with the existing per-backend dense column
      (separate column? per-index dim selection at init?).
- [ ] **Registry-driven embedder selection** — the
      `embedder_registry_list` Tauri command surfaces the full
      CrispEmbed registry, but the dropdown still keys off the
      `EmbedderModel` enum.  Wiring a parallel String-keyed
      selection path (or refactoring `EmbedderModel` to String)
      would let new upstream registry models be picked without a
      CrispSorter release.  ~1 session.

### P13.5 follow-ups (remaining after the 2026-05-13 batch)

Ten P13.5 follow-ups shipped on 2026-05-13 (see HISTORY.md):
`--stream` flag, LID/MT model auto-resolution,
`SearchFilters::prefer_translated_lang` + snippet swap,
`IndexConfig.translate_to` persistence + Settings UI, frontend
`translate_text` integration in the search-results panel, SRT /
VTT output formats for `chat transcribe` (`63ec866`),
Audio-LID auto-resolution for whisper-family backends
(`2b80345`), `index/reranker.rs` routed through
CrispEmbedBackend (`ebd511f`), `crispembed::list_models()`
registry helper + Settings disclosure (`b0ebc23`), and the
FTS-over-translated-body Tantivy schema slice (`be73321`).

Still open:

- [ ] **Per-language reranker selection** — `language` LanceDB
      column is populated (Phase 7); routing the reranker model
      by it is the next slice.  Likely shape: `IndexConfig` gets a
      `Map<Language, RerankerModel>` (per-language pick) or a
      simpler "use multilingual reranker when language differs
      from the embedder's primary" toggle.
- [ ] **Per-chunk vs per-doc translation storage** — today we
      replicate the full translation across every chunk row of a
      doc, matching the existing `full_text_md` convention.  For
      very long docs (100 KB translation × 100 chunks = 10 MB
      replicated) this is wasteful.  Alternative: store only on
      `chunk_index = 0` and JOIN at search time — needs a careful
      migration on shipped data + decisions around the
      `record_batches_to_search_results` snippet path.
- [ ] **FTS body_translated migration on legacy indexes** —
      `be73321` adds the field for fresh indexes and gracefully
      degrades for legacy ones (`IndexFields.body_translated =
      None`).  A proper "rebuild Tantivy from LanceDB to upgrade
      the schema" migration is needed for users with shipped
      indexes to get the FTS-over-translated-body benefit
      without re-ingesting from disk.  Should go through the
      migration framework with a fresh version > v100.
- [ ] **Non-whisper audio-LID auto-resolution** — `2b80345`
      handles the whisper-method case by registry-resolving
      `whisper`.  Silero / Ecapa / Firered still require explicit
      `--lid-model` paths because they aren't in CrispASR's
      registry.  Add upstream registry entries
      (`lid-silero`, `lid-ecapa`, `lid-firered`) to close this.

---

(For per-version changelog and shipped phase specs, see [HISTORY.md](HISTORY.md).)
