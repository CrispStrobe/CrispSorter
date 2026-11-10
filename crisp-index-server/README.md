# crisp-index-server

Self-hosted vector + full-text search backend for [CrispSorter](https://github.com/CrispStrobe/CrispSorter).

CrispSorter embeds documents locally using fastembed (BGE-M3 or similar) and sends the pre-computed dense vectors to this server. The server stores them in **LanceDB** (ANN) and **Tantivy** (BM25 full-text), then handles hybrid search with Reciprocal Rank Fusion (RRF, k=60).

Optionally, the server can also embed missing vectors itself. Set `SERVER_EMBED=1` and the server loads a CPU-side BGE-M3 fastembed model; queued `/v1/ingest/batch` tasks with empty `embedding` arrays are hydrated on arrival before they are written, and `/v1/search` can derive the query embedding server-side for `vector` / `hybrid` requests when the client sends `query` but omits `embedding`.

---

## Architecture overview

| Stage | Responsibility |
|---|---|
| Client extraction | Extract source text and metadata, retain chunk byte offsets, and embed chunks |
| Durable upload | Submit complete documents through `/v1/ingest/batch` and poll their task receipts |
| Storage publication | Atomically replace Lance rows and publish the repaired Tantivy generation |
| Query negotiation | Check `/v1/capabilities`, validate the shared grammar, and embed semantic queries |
| Retrieval | Apply metadata and explicit lexical constraints, fuse ranked lists, and return document results |

---

## Requirements

- Current stable Rust (`rustup update stable`) and a C/C++ build toolchain
- Protocol Buffers compiler (`protoc`) on `PATH`, or an explicit `PROTOC` path before running Cargo
- Linux x86_64 or macOS (ARM or x86)
- ~200 MB disk for the binary; data dir grows with indexed documents

---

## Build

```bash
# Debug build (fast compilation, slower runtime)
cargo build --locked -p crisp-index-server

# Release build (optimised, recommended for production)
cargo build --locked -p crisp-index-server --release

# Static binary for musl targets (for minimal Docker images / Alpine)
rustup target add x86_64-unknown-linux-musl
cargo build --locked -p crisp-index-server --release --target x86_64-unknown-linux-musl
```

The release binary is at `target/release/crisp-index-server`.

---

## Configuration

All configuration is via environment variables. Create a `.env` file in the working directory for development (loaded automatically via `dotenvy`).

| Variable | Default | Description |
|---|---|---|
| `PORT` | `8473` | TCP port to listen on |
| `CRISP_API_KEY` | *(empty)* | Bearer token for authenticated endpoints. If empty, any token is accepted — **set this in production** |
| `LANCE_DATA_DIR` | `./data` | Directory containing `lance/`, versioned `fts-v2/`, and the durable queue; the legacy `fts/` directory is preserved |
| `EMBED_DIMS` | `1024` | Embedding dimension — **must match the embedder used by the CrispSorter client** |
| `RUST_LOG` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`) |
| `SERVER_EMBED` | `0` | When `1` / `true`, load a server-side BGE-M3 embedder and allow ingest chunks with empty `embedding` arrays, plus query-side embedding for `vector` / `hybrid` search |
| `QUEUE_LEASE_SECS` | `30` | Lease timeout for queued batch tasks before stale `processing` rows are reclaimed |
| `QUEUE_RETRY_BASE_MS` | `1000` | Base retry backoff in milliseconds; later retries use exponential backoff from this base |
| `QUEUE_MAX_ATTEMPTS` | `3` | Maximum attempts per queued batch task, including the first try |

**Example `.env`:**
```env
PORT=8473
CRISP_API_KEY=your-secret-key-here
LANCE_DATA_DIR=/var/lib/crisp-index/data
EMBED_DIMS=1024
SERVER_EMBED=1
QUEUE_LEASE_SECS=30
QUEUE_RETRY_BASE_MS=1000
QUEUE_MAX_ATTEMPTS=3
RUST_LOG=info
```

### Embedding dimension reference

| CrispSorter embedder | `EMBED_DIMS` |
|---|---|
| BGE-M3 (default) | `1024` |
| E5-Large | `1024` |
| E5-Base | `768` |
| MiniLM-L6 | `384` |
| BGE-Small-EN | `384` |

---

## Running

```bash
# Development
CRISP_API_KEY=dev-key cargo run --locked -p crisp-index-server

# Production (release binary)
CRISP_API_KEY=prod-key LANCE_DATA_DIR=/data ./crisp-index-server
```

---

## API reference

All endpoints except `/health` require a `Authorization: Bearer <CRISP_API_KEY>` header.

The compact vector examples below assume `EMBED_DIMS=2` for illustration. Supply
the complete vector for the configured production model, or use an empty vector
on ingest when server embedding is enabled.

### `GET /health`

Liveness probe — no authentication required.

**Response 200:**
```json
{ "status": "ok" }
```

---

### `GET /v1/stats`

Returns index statistics.

**Response 200:**
```json
{
  "row_count": 142503,
  "doc_count": 1201,
  "embed_dims": 1024
}
```

- `row_count` — total number of chunks in LanceDB
- `doc_count` — live full-text document count, scoped by owner and document ID
- `embed_dims` — configured embedding dimension

---

### `POST /v1/ingest`

Store a single pre-embedded chunk.

**Request body:**
```json
{
  "doc_id":       "report-2024",
  "chunk_index":  0,
  "chunk_total":  1,
  "chunk_start_char": 0,
  "chunk_end_char": 23,
  "full_text":    "plain text of the chunk",
  "full_text_md": "## Optional Markdown\n\nmarkdown version of the chunk",
  "headings":     ["Section title", "Subsection"],
  "embedding":    [0.012, -0.034],
  "title":        "Document Title",
  "author":       "Jane Smith",
  "year":         2024,
  "filename":     "report.pdf",
  "ext":          "pdf",
  "language":     "en",
  "location_uri": "crisp+local://user-uuid@machine-uuid/path/to/report.pdf",
  "owner_id":     "user-uuid",
  "source_hash":  "sha256-of-file-content",
  "tags":         ["finance", "Q4"]
}
```

- `embedding` — pre-computed dense vector; length must equal `EMBED_DIMS`
- if `SERVER_EMBED=1`, you may instead send `embedding: []` and the server will compute the vector with BGE-M3 before writing
- `full_text_md` and `headings` are optional
- Every chunk contributes to one full-text document per owner and document identifier; headings and titles are indexed separately
- The endpoint is **idempotent**: retrying the same owner, document identifier, and chunk index replaces that chunk
- Include `chunk_total`, `chunk_start_char`, and `chunk_end_char` when known. Despite their historical names, start/end are UTF-8 **byte** offsets into the original text
- A batch containing every chunk `0..chunk_total-1` replaces the complete document and removes stale trailing chunks. Partial requests retain untouched siblings

**Response 200:**
```json
{ "chunk_count": 1, "write_time_ms": 12 }
```

**Response 422** — embedding dimension mismatch:
```json
{ "error": "embedding length 384 != expected 1024" }
```

---

### `POST /v1/ingest/batch`

Queue a batch ingest task and return immediately.

**Request body:**
```json
{
  "chunks": [
    {
      "doc_id": "report-2024",
      "chunk_index": 0,
      "chunk_total": 1,
      "chunk_start_char": 0,
      "chunk_end_char": 10,
      "full_text": "plain text",
      "embedding": [0.012, -0.034],
      "filename": "report.pdf",
      "ext": "pdf",
      "language": "en",
      "location_uri": "crisp+local://user-uuid@machine-uuid/path/to/report.pdf",
      "owner_id": "user-uuid",
      "source_hash": "sha256-of-file-content",
      "tags": []
    }
  ]
}
```

- when `SERVER_EMBED=0`, every chunk must carry a full embedding vector
- when `SERVER_EMBED=1`, chunks may carry `embedding: []` and the server fills them during queue processing
- the server persists the task in `crisp_jobs.db` and processes it asynchronously through a single background worker
- queue rows carry lease / heartbeat / retry metadata so stale `processing` tasks can be reclaimed after a crash or restart
- legacy `ingest_tasks.json` state is imported into `crisp_jobs.db` automatically on first startup after upgrade

**Response 202:**
```json
{
  "task_id": "6d9b0d31-8f4a-4e52-b12a-7d8d14a4b8e6",
  "queue_depth": 3,
  "chunk_count": 1
}
```

### `GET /v1/tasks/:task_id`

Poll the current state of one queued batch.

**Response 200:**
```json
{
  "task_id": "6d9b0d31-8f4a-4e52-b12a-7d8d14a4b8e6",
  "state": "processing",
  "total_chunks": 64,
  "completed_chunks": 0,
  "queue_depth": 2,
  "attempt_count": 1,
  "max_attempts": 3,
  "last_heartbeat_at": 1746547800123,
  "lease_expires_at": 1746547830123
}
```

`state` is one of: `queued`, `processing`, `done`, `failed`.
The lease / heartbeat / attempt fields are optional metadata for operators and
may be absent on older servers or terminal rows.

---

### `POST /v1/search`

Search for documents. Supports text-only (BM25), vector-only (ANN), or hybrid (RRF).

**Request body:**
```json
{
  "query":     "machine learning transformers",
  "embedding": [0.012, -0.034],
  "mode":      "hybrid",
  "query_language_version": 1,
  "limit":     20,
  "owner_id":  "user-uuid",
  "language":  "en",
  "year_min":  2020,
  "year_max":  2024
}
```

| Field | Required | Description |
|---|---|---|
| `mode` | no | `"text"`, `"vector"`, or `"hybrid"` (default) |
| `query` | for `text` / `hybrid` | BM25 query string |
| `embedding` | for `vector` / `hybrid` unless `SERVER_EMBED=1` | pre-computed query vector |
| `limit` | no | max results, 1–200 (default: 20) |
| `query_language_version` | after negotiation | grammar version confirmed by `/v1/capabilities` |
| `owner_id` | no | filter to a specific user's documents |
| `language` | no | filter by language code (`"en"`, `"de"`, …) |
| `year_min` / `year_max` | no | filter by publication year |

**Response 200:**
```json
[
  {
    "doc_id":       "report-2024",
    "owner_id":     "user-uuid",
    "chunk_index":  0,
    "score":        0.0164,
    "title":        "Document Title",
    "author":       "Jane Smith",
    "year":         2024,
    "filename":     "report.pdf",
    "ext":          "pdf",
    "language":     "en",
    "location_uri": "crisp+local://user-uuid@machine-uuid/path/to/report.pdf",
    "snippet":      "plain text of the matching passage",
    "indexed_at":   1746547800123,
    "source_hash":  "sha256-of-file-content"
  }
]
```

**Hybrid search (RRF):**
Merges BM25 and ANN ranked lists using Reciprocal Rank Fusion with k=60:
`score = 1/(k + rank_bm25) + 1/(k + rank_ann)`

When `SERVER_EMBED=1`, the client may omit `embedding` for `vector` and `hybrid` requests as long as it still supplies `query`; the server embeds the query text with BGE-M3 before running ANN / RRF.

**Response 422** — missing required fields for mode:
```json
{ "error": "hybrid mode requires both query and embedding" }
```

---

### `DELETE /v1/docs/:doc_id?owner_id=...`

Delete all chunks for one owner/document from both LanceDB and Tantivy. The owner
is optional only when the document ID identifies one source occurrence. An
ambiguous owner or legacy location returns HTTP 409 before any deletion.

**Response 200:**
```json
{ "deleted": true }
```

---

### `POST /v1/docs/:doc_id/location?owner_id=...`

Update the stored `location_uri` for all chunks of a document (called after a file is moved/renamed).

**Request body:**
```json
{ "new_uri": "crisp+local://user-uuid@machine-uuid/new/path/to/report.pdf" }
```

**Response 200:**
```json
{ "updated": true }
```

---

### `POST /v1/docs/location/by-uri`

Update one source when the client knows its exact previous URI, including a bare
local path. The server preserves the selected document ID and owner, updates all
its chunks in one transaction, and advances its monotonic write timestamp.

```json
{ "old_uri": "/source/report.pdf", "new_uri": "/sorted/report.pdf" }
```

The response is `{ "updated": true }`. An unindexed old URI is a successful
no-op. Multiple identities at either URI, or a legacy selected identity shared
across several locations, return HTTP 409 without changing any rows. Use an
explicit owner/document operation or resolve the legacy source in those cases.

---

### `POST /v1/admin/build-ivf-pq`

Build an IVF-PQ approximate nearest neighbour index on LanceDB. Run this after bulk ingest (≥ 10 000 rows) to dramatically speed up vector search.

**Response 200:**
```json
{ "built": true }
```

> This operation is CPU-intensive and may take several minutes on large datasets.

---

## Deployment

### systemd (recommended for VPS)

Create `/etc/systemd/system/crisp-index-server.service`:

```ini
[Unit]
Description=crisp-index-server
After=network.target

[Service]
Type=simple
User=crisp
WorkingDirectory=/opt/crisp-index-server
ExecStart=/opt/crisp-index-server/crisp-index-server
Restart=on-failure
RestartSec=5s

Environment=PORT=8473
Environment=CRISP_API_KEY=your-secret-key-here
Environment=LANCE_DATA_DIR=/var/lib/crisp-index/data
Environment=EMBED_DIMS=1024
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

```bash
sudo mkdir -p /opt/crisp-index-server /var/lib/crisp-index/data
sudo useradd -r -s /sbin/nologin crisp
sudo cp target/release/crisp-index-server /opt/crisp-index-server/
sudo chown -R crisp:crisp /opt/crisp-index-server /var/lib/crisp-index
sudo systemctl daemon-reload
sudo systemctl enable --now crisp-index-server
sudo journalctl -fu crisp-index-server
```

---

### Docker

```dockerfile
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY target/release/crisp-index-server /usr/local/bin/crisp-index-server
RUN chmod +x /usr/local/bin/crisp-index-server
VOLUME ["/data"]
ENV LANCE_DATA_DIR=/data
EXPOSE 8473
CMD ["crisp-index-server"]
```

```bash
docker build -t crisp-index-server .
docker run -d \
  -p 8473:8473 \
  -v /var/lib/crisp-index/data:/data \
  -e CRISP_API_KEY=your-secret-key-here \
  -e EMBED_DIMS=1024 \
  --name crisp-index-server \
  crisp-index-server
```

Or with Docker Compose:

```yaml
services:
  crisp-index-server:
    image: crisp-index-server:latest
    ports:
      - "8473:8473"
    volumes:
      - crisp_data:/data
    environment:
      CRISP_API_KEY: your-secret-key-here
      LANCE_DATA_DIR: /data
      EMBED_DIMS: 1024
      RUST_LOG: info
    restart: unless-stopped

volumes:
  crisp_data:
```

---

### Reverse proxy (nginx)

Expose over HTTPS via nginx with Let's Encrypt:

```nginx
server {
    listen 443 ssl;
    server_name crisp.example.com;

    ssl_certificate     /etc/letsencrypt/live/crisp.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/crisp.example.com/privkey.pem;

    location / {
        proxy_pass         http://127.0.0.1:8473;
        proxy_set_header   Host $host;
        proxy_set_header   X-Real-IP $remote_addr;
        proxy_read_timeout 120s;
    }
}
```

Then set the CrispSorter remote URL to `https://crisp.example.com`.

---

## Security

- **Always set `CRISP_API_KEY`** in production. If unset, any bearer token is accepted.
- The API key is sent in the `Authorization: Bearer` header — use HTTPS in production.
- The server binds to `0.0.0.0` by default. Put it behind a firewall or reverse proxy.
- CORS is set to permissive (`*`) for Tauri desktop app compatibility. Restrict it if you expose the server to other clients.
- Data in `LANCE_DATA_DIR` is unencrypted — use filesystem-level encryption (LUKS, FileVault) for sensitive documents.

---

## Data layout

| Path within `LANCE_DATA_DIR` | Contents |
|---|---|
| `lance/documents.lance/` | Chunk rows, vectors, and retained table versions |
| `fts-v2/generation-*/` | Verified full-text generations |
| `fts-v2/state.sqlite` | Active generation, repair intents, logical clock, and deletion history |
| `fts-v2/server-lock.sqlite` | Exclusive server-process lease |
| `crisp_jobs.db` | Durable queued ingestion and task receipts |
| `fts/` | Preserved legacy full-text files when upgrading an earlier installation |

Back up the entire data directory while the server is stopped, or use a coherent
filesystem snapshot that includes SQLite WAL files. Restore the directory as a
unit; the next startup repairs any incomplete paired publication before serving
searches.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `embedding length X != expected Y` on ingest | Client embedder dimension doesn't match `EMBED_DIMS` | Set `EMBED_DIMS` to match the client model |
| Search returns no results | Index is empty or FTS not committed | Check the ingest task/error and pending repair status; successful publication reloads the full-text reader before returning |
| Vector search slow on large index | IVF-PQ index not built | Call `POST /v1/admin/build-ivf-pq` after bulk ingest |
| `CRISP_API_KEY is not set` warning | Missing env var | Set `CRISP_API_KEY` in `.env` or systemd unit |
| `address already in use` | Port 8473 taken | Set `PORT=<other>` |

---

## License

MIT


## Query contract and versioned upgrades

The desktop and server use the same bounded query parser and text analyzer.
Adjacent unquoted words require all words. Boolean `AND`, `OR`, and `NOT`, grouped
expressions, exact quoted phrases, `*` / `?` wildcards, and fuzzy terms such as
`report~1` are supported. Fields are `title`, `body` (alias `text`), and `headings`
(alias `h`); field scopes can contain a phrase or a group, for example
`title:"annual report"` or `body:(budget OR forecast)`.

`alpha pre/2 beta` requires alpha before beta with at most two intervening words.
`alpha w/2 beta` permits either order with the same distance bound. `/0` means
adjacent. Positional operands may be words, exact phrases, OR alternatives, or
bounded wildcard/fuzzy expansions. Unsupported or malformed combinations return
an error instead of changing the query's meaning. The parser bounds query size,
nesting, distance, and dictionary expansion.

In hybrid mode, explicit syntax constrains every vector candidate using the same
owner-filtered full-text snapshot. Plain natural-language questions can still
retrieve semantic matches. Search results contain one result per owner/document,
with stable score tie-breaking and snippets from matching text when available.

Clients negotiate `GET /v1/capabilities` before lexical search. The response
advertises API, grammar, and schema versions, readiness, searchable fields, and
chunk-boundary behavior. A negotiated `POST /v1/search` includes
`query_language_version`; successful responses confirm it through
`x-crisp-query-version`. Older servers remain usable for plain queries. Explicit
syntax requires a compatible ready server, and clients reject results that do
not confirm the negotiated version.

Run one server per data directory. On upgrade, stop the previous process and
start the new binary with the same `LANCE_DATA_DIR`. Before accepting requests,
the server streams all stored chunks into a disk spool, builds and verifies a
new full-text generation, canonicalizes legacy duplicate vector rows, and
publishes the generation through durable SQLite state. The legacy `fts/`
directory and earlier Lance versions remain retained. Provision disk space for
the new generation and temporary spool as well as the retained data. A rebuild
failure stops startup and reports its cause; it does not publish a partially
built full-text generation.

When byte offsets are available, verified overlapping chunks reconstruct the
original positional text. Legacy chunks without offsets remain separate
positional regions: words anywhere in the document are searchable, while a
phrase or proximity query cannot invent adjacency between unknown boundaries.
Re-ingest the source with offsets to recover cross-boundary positional matching.
The server has no translated field; a translated-field query returns an explicit
error.

Each mutation records a repair intent before atomically replacing its Lance
rows. Search and metadata reads repair interrupted publication before returning
results. Successful writes reload the full-text snapshot. Queue acknowledgments
and repair state use SQLite full synchronization. A process lock prevents two
current servers from opening the same full-text directory.

Document identifiers can be shared across owners. Delete and location-update
requests accept `?owner_id=...`. A bare identifier that belongs to multiple owners
returns HTTP 409 without changing either document. Scoped requests modify only
the selected owner's rows.

## Storage and HTTP contract tests

From the repository root:

```bash
cargo test --locked --manifest-path tests/server-search-contract/Cargo.toml
```

The isolated harness compiles the production server stores, queue, protocol,
routes, shared query engine, and desktop HTTP client. It exercises real LanceDB,
Tantivy, SQLite, and loopback HTTP, including failed/retried publication, migration,
owner collisions, strict hybrid constraints, Unicode text, and capability
negotiation. Model loading and platform proxy setup are adapters in this harness;
model quality and full packaged application behavior require their separate
checks.

### Occurrence identity and deletion-aware synchronization

Ordinary per-file uploads use the shared owner-and-source-URI identity and send
`source_identity_version: 1`. These requests must contain a complete declared
chunk sequence. Re-reading changed bytes at the same source updates that
occurrence; identical bytes at another URI or under another owner remain
separate. A unique legacy identifier at the exact source is reused to retain
existing references. A legacy identifier shared across locations is split only
for the selected source. Ambiguous legacy identities produce an explicit error.
Explicit API/import identifiers without the marker retain their supplied
identity. When a moved occurrence retains an identifier derived from its former
address, a new file at that address receives a distinct identifier, which later
refreshes reuse.

`GET /v1/sync/since` accepts `cursor_version=2`, an inclusive `ts` watermark,
`limit` (1–1000), and an opaque `cursor` continuation. Each page contains a bounded
combined set of live `rows` and `deletions`, ordered by write time, owner, and
document identifier. The cursor pins both the Lance snapshot and a deletion
history horizon. Concurrent updates and removals are picked up by the following
cycle; same-timestamp rows cannot be skipped at a page boundary.

Deletion receipts are committed after the paired stores confirm absence. An
interrupted deletion is repaired before a feed page is served; an intent that
never reached the vector store does not emit a false removal. Append-only
history keeps earlier cursors repeatable, and a newer live occurrence suppresses
an obsolete deletion receipt in the current snapshot. Each live row carries its
actual `indexed_at` timestamp so metadata conflict policies can compare the
correct document version.

Clients advance their durable cursor only after local application succeeds.
Saved version-1 continuation state is migrated by replaying its last completed
timestamp. Older clients can receive complete, deletion-free deltas; a page that
requires continuation or deletion support returns an upgrade error. Applying a
remote deletion removes the paired index records according to the configured
conflict policy while retaining local source files.

This feed transfers document metadata and deletion records, without source
bytes, full text, or embeddings. Desktop clients can apply accepted same-content
title/location changes while retaining reviewed body rows and evidence. Local
Wins and Keep Both preserve local intent. An extracted local document with a
different fingerprint requires a source refresh before applying remote
metadata; the client retains its prior checkpoint on that error. Each live
row includes its source fingerprint and write timestamp so the client can make
that decision without treating a summary as a complete content replacement.
