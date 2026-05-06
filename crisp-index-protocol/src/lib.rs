//! Wire-format types for the `crisp-index-server` HTTP API.
//!
//! Single source of truth for the request/response shapes that flow
//! between [`CrispSorter`](https://github.com/CrispStrobe/CrispSorter) (the
//! Tauri desktop client, in `src-tauri/`) and the `crisp-index-server`
//! Axum backend (in `crisp-index-server/`). Both crates depend on this one;
//! changing a struct here changes both sides at once.
//!
//! Endpoints:
//!
//! ```text
//! GET    /health                        — HealthResponse
//! GET    /v1/stats                      — StatsResponse
//! POST   /v1/ingest         IngestChunk → IngestResponse
//! POST   /v1/search       SearchRequest → Vec<SearchHit>
//! DELETE /v1/docs/:id                   — DeleteResponse
//! POST   /v1/docs/:id/location  UpdateLocationBody → UpdateLocationResponse
//! POST   /v1/admin/build-ivf-pq         — { "built": true }
//! ```
//!
//! Authentication: `Authorization: Bearer <CRISP_API_KEY>` on every `/v1/*`
//! request; `/health` is unauthenticated. See `crisp-index-server/README.md`
//! for deployment specifics.
//!
//! Conventions:
//!
//! - Optional metadata fields use `Option<T>` with
//!   `#[serde(default, skip_serializing_if = "Option::is_none")]` so that
//!   missing fields round-trip as `None`. New fields can be added as
//!   `Option<...>` without breaking older clients/servers.
//! - List fields use `Vec<T>` with `#[serde(default)]` and serialise as
//!   `[]` even when empty (small wins for protocol explicitness; servers
//!   that are happier with absent-vs-empty have not yet been observed).
//! - The richer client-side `SearchResult` struct (in
//!   `src-tauri/src/index/schema.rs`) is a strict superset of the
//!   `SearchHit` wire type below — server-only fields stay `None` until
//!   the server learns to populate them.

#![warn(missing_docs)]

use serde::{Deserialize, Serialize};

// ── Ingest ──────────────────────────────────────────────────────────────────

/// One indexed chunk on the wire. Sent by the client to `POST /v1/ingest`
/// (singular, today) — the future bulk endpoint
/// `POST /v1/ingest/batch` (PLAN P11 step 4) will accept `Vec<IngestChunk>`.
///
/// `embedding` is a pre-computed dense vector (typically 1024-dim BGE-M3).
/// Its length must equal the server's configured `EMBED_DIMS`; servers
/// reject mismatches with HTTP 422.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestChunk {
    /// Stable document identifier — typically the SHA-256 of the source
    /// file's bytes, so re-ingesting the same file is idempotent.
    pub doc_id: String,
    /// 0-based index within the document's chunk sequence.
    pub chunk_index: i32,
    /// Plain-text body of the chunk. Required.
    pub full_text: String,
    /// Optional Markdown rendering of the chunk (preserves headings /
    /// lists / tables for display).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_text_md: Option<String>,
    /// Optional list of heading-path strings leading to this chunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headings: Option<Vec<String>>,
    /// Pre-computed dense embedding. Length must match server `EMBED_DIMS`.
    pub embedding: Vec<f32>,

    // ── Document-level metadata (mostly redundant across chunks of one doc;
    //    the server's denormalised storage trades a few KB for fewer joins).

    /// Display title (PDF /Info.Title, DOCX core.title, etc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Display author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Publication year extracted from metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    /// File name including extension.
    pub filename: String,
    /// Lowercase extension without leading dot (`pdf`, `docx`, …).
    pub ext: String,
    /// ISO-639-1 language code (`en`, `de`, …) or empty if unknown.
    pub language: String,
    /// Stable URI for the file's location, e.g. `crisp+local://machine-uuid/owner/path`.
    pub location_uri: String,
    /// Owner / user identifier — used for multi-tenant filtering.
    pub owner_id: String,
    /// SHA-256 of the source file's bytes (typically equals `doc_id`).
    pub source_hash: String,
    /// User-supplied tags. Always serialised, empty `[]` if none.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Response for `POST /v1/ingest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResponse {
    /// Number of chunks accepted (always 1 today; will be `chunks.len()`
    /// once the bulk endpoint lands).
    pub chunk_count: usize,
    /// Server-measured time spent in the LanceDB / Tantivy write path.
    pub write_time_ms: u64,
}

