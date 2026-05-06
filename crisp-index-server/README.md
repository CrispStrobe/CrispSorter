# crisp-index-server

Self-hosted vector + full-text search backend for [CrispSorter](https://github.com/your-org/CrispSorter).

CrispSorter embeds documents locally using fastembed (BGE-M3 or similar) and sends the pre-computed dense vectors to this server. The server stores them in **LanceDB** (ANN) and **Tantivy** (BM25 full-text), then handles hybrid search with Reciprocal Rank Fusion (RRF, k=60).

**No GPU required on the server** — all embedding is done on the client.

---

## Architecture overview

```
CrispSorter (client)
  ├── Extract text / markdown from PDF, DOCX, TXT, MD
  ├── Chunk text (sliding window)
  ├── Embed locally via fastembed (BGE-M3, 1024-dim)
  └── POST /v1/ingest  ──► crisp-index-server
                                ├── LanceDB  (ANN vector search)
                                └── Tantivy  (BM25 full-text)

  Search query
  ├── Embed query locally
  └── POST /v1/search  ──► crisp-index-server
                                └── Hybrid RRF result  ──► CrispSorter UI
```

---

## Requirements

- Rust 1.77+ (`rustup update stable`)
- Linux x86_64 or macOS (ARM or x86)
- ~200 MB disk for the binary; data dir grows with indexed documents

---

## Build

```bash
# Debug build (fast compilation, slower runtime)
cargo build

# Release build (optimised, recommended for production)
cargo build --release

# Static binary for musl targets (for minimal Docker images / Alpine)
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

The release binary is at `target/release/crisp-index-server`.

---

## Configuration

All configuration is via environment variables. Create a `.env` file in the working directory for development (loaded automatically via `dotenvy`).

| Variable | Default | Description |
|---|---|---|
| `PORT` | `8473` | TCP port to listen on |
| `CRISP_API_KEY` | *(empty)* | Bearer token for authenticated endpoints. If empty, any token is accepted — **set this in production** |
| `LANCE_DATA_DIR` | `./data` | Directory where `lance/` (LanceDB) and `fts/` (Tantivy) subdirectories are created |
| `EMBED_DIMS` | `1024` | Embedding dimension — **must match the embedder used by the CrispSorter client** |
| `RUST_LOG` | `info` | Log level (`trace`, `debug`, `info`, `warn`, `error`) |

**Example `.env`:**
```env
PORT=8473
CRISP_API_KEY=your-secret-key-here
LANCE_DATA_DIR=/var/lib/crisp-index/data
EMBED_DIMS=1024
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
CRISP_API_KEY=dev-key cargo run

# Production (release binary)
CRISP_API_KEY=prod-key LANCE_DATA_DIR=/data ./crisp-index-server
```

---

## API reference

All endpoints except `/health` require a `Authorization: Bearer <CRISP_API_KEY>` header.

### `GET /health`

Liveness probe — no authentication required.

**Response 200:**
```json
{ "ok": true }
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
- `doc_count` — approximate distinct document count
- `embed_dims` — configured embedding dimension

---

### `POST /v1/ingest`

Store a single pre-embedded chunk.

**Request body:**
```json
{
  "doc_id":       "sha256-of-file-content",
  "chunk_index":  0,
  "full_text":    "plain text of the chunk",
  "full_text_md": "## Optional Markdown\n\nmarkdown version of the chunk",
  "headings":     ["Section title", "Subsection"],
  "embedding":    [0.012, -0.034, ...],
  "title":        "Document Title",
  "author":       "Jane Smith",
  "year":         2024,
  "filename":     "report.pdf",
  "ext":          "pdf",
  "language":     "en",
  "location_uri": "crisp+local://machine-uuid/user-uuid/path/to/report.pdf",
  "owner_id":     "user-uuid",
  "source_hash":  "sha256-of-file-content",
  "tags":         ["finance", "Q4"]
}
```

- `embedding` — pre-computed dense vector; length must equal `EMBED_DIMS`
- `full_text_md` and `headings` are optional
- Full-text indexing (Tantivy) only happens for `chunk_index == 0` to avoid duplicate BM25 entries
- The endpoint is **idempotent**: re-ingesting the same `doc_id` + `chunk_index` overwrites the previous record

**Response 200:**
```json
{ "chunk_count": 1, "write_time_ms": 12 }
```

**Response 422** — embedding dimension mismatch:
```json
{ "error": "embedding length 384 != expected 1024" }
```

---

### `POST /v1/search`

Search for documents. Supports text-only (BM25), vector-only (ANN), or hybrid (RRF).

**Request body:**
```json
{
  "query":     "machine learning transformers",
  "embedding": [0.012, -0.034, ...],
  "mode":      "hybrid",
  "limit":     20,
  "owner_id":  "user-uuid",
  "language":  "en",
  "year_min":  2020,
  "year_max":  2024
}
```

| Field | Required | Description |
|---|---|---|
| `mode` | yes | `"text"`, `"vector"`, or `"hybrid"` |
| `query` | for `text` / `hybrid` | BM25 query string |
| `embedding` | for `vector` / `hybrid` | pre-computed query vector |
| `limit` | no | max results (default: 20) |
| `owner_id` | no | filter to a specific user's documents |
| `language` | no | filter by language code (`"en"`, `"de"`, …) |
| `year_min` / `year_max` | no | filter by publication year |

**Response 200:**
```json
[
  {
    "doc_id":       "sha256...",
    "chunk_index":  0,
    "score":        0.0164,
    "title":        "Document Title",
    "author":       "Jane Smith",
    "year":         2024,
    "filename":     "report.pdf",
    "ext":          "pdf",
    "language":     "en",
    "location_uri": "crisp+local://machine-uuid/user-uuid/path/to/report.pdf",
    "full_text":    "plain text of the chunk",
    "headings":     ["Section title"]
  }
]
```

**Hybrid search (RRF):**
Merges BM25 and ANN ranked lists using Reciprocal Rank Fusion with k=60:
`score = 1/(k + rank_bm25) + 1/(k + rank_ann)`

**Response 422** — missing required fields for mode:
```json
{ "error": "hybrid mode requires both query and embedding" }
```

---

### `DELETE /v1/docs/:doc_id`

Delete all chunks for a document from both LanceDB and Tantivy.

**Response 200:**
```json
{ "deleted": true }
```

---

### `POST /v1/docs/:doc_id/location`

Update the stored `location_uri` for all chunks of a document (called after a file is moved/renamed).

**Request body:**
```json
{ "new_uri": "crisp+local://machine-uuid/user-uuid/new/path/to/report.pdf" }
```

**Response 200:**
```json
{ "updated": true }
```

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

```
LANCE_DATA_DIR/
  lance/
    chunks.lance/          # LanceDB table — one row per chunk
      _latest_manifest/
      data/
      ...
  fts/
    meta.json              # Tantivy index metadata
    .managed.json
    *.term, *.idx, *.pos   # Tantivy segment files
```

Back up the entire `LANCE_DATA_DIR` to preserve the index. Restoring is just copying the directory back.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `embedding length X != expected Y` on ingest | Client embedder dimension doesn't match `EMBED_DIMS` | Set `EMBED_DIMS` to match the client model |
| Search returns no results | Index is empty or FTS not committed | Wait for Tantivy auto-commit (1 s) or restart server |
| Vector search slow on large index | IVF-PQ index not built | Call `POST /v1/admin/build-ivf-pq` after bulk ingest |
| `CRISP_API_KEY is not set` warning | Missing env var | Set `CRISP_API_KEY` in `.env` or systemd unit |
| `address already in use` | Port 8473 taken | Set `PORT=<other>` |

---

## License

MIT