/// Bulk-ingest request body for `POST /v1/ingest/batch` (PLAN P11
/// step 4). Same chunk shape as the singular endpoint; the server
/// coalesces all chunks into one Arrow record-batch + one Tantivy
/// commit, which is materially faster than N individual writes for
/// large ingests.
///
/// On the local side the same shape lets `index_ingest_batch` write N
/// documents in a single LanceDB transaction; the wire format and the
/// in-process struct are identical so the client/server cutover is a
/// trait-impl swap, not a re-encode.
///
/// Server response is `IngestResponse` with `chunk_count = chunks.len()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestBatch {
    /// Pre-embedded chunks. Every element is validated against the
    /// server's `EMBED_DIMS` exactly the way the singular endpoint
    /// does; one mismatch rejects the whole batch with HTTP 422 to
    /// keep partial writes off the table.
    #[serde(default)]
    pub chunks: Vec<IngestChunk>,
}

// ── Search ──────────────────────────────────────────────────────────────────

/// Hybrid-search request body for `POST /v1/search`. The server picks the
/// scoring path from `mode`:
///
/// - `text`   — BM25 only; requires `query`.
/// - `vector` — ANN only; requires `embedding`.
/// - `hybrid` — RRF (k=60) merge of both; requires both. Default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchRequest {
    /// Plain-text BM25 query. Required for `text` and `hybrid` modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Pre-computed query embedding. Required for `vector` and `hybrid` modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    /// `text` | `vector` | `hybrid`. Server defaults to `hybrid` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Maximum results to return; server clamps to a sane upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,

    // ── Filters (flat for backwards compatibility; matches existing wire). ──

    /// Restrict to one owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    /// Restrict to one language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Lower bound (inclusive) on `year`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_min: Option<i32>,
    /// Upper bound (inclusive) on `year`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_max: Option<i32>,
}

/// Filters in struct form, used internally by both client and server. Wire
/// representation is the flat fields on [`SearchRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchFilters {
    /// Restrict to one owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    /// Restrict to one language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Lower bound (inclusive) on `year`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_min: Option<i32>,
    /// Upper bound (inclusive) on `year`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_max: Option<i32>,
}

impl SearchFilters {
    /// Build a LanceDB SQL `WHERE` clause fragment from the active filters.
    /// Returns `None` if no filters are set.
    ///
    /// Single-quote escaping is naive (`'` → `''`) — matches the existing
    /// implementations on both sides. LanceDB's pre-filter language doesn't
    /// have a parameter binder yet, so this is the conservative path.
    pub fn to_lance_sql(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(o) = &self.owner_id {
            parts.push(format!("owner_id = '{}'", o.replace('\'', "''")));
        }
        if let Some(l) = &self.language {
            parts.push(format!("language = '{}'", l.replace('\'', "''")));
        }
        if let Some(y) = self.year_min {
            parts.push(format!("year >= {y}"));
        }
        if let Some(y) = self.year_max {
            parts.push(format!("year <= {y}"));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" AND "))
        }
    }
}

/// One hit returned by `POST /v1/search`. The fields below are the strict
/// wire subset; the client-side `SearchResult` (in
/// `src-tauri/src/index/schema.rs`) has additional optional fields
/// (`metadata_json`, `catalog_source`, `volume_id`) that are tolerated as
/// `None` when reading server responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// Stable document identifier.
    pub doc_id: String,
    /// Stable URI for the file's location.
    pub location_uri: String,
    /// Owner identifier.
    pub owner_id: String,
    /// Display title (or absent if not extracted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Display author (or absent if not extracted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Publication year (or absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    /// File name including extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Lowercase extension without leading dot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,
    /// ISO-639-1 language code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Snippet of the matched chunk (server-determined length).
    pub snippet: String,
    /// Relevance score. Higher is better; units depend on `mode`:
    /// cosine similarity, BM25, or RRF fusion score.
    pub score: f32,
    /// Index of the chunk that matched within its document.
    pub chunk_index: i32,
}

// ── Docs admin ──────────────────────────────────────────────────────────────

/// Body for `POST /v1/docs/:doc_id/location` — used to update the stored
/// `location_uri` for every chunk of a document after the file has been
/// moved or renamed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateLocationBody {
    /// New URI to store for every row matching `doc_id`.
    pub new_uri: String,
}

/// Body for the auxiliary `POST /v1/docs/location/by-uri` endpoint — used
/// when the client only knows the *old* URI, not the doc ID. (Currently
/// only the client posts this; the server stub is forthcoming.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateLocationByUriBody {
    /// Existing `location_uri` to match.
    pub old_uri: String,
    /// Replacement value.
    pub new_uri: String,
}

/// Response for `POST /v1/docs/:doc_id/location`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateLocationResponse {
    /// Always `true` on success; the endpoint returns 5xx on failure.
    pub updated: bool,
}

/// Response for `DELETE /v1/docs/:doc_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResponse {
    /// Always `true` on success.
    pub deleted: bool,
}

// ── Stats / health / errors ─────────────────────────────────────────────────

/// Response for `GET /v1/stats`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    /// Total number of chunk rows across all documents.
    pub row_count: usize,
    /// Approximate distinct-document count (may be cached / sampled on
    /// large indexes).
    pub doc_count: usize,
    /// Server-configured embedding dimension.
    pub embed_dims: usize,
}

/// Response for `GET /health`. The client reads only the status code today
/// (200 ⇒ alive); this struct is provided for completeness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Always the literal string `"ok"` today.
    pub status: String,
}

/// Standard error envelope returned by any `/v1/*` endpoint that fails
/// validation (HTTP 4xx) or hits a server-side error (HTTP 5xx).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Human-readable error message. Stable enough for `assert_eq!` in
    /// tests; not stable enough for users to switch on.
    pub error: String,
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_chunk_round_trip() {
        let c = IngestChunk {
            doc_id: "abc".into(),
            chunk_index: 0,
            full_text: "hello".into(),
            full_text_md: None,
            headings: None,
            embedding: vec![0.1, 0.2, 0.3],
            title: Some("Demo".into()),
            author: None,
            year: Some(2026),
            filename: "demo.txt".into(),
            ext: "txt".into(),
            language: "en".into(),
            location_uri: "crisp+local://m/u/demo.txt".into(),
            owner_id: "u".into(),
            source_hash: "deadbeef".into(),
            tags: vec!["one".into(), "two".into()],
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"tags\":[\"one\",\"two\"]"));
        // Optionals that are None must be skipped.
        assert!(!json.contains("full_text_md"));
        assert!(!json.contains("headings"));
        assert!(!json.contains("author"));
        let back: IngestChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(back.doc_id, "abc");
        assert_eq!(back.embedding.len(), 3);
        assert_eq!(back.tags.len(), 2);
    }

    #[test]
    fn ingest_batch_round_trip() {
        let chunk = |i: i32| IngestChunk {
            doc_id: format!("doc-{i}"),
            chunk_index: 0,
            full_text: format!("text-{i}"),
            full_text_md: None,
            headings: None,
            embedding: vec![0.0; 4],
            title: None,
            author: None,
            year: None,
            filename: format!("doc-{i}.txt"),
            ext: "txt".into(),
            language: "en".into(),
            location_uri: format!("crisp+local://m/u/doc-{i}.txt"),
            owner_id: "u".into(),
            source_hash: format!("hash-{i}"),
            tags: Vec::new(),
        };
        let b = IngestBatch {
            chunks: (0..3).map(chunk).collect(),
        };
        let json = serde_json::to_string(&b).unwrap();
        // Three chunks present; `chunks` array is non-null.
        assert!(json.contains("\"chunks\":["));
        assert!(json.contains("\"doc-0\""));
        assert!(json.contains("\"doc-2\""));

        let back: IngestBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back.chunks.len(), 3);
        assert_eq!(back.chunks[1].doc_id, "doc-1");

        // Empty / missing `chunks` deserialises to an empty Vec
        // (forward-compat: a server that received `{}` would treat
        // it as "ingest nothing" rather than rejecting on missing
        // field).
        let empty: IngestBatch = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.chunks.len(), 0);
    }

    #[test]
    fn search_filters_to_lance_sql() {
        let f = SearchFilters::default();
        assert_eq!(f.to_lance_sql(), None);

        let f = SearchFilters {
            owner_id: Some("u'1".into()),
            language: Some("en".into()),
            year_min: Some(2020),
            year_max: Some(2024),
        };
        let sql = f.to_lance_sql().unwrap();
        assert!(sql.contains("owner_id = 'u''1'"));
        assert!(sql.contains("language = 'en'"));
        assert!(sql.contains("year >= 2020"));
        assert!(sql.contains("year <= 2024"));
    }

    #[test]
    fn search_request_omits_none() {
        let req = SearchRequest {
            query: Some("hello".into()),
            mode: Some("hybrid".into()),
            limit: Some(20),
            ..Default::default()
        };
        let json = serde_json::to_string(&req).unwrap();
        // owner_id, embedding, etc must not appear.
        assert!(!json.contains("owner_id"));
        assert!(!json.contains("embedding"));
        assert!(json.contains("\"mode\":\"hybrid\""));
    }

    #[test]
    fn search_hit_tolerates_missing_optionals() {
        let json = r#"{
          "doc_id": "d", "location_uri": "u", "owner_id": "o",
          "snippet": "s", "score": 0.5, "chunk_index": 0
        }"#;
        let hit: SearchHit = serde_json::from_str(json).unwrap();
        assert_eq!(hit.doc_id, "d");
        assert!(hit.title.is_none());
        assert!(hit.ext.is_none());
    }
}
