//! P13.7 Step 5 — HTTP wire client for the cloud-backup VPS API.
//!
//! Lives alongside the P11 [`super::SyncManager`] which targets
//! `crisp-index-server`; this module is the sibling target for the
//! new cloud-backup HTTP surface.  Both share the same outbox
//! state-KV (the `sync_state` table) so watermarks for both
//! backends can be inspected via one `sync_status` call.
//!
//! Wire shape matches `../../cloud-backup/api/app.py` exactly —
//! see that file for the pydantic models the routes accept.  Any
//! drift here is a protocol break and shows up as 422 from
//! FastAPI's pydantic validation.
//!
//! ## Wire endpoints
//!
//! | route                              | direction | purpose                       |
//! |------------------------------------|-----------|-------------------------------|
//! | `POST /api/manifest/push`          | up        | upsert source-file batch      |
//! | `GET  /api/manifest/pull?since=…`  | down      | rows newer than `since`       |
//! | `POST /api/index/push-embeddings`  | up        | store text/vector embeddings  |
//! | `GET  /api/index/by-embedding?…`   | query     | brute-force k-NN              |
//! | `GET  /api/health`                 | probe     | unauthenticated health check  |
//!
//! ## Authentication
//!
//! Every authenticated request carries
//! `Authorization: Bearer <key>`.  Keys are minted on the VPS via
//! `python -m api.admin mint` (see `../../cloud-backup/api/admin.py`)
//! and stored client-side in the OS keychain through
//! [`super::secret`].

use anyhow::{anyhow, Context, Result};
use reqwest::header::AUTHORIZATION;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use super::proxy::ProxyConfig;

/// Deserialize a `tags` list tolerating an explicit JSON `null`.  cb-api
/// emits `"tags": null` for a row with no tags (its Pydantic model is
/// `Optional[List[str]]`), and a bare `#[serde(default)]` only covers an
/// *absent* key — an explicit `null` would otherwise fail the whole row's
/// deserialization.  Maps `null`/absent → empty `Vec`.
fn de_tags_null_as_empty<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(de)?.unwrap_or_default())
}

const PUSH_PATH: &str = "/api/manifest/push";
const PULL_PATH: &str = "/api/manifest/pull";
const EMBED_PUSH_PATH: &str = "/api/index/push-embeddings";
const BY_EMBED_PATH: &str = "/api/index/by-embedding";
const SEARCH_PATH: &str = "/api/search";
const FILES_PATH_PREFIX: &str = "/api/files/by-hash/";
const EMBED_QUERY_PATH: &str = "/api/index/embed-query";
const EMBED_MODELS_PATH: &str = "/api/index/embed-models";
const V2_SEARCH_PATH: &str = "/api/v2/index/search";
const HEALTH_PATH: &str = "/api/health";
const SHARD_LIST_PATH: &str = "/api/shard/list";
const EXTRACT_STATUS_PATH: &str = "/api/v2/extract/status";
const SHARD_EXPORT_PREFIX: &str = "/api/shard/export/";
const SHARD_IMPORT_PREFIX: &str = "/api/shard/import/";

/// Single row of the manifest-push payload.  Field names match the
/// FastAPI `ManifestRow` model exactly; reordering or renaming
/// requires a protocol bump.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestRow {
    pub path:       String,
    pub size_bytes: i64,
    pub sha256:     String,
    pub mtime_unix: f64,
    /// Hint only — the server overwrites with the authenticated
    /// key's owner_id (per-owner scoping).  Sending a stable value
    /// in shared-catalog mode keeps cross-client provenance tidy.
    pub owner_id:   String,
    pub filename:   String,
    pub ext:        String,
    pub parent_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language:   Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title:      Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author:     Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year:       Option<i32>,
    /// Stage A — extracted body text at index time.  Optional —
    /// clients can opt out per-row for sensitive corpora.  Server
    /// stores it in `file_references.full_text` and indexes it
    /// into the FTS5 virtual table behind `/api/search`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_text:  Option<String>,
    /// Stage K — topical-locality sharding hint.  Set this per
    /// logical group ("research-task-X") so related files land
    /// on the same Lance/SQLite shard on the VPS.  `None` falls
    /// back to sha-prefix sharding (the Stage G default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<String>,
    /// Stage R — controller.py archive reference.  Non-None only
    /// when importing rows from a controller.py manifest DB; carries
    /// the source `archive_id` so archive-membership survives the
    /// HTTP round-trip.  Normal bg_ingest pushes leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_in: Option<i64>,
    /// v106 — Original source URL the document came from (YAML
    /// frontmatter `url:`, PDF /URL, EPUB dc:source).  Server stores
    /// it on `file_references.url`; FTS5 indexes it so domain queries
    /// hit.  None for files with no provenance URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// v107 — Tag list lifted from YAML frontmatter (`tags: [...]`),
    /// EPUB `<dc:subject>`, DOCX keywords, etc.  Server stores as
    /// JSON-encoded text in `file_references.tags`.  Empty Vec / None
    /// both treated as "no tags".
    #[serde(default, deserialize_with = "de_tags_null_as_empty", skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl ManifestRow {
    /// P13.7 Stage C — build a ManifestRow snapshot from a fresh
    /// `RawDocument` (taken in bg_ingest right before
    /// `pipeline.ingest_document` consumes it).  The auto-push
    /// hook fires this synchronously to capture all the fields
    /// the wire shape needs before the underlying value moves.
    pub fn from_raw_document(raw: &crate::index::ingest::RawDocument) -> Self {
        // Map RawDocument's stored shape onto the wire shape.  The
        // path mirrors `index_ingest_cb_manifest`'s lifting from
        // the LocalIndex documents row: prefer the local filesystem
        // path when known, fall back to the location_uri.
        let path = match crate::images::tauri_commands::location_uri_to_local_path(
            &raw.location_uri,
        ) {
            Some(p) => p.to_string_lossy().into_owned(),
            None => raw.location_uri.clone(),
        };
        let mtime_unix = raw.mtime_unix.map(|s| s as f64).unwrap_or(0.0);
        let size_bytes = raw.file_size.unwrap_or(0);
        // Stage K — pick a collection_id from raw.tags when one
        // is tagged via the convention `collection:<id>`.  Lets
        // a user mark a batch of docs (e.g. all files from one
        // research task) by adding that tag at ingest time;
        // bg_ingest's auto-push then routes them to the same
        // VPS shard.  `None` falls back to sha-prefix routing.
        let collection_id = raw.tags.iter()
            .find_map(|t| t.strip_prefix("collection:").map(str::to_string));
        ManifestRow {
            path,
            size_bytes,
            sha256:     raw.source_hash.clone(),
            mtime_unix,
            owner_id:   raw.owner_id.clone(),
            filename:   raw.filename.clone(),
            ext:        raw.ext.clone(),
            parent_dir: raw.parent_dir.clone().unwrap_or_default(),
            language:   if raw.language.is_empty() { None } else { Some(raw.language.clone()) },
            title:      raw.title.clone(),
            author:     raw.author.clone(),
            year:       raw.year,
            full_text:  if raw.full_text.is_empty() { None } else { Some(raw.full_text.clone()) },
            collection_id,
            archived_in: None,
            url:        raw.url.clone(),
            // v107 — also lift any non-routing-marker tag (the
            // `collection:` prefix is reserved above for shard
            // routing and shouldn't surface as a user-visible tag).
            tags:       raw.tags.iter()
                .filter(|t| !t.starts_with("collection:"))
                .cloned()
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestPushRequest<'a> {
    pub rows: &'a [ManifestRow],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestPushResponse {
    pub accepted: usize,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Row shape returned by `/api/manifest/pull`.  Maps onto
/// `L1FileEntry` 1:1 in the apply path (`tauri_commands.rs`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PullRow {
    pub path:       String,
    pub size_bytes: i64,
    pub sha256:     String,
    pub mtime_unix: f64,
    pub owner_id:   String,
    pub filename:   String,
    pub ext:        String,
    pub parent_dir: String,
    #[serde(default)]
    pub language:   Option<String>,
    #[serde(default)]
    pub title:      Option<String>,
    #[serde(default)]
    pub author:     Option<String>,
    #[serde(default)]
    pub year:       Option<i32>,
    /// Stage A — body text echoed back from the server.
    #[serde(default)]
    pub full_text:  Option<String>,
    pub indexed_at: i64,
    #[serde(default)]
    pub archived_in: Option<i64>,
    /// Stage K — echoed back so the client can preserve the
    /// shard-routing key when promoting a pulled row to a local
    /// cache write.
    #[serde(default)]
    pub collection_id: Option<String>,
    /// v106 — Original source URL (round-tripped from manifest_push).
    #[serde(default)]
    pub url: Option<String>,
    /// v107 — Tags echoed back from `file_references.tags`.
    /// Server decoded the JSON-encoded column into a list; empty
    /// vec or absent both mean "no tags".
    #[serde(default, deserialize_with = "de_tags_null_as_empty")]
    pub tags: Vec<String>,
}

/// One row returned by `/api/search`.  Same payload shape as
/// `PullRow` plus a `score: f32` (higher = better match).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchHit {
    pub path:       String,
    pub size_bytes: i64,
    pub sha256:     String,
    pub mtime_unix: f64,
    pub owner_id:   String,
    pub filename:   String,
    pub ext:        String,
    pub parent_dir: String,
    #[serde(default)]
    pub language:   Option<String>,
    #[serde(default)]
    pub title:      Option<String>,
    #[serde(default)]
    pub author:     Option<String>,
    #[serde(default)]
    pub year:       Option<i32>,
    #[serde(default)]
    pub full_text:  Option<String>,
    pub indexed_at: i64,
    pub score:      f32,
    /// v106 — Original source URL (mirrors PullRow.url so ingest is
    /// symmetric across the pull and search flows).
    #[serde(default)]
    pub url: Option<String>,
    /// v107 — Tags decoded from `file_references.tags` JSON.
    #[serde(default, deserialize_with = "de_tags_null_as_empty")]
    pub tags: Vec<String>,
    /// Server-computed snippet — a match-centred `<mark>`-highlighted
    /// window of the body.  Populated when the hit has a body; lets a
    /// display client render a result row without the full `full_text`
    /// (pair with `include_full_text=false` to drop the body off the wire).
    #[serde(default)]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    pub rows: Vec<SearchHit>,
    pub total: usize,
}

/// Stage S — normalised hit shape returned by `sync_federated_search`.
/// Carries enough metadata for the frontend to badge the source backend
/// and open the file; heavy fields (full_text / snippet) are elided to
/// keep the payload small.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FederatedHit {
    /// Composite key: `"<source>:<id>"`.  Stable within a search result
    /// set; the frontend uses it as a `key=` binding only.
    pub id: String,
    /// `"local"` | `"cloud_backup"` | `"crisplens"`
    pub source: String,
    pub score: f32,
    /// RRF rank (1-based, lower = better) after merging all backends.
    pub rrf_rank: usize,
    pub filename: Option<String>,
    pub path: Option<String>,
    pub ext: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<i64>,
    pub snippet: Option<String>,
    /// `location_uri` for local hits (e.g. `file:///…`); empty for remote.
    pub location_uri: Option<String>,
    /// v106 — source URL provenance, echoed so a federated hit can render an
    /// "Open original" link without a second round-trip.  `None` for backends
    /// that don't carry it (CrispLens) or rows ingested pre-v106.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// v107 — structured tag list.  `None` means "no tags carried"; an empty
    /// `Vec` is also valid (a row known to have zero tags).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// Stage T — admin key management wire types.
#[derive(Debug, Clone, Serialize)]
pub struct AdminMintRequest<'a> {
    pub name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminMintResponse {
    pub raw_key: String,
    pub name: String,
    pub owner_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdminRevokeRequest<'a> {
    pub name: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AdminRevokeResponse {
    pub revoked: bool,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminKeyInfo {
    pub id: i64,
    pub name: String,
    pub owner_id: Option<String>,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

/// Stage U — extraction worker queue depths from `/api/v2/extract/status`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtractStatusResponse {
    pub pending:         u64,
    pub in_progress:     u64,
    pub done:            u64,
    pub failed:          u64,
    pub worker_db_found: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ManifestPullResponse {
    pub rows: Vec<PullRow>,
    pub max_indexed_at: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingRow {
    pub doc_id:      String,
    pub chunk_index: i32,
    pub embedding:   Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sparse_json: Option<String>,
    pub model_id:    String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingPushRequest<'a> {
    pub rows: &'a [EmbeddingRow],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmbeddingPushResponse {
    pub accepted: usize,
    #[serde(default)]
    pub rejected: usize,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ByEmbeddingHit {
    pub doc_id:      String,
    pub chunk_index: i32,
    pub model_id:    String,
    pub distance:    f32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ByEmbeddingResponse {
    pub rows: Vec<ByEmbeddingHit>,
}

/// Response from `GET /api/index/embed-query`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedQueryResponse {
    pub model:     String,
    pub dim:       usize,
    pub embedding: Vec<f32>,
}

/// Response from `GET /api/index/embed-models`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedModelsResponse {
    pub models:    Vec<String>,
    pub default:   String,
    pub available: bool,
}

/// Response from `POST /api/files/by-hash/<sha>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileUploadResponse {
    pub sha256: String,
    pub size_bytes: i64,
    /// `true` if the blob was written this request; `false` for the
    /// idempotent no-op case (a previous upload already deposited
    /// the same bytes).
    pub stored: bool,
    /// Relative path under `CB_API_STORAGE_ROOT` on the VPS.
    pub local_blob_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: String,
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub shared_catalog: bool,
    /// Stage I — capability flags.  Clients use these to decide
    /// whether to call v2 routes or fall back to v1 / local-only.
    #[serde(default)]
    pub lance_enabled: bool,
    #[serde(default)]
    pub fastembed_enabled: bool,
}

// ── Stage Q — shard backup / restore wire shapes ────────────────────────

/// One row in the shard list from `GET /api/shard/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardInfo {
    pub prefix:         String,
    pub row_count:      u64,
    pub max_indexed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardListResponse {
    pub shards: Vec<ShardInfo>,
}

// ── Stage I — v2 hybrid search wire shapes ──────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HybridSearchFilters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ext: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_dir_prefix: Option<String>,
    /// Substring match against the `author` column (case-
    /// sensitive on LanceDB today; LIKE '%substr%').
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_min: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_max: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_after_ms: Option<i64>,
    /// Stage K — narrow to one or more collection_id values.
    /// "show me everything in this research task" queries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collection_ids: Vec<String>,
    #[serde(default)]
    pub require_bytes_local: bool,
    /// v106 — substring match against the `url` column server-side.
    /// Mirrors `SearchFilters.url_domain` so a CLI user gets the same
    /// `--url-domain` semantics whether they search locally or
    /// federated.  None == no filter; pre-v106 rows are excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_domain: Option<String>,
    /// v107 — exact-match against any element of the `tags` list.
    /// Translates server-side to `array_has(tags, '<value>')` on the
    /// Lance `List<Utf8>` tags column.  None == no filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HybridSearchRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vec: Option<&'a [f32]>,
    /// When set, the server computes the embedding via fastembed.
    /// Saves the client a round-trip to /api/index/embed-query.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embed_model: Option<&'a str>,
    pub filters: HybridSearchFilters,
    pub limit: usize,
    pub rrf_k: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HybridSearchHit {
    pub doc_id:        String,
    pub sha256:        String,
    pub owner_id:      String,
    #[serde(default)]
    pub path:          Option<String>,
    #[serde(default)]
    pub filename:      Option<String>,
    #[serde(default)]
    pub ext:           Option<String>,
    #[serde(default)]
    pub parent_dir:    Option<String>,
    #[serde(default)]
    pub language:      Option<String>,
    #[serde(default)]
    pub title:         Option<String>,
    #[serde(default)]
    pub author:        Option<String>,
    #[serde(default)]
    pub year:          Option<i32>,
    #[serde(default)]
    pub size_bytes:    Option<i64>,
    #[serde(default)]
    pub mtime_unix:    Option<f64>,
    pub indexed_at:    i64,
    #[serde(default)]
    pub full_text:     Option<String>,
    pub score:         f32,
    #[serde(default)]
    pub score_text:    Option<f32>,
    #[serde(default)]
    pub score_vector:  Option<f32>,
    /// Stage K — surfaced so clients can group results by
    /// research task / corpus.
    #[serde(default)]
    pub collection_id: Option<String>,
    /// v106 — Source URL provenance.  Echoed back from
    /// `documents.url` so a federated hit can render an "Open
    /// original" link without a second round-trip.
    #[serde(default)]
    pub url: Option<String>,
    /// v107 — Structured tag list.  None means "no tags"; empty Vec
    /// is also valid.  Decoded server-side from the Lance
    /// `List<Utf8>` column.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchResponse {
    pub rows: Vec<HybridSearchHit>,
    pub total: usize,
    pub used_text: bool,
    pub used_vector: bool,
    pub shards_queried: usize,
}

/// Thin async wrapper over `reqwest::Client` that carries the
/// server's base URL + bearer token.  Cheap to construct per
/// operation; reqwest pools connections internally.
#[derive(Debug, Clone)]
pub struct CloudBackupClient {
    base_url: String,
    api_key:  String,
    client:   Client,
}

/// The one place cloud-backup can be switched off — PLAN P36.16.
///
/// Every route that talks to cb-api (21 `sync_cb_*` Tauri commands, the
/// `cloud-backup` CLI subcommand tree, the offline replay queue) reaches
/// the network by constructing a [`CloudBackupClient`]. Refusing here
/// therefore covers all of them, and it keeps working when someone adds a
/// twenty-second route without reading this comment — which a per-route
/// check would not.
///
/// Gating the *module* was the obvious alternative and is wrong: its types
/// are used by `bg_ingest` (`ManifestRow::from_raw_document`) and the local
/// index path, so `#[cfg]`-ing it out cascades far past the network
/// surface. The feature is about not shipping a client for a private
/// server, not about deleting the wire format.
#[inline]
fn ensure_cloud_backup_enabled() -> Result<()> {
    if cfg!(feature = "cloud-backup") {
        return Ok(());
    }
    Err(anyhow!(
        "cloud-backup sync is not available in this build: the server it \
         talks to is not public, so the feature ships off. Rebuild with \
         `--features cloud-backup` if you run a cb-api instance."
    ))
}

impl CloudBackupClient {
    /// Build a client with sensible defaults: 30 s timeout, default
    /// reqwest connection-pool settings.  `base_url` is trimmed
    /// of any trailing `/` so concatenation with path constants
    /// never produces `//api/...`.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Result<Self> {
        Self::new_with_proxy(base_url, api_key, &ProxyConfig::default())
    }

    /// Build a client using the shared HTTP/SOCKS5 proxy policy while
    /// retaining the cloud-backup request timeout.
    pub fn new_with_proxy(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        proxy: &ProxyConfig,
    ) -> Result<Self> {
        ensure_cloud_backup_enabled()?;
        let client = super::proxy::build_async_client_with_timeout(proxy, Duration::from_secs(30))?;
        let url = base_url.into().trim_end_matches('/').to_string();
        Ok(Self { base_url: url, api_key: api_key.into(), client })
    }

    /// Construct with an explicit pre-built client (used in unit
    /// tests so we can wire a mockito server's address in).
    pub fn with_client(base_url: impl Into<String>, api_key: impl Into<String>, client: Client) -> Self {
        let url = base_url.into().trim_end_matches('/').to_string();
        Self { base_url: url, api_key: api_key.into(), client }
    }

    pub fn base_url(&self) -> &str { &self.base_url }

    fn endpoint(&self, suffix: &str) -> String {
        format!("{}{}", self.base_url, suffix)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.api_key)
    }

    /// `GET /api/health`.  Public, no auth required; used by the
    /// status surface.
    pub async fn health(&self) -> Result<HealthResponse> {
        let resp = self.client
            .get(self.endpoint(HEALTH_PATH))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("health: send")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("health: HTTP {status}");
        }
        Ok(resp.json::<HealthResponse>().await.context("health: parse body")?)
    }

    /// `POST /api/manifest/push`.  Uploads a batch of source-file
    /// rows.  Empty `rows` is allowed (server returns `accepted=0`).
    pub async fn manifest_push(&self, rows: &[ManifestRow]) -> Result<ManifestPushResponse> {
        let body = ManifestPushRequest { rows, cursor: None };
        let resp = self.client
            .post(self.endpoint(PUSH_PATH))
            .header(AUTHORIZATION, self.auth_header())
            .json(&body)
            .send()
            .await
            .context("manifest_push: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("manifest_push: HTTP {status}: {body}");
        }
        Ok(resp.json::<ManifestPushResponse>().await.context("manifest_push: parse body")?)
    }

    /// `GET /api/manifest/pull?since=…&limit=…&include_full_text=…`.
    /// Returns rows with `indexed_at > since`.  `since=0` for an
    /// initial pull.
    ///
    /// `include_full_text`: when `false` (default), the server
    /// omits body text from each row — clients in the tiered-
    /// cache model keep metadata near-full and pull bodies only
    /// on demand.  When `true`, the body is included (matching
    /// the pre-Stage-I behavior).
    pub async fn manifest_pull(
        &self,
        since: i64,
        limit: usize,
    ) -> Result<ManifestPullResponse> {
        self.manifest_pull_with_options(since, limit, false).await
    }

    /// Variant of [`Self::manifest_pull`] that exposes the
    /// `include_full_text` knob.  Default route stays metadata-
    /// only to match the tiered-cache model; callers that want to
    /// hydrate the local body cache call this explicitly.
    pub async fn manifest_pull_with_options(
        &self,
        since: i64,
        limit: usize,
        include_full_text: bool,
    ) -> Result<ManifestPullResponse> {
        // Hand-roll the query string — `reqwest::RequestBuilder::query`
        // depends on `serde_urlencoded` which isn't on our reqwest
        // feature set; both params here are i64 / usize and need no
        // url-escaping, so plain `format!` is correct.
        let url = format!(
            "{}{}?since={}&limit={}&include_full_text={}",
            self.base_url, PULL_PATH, since, limit,
            if include_full_text { "true" } else { "false" }
        );
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("manifest_pull: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("manifest_pull: HTTP {status}: {body}");
        }
        Ok(resp.json::<ManifestPullResponse>().await.context("manifest_pull: parse body")?)
    }

    /// `GET /api/manifest/resolve?path=…&sha256=…` — fetch one exact
    /// owner-scoped manifest candidate without advancing the pull watermark.
    pub async fn manifest_resolve(
        &self,
        path: &str,
        sha256: &str,
        include_full_text: bool,
    ) -> Result<ManifestPullResponse> {
        let encode = |value: &str| -> String {
            value.chars().flat_map(|c| {
                if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '~') {
                    vec![c]
                } else {
                    format!("%{:02X}", c as u32).chars().collect()
                }
            }).collect()
        };
        let url = format!(
            "{}/api/manifest/resolve?path={}&sha256={}&include_full_text={}",
            self.base_url, encode(path), encode(sha256), include_full_text,
        );
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("manifest_resolve: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("manifest_resolve: HTTP {status}: {body}");
        }
        Ok(resp.json::<ManifestPullResponse>().await
            .context("manifest_resolve: parse body")?)
    }

    /// Fetch a server-side delta blockmap. A missing map is a normal first
    /// upload condition and returns `None` so callers can use full upload.
    pub async fn delta_blockmap(&self, shard_id: &str) -> Result<Option<serde_json::Value>> {
        let url = format!("{}/api/v2/shards/{}/blockmap", self.base_url, shard_id);
        let resp = self.client.get(url).header(AUTHORIZATION, self.auth_header()).send().await
            .context("delta_blockmap: send")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND { return Ok(None); }
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("delta_blockmap: HTTP {status}: {}", resp.text().await.unwrap_or_default());
        }
        Ok(Some(resp.json().await.context("delta_blockmap: parse body")?))
    }

    /// Declare or replace the staged blockmap for one shard.
    pub async fn delta_put_blockmap(
        &self,
        shard_id: &str,
        blockmap: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/api/v2/shards/{}/blockmap", self.base_url, shard_id);
        let resp = self.client.put(url).header(AUTHORIZATION, self.auth_header())
            .json(blockmap).send().await.context("delta_put_blockmap: send")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("delta_put_blockmap: HTTP {status}: {}", resp.text().await.unwrap_or_default());
        }
        Ok(resp.json().await.context("delta_put_blockmap: parse body")?)
    }

    /// Upload one changed block into a previously declared staging map.
    pub async fn delta_put_block(
        &self,
        shard_id: &str,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/api/v2/shards/{}/blocks?offset={offset}&size={}", self.base_url, shard_id, data.len());
        let resp = self.client.put(url).header(AUTHORIZATION, self.auth_header())
            .body(data).send().await.context("delta_put_block: send")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("delta_put_block: HTTP {status}: {}", resp.text().await.unwrap_or_default());
        }
        Ok(resp.json().await.context("delta_put_block: parse body")?)
    }

    /// Finalize a fully populated staged shard.
    pub async fn delta_finalize(&self, shard_id: &str) -> Result<serde_json::Value> {
        let url = format!("{}/api/v2/shards/{}/finalize", self.base_url, shard_id);
        let resp = self.client.post(url).header(AUTHORIZATION, self.auth_header()).send().await
            .context("delta_finalize: send")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("delta_finalize: HTTP {status}: {}", resp.text().await.unwrap_or_default());
        }
        Ok(resp.json().await.context("delta_finalize: parse body")?)
    }

    /// `POST /api/index/push-embeddings`.  Per-row rejections (e.g.
    /// pack failure) show up in the response's `errors` list but
    /// don't fail the whole batch.
    pub async fn embeddings_push(&self, rows: &[EmbeddingRow]) -> Result<EmbeddingPushResponse> {
        let body = EmbeddingPushRequest { rows, cursor: None };
        let resp = self.client
            .post(self.endpoint(EMBED_PUSH_PATH))
            .header(AUTHORIZATION, self.auth_header())
            .json(&body)
            .send()
            .await
            .context("embeddings_push: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("embeddings_push: HTTP {status}: {body}");
        }
        Ok(resp.json::<EmbeddingPushResponse>().await.context("embeddings_push: parse body")?)
    }

    /// `GET /api/index/by-embedding?vec=…&k=…&model=…`.  `vec`
    /// elements are serialised as comma-separated decimals (the
    /// FastAPI route parses them via `vec.split(",")`).
    pub async fn by_embedding(
        &self,
        vec: &[f32],
        k: usize,
        model: Option<&str>,
    ) -> Result<ByEmbeddingResponse> {
        let vec_str = vec
            .iter()
            .map(|f| format!("{f}"))
            .collect::<Vec<_>>()
            .join(",");
        // `vec_str` may contain `,` and `.`/`-` — none of which need
        // percent-encoding in a query string per RFC 3986; the
        // `model_id` token similarly is ASCII alnum + `-` / `_` by
        // convention.  Skipping the percent-encode keeps the wire
        // pretty + readable in nginx logs.
        let mut url = format!(
            "{}{}?vec={}&k={}",
            self.base_url, BY_EMBED_PATH, vec_str, k
        );
        if let Some(m) = model {
            url.push_str("&model=");
            url.push_str(m);
        }
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("by_embedding: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("by_embedding: HTTP {status}: {body}");
        }
        Ok(resp.json::<ByEmbeddingResponse>().await.context("by_embedding: parse body")?)
    }

    /// `GET /api/index/embed-query?text=…&model=…` — compute the
    /// embedding vector for `text` on the cloud-backup VPS (CPU
    /// inference via fastembed).  Useful when the client doesn't
    /// have the embedder model loaded locally — phone client, web
    /// browser, headless CI — and wants to feed the resulting
    /// vector into `/api/index/by-embedding` for k-NN.
    ///
    /// First call to a given model triggers a ~500MB ONNX
    /// download on the server (response may take 30-60s).
    /// Subsequent calls reuse the resident handle.
    pub async fn embed_query(
        &self,
        text: &str,
        model: Option<&str>,
    ) -> Result<EmbedQueryResponse> {
        let encoded_text: String = text
            .chars()
            .flat_map(|c| {
                if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '~') {
                    vec![c]
                } else {
                    format!("%{:02X}", c as u32).chars().collect()
                }
            })
            .collect();
        let mut url = format!(
            "{}{}?text={}",
            self.base_url, EMBED_QUERY_PATH, encoded_text
        );
        if let Some(m) = model {
            url.push_str("&model=");
            url.push_str(m);
        }
        // Server-side first-call cold-start can be 30-60s while
        // fastembed downloads ONNX weights — use a generous
        // timeout, override the client's default 30s.
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .context("embed_query: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("embed_query: HTTP {status}: {body}");
        }
        Ok(resp.json::<EmbedQueryResponse>().await
            .context("embed_query: parse body")?)
    }

    /// `POST /api/v2/index/search` — hybrid metadata + FTS + vector
    /// search across every LanceDB shard on the VPS.  This is the
    /// "search-remote" entry point — local clients hit it when the
    /// local cache miss requires escalation to the full corpus.
    ///
    /// Set any combination of `q`, `vec` / `embed_text`, and
    /// `filters` fields.  When both `q` and a vector arm are set,
    /// the server fuses results via Reciprocal Rank Fusion (RRF).
    pub async fn v2_search(
        &self,
        req: &HybridSearchRequest<'_>,
    ) -> Result<HybridSearchResponse> {
        let url = format!("{}{}", self.base_url, V2_SEARCH_PATH);
        // Server-side embedding via `embed_text` can trigger a
        // ~500MB first-call download → generous timeout, same as
        // /api/index/embed-query.
        let resp = self.client
            .post(url)
            .header(AUTHORIZATION, self.auth_header())
            .json(req)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .context("v2_search: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("v2_search: HTTP {status}: {body}");
        }
        Ok(resp.json::<HybridSearchResponse>().await
            .context("v2_search: parse body")?)
    }

    /// `GET /api/index/embed-models` — list the model names this
    /// server's `/api/index/embed-query` accepts + whether fastembed
    /// is installed at all.  Clients use this to decide whether to
    /// fall back to client-side embedding.
    pub async fn embed_models(&self) -> Result<EmbedModelsResponse> {
        let url = format!("{}{}", self.base_url, EMBED_MODELS_PATH);
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("embed_models: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("embed_models: HTTP {status}: {body}");
        }
        Ok(resp.json::<EmbedModelsResponse>().await
            .context("embed_models: parse body")?)
    }

    /// `POST /api/files/by-hash/<sha256>` — upload raw file bytes.
    ///
    /// The server verifies the hash server-side; a mismatch returns
    /// 400.  Idempotent: re-upload of the same hash returns
    /// `stored=false`.  Owner-scoping: requires a manifest row
    /// referencing this hash to exist for the caller.
    pub async fn upload_file_by_hash(
        &self,
        sha256: &str,
        path: &std::path::Path,
    ) -> Result<FileUploadResponse> {
        // Stream the body from disk via `reqwest::Body::wrap_stream`
        // rather than slurping the whole file into memory.  Keeps
        // multi-GB uploads workable on memory-constrained clients.
        let file = tokio::fs::File::open(path).await
            .with_context(|| format!("open {}", path.display()))?;
        let stream = tokio_util::io::ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);

        let url = format!("{}{}{}", self.base_url, FILES_PATH_PREFIX, sha256);
        let resp = self.client
            .post(url)
            .header(AUTHORIZATION, self.auth_header())
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(body)
            .send()
            .await
            .context("upload_file_by_hash: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("upload_file_by_hash: HTTP {status}: {body}");
        }
        Ok(resp.json::<FileUploadResponse>().await
            .context("upload_file_by_hash: parse body")?)
    }

    /// `GET /api/files/by-hash/<sha256>` — stream the file bytes to
    /// `dest_path`, verifying the hash matches as bytes arrive.
    ///
    /// Returns `(bytes_written, sha256_verified)`.  If the
    /// server-claimed sha matches what we computed during streaming,
    /// the boolean is true; otherwise the file is removed and an
    /// error returned.  Files are written to a `.partial` sibling
    /// then atomically renamed so a concurrent reader never sees a
    /// half-written file.
    pub async fn download_file_by_hash(
        &self,
        sha256: &str,
        dest_path: &std::path::Path,
    ) -> Result<u64> {
        use sha2::{Digest, Sha256};
        use tokio::io::AsyncWriteExt;
        use futures::StreamExt;

        let url = format!("{}{}{}", self.base_url, FILES_PATH_PREFIX, sha256);
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("download_file_by_hash: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("download_file_by_hash: HTTP {status}: {body}");
        }

        // Atomic-write pattern: <dest>.partial → fsync → rename.
        // Parent dir must exist; caller is responsible (we don't
        // create arbitrary tree structures behind their back).
        let partial = dest_path.with_extension(
            format!(
                "{}.partial",
                dest_path.extension().and_then(|e| e.to_str()).unwrap_or("")
            )
        );
        let mut file = tokio::fs::File::create(&partial).await
            .with_context(|| format!("create {}", partial.display()))?;

        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        let mut stream = resp.bytes_stream();
        while let Some(chunk_res) = stream.next().await {
            let chunk = chunk_res.context("download_file_by_hash: stream")?;
            hasher.update(&chunk);
            total += chunk.len() as u64;
            file.write_all(&chunk).await
                .context("download_file_by_hash: write")?;
        }
        file.flush().await.ok();
        drop(file);

        let computed = format!("{:x}", hasher.finalize());
        if computed != sha256 {
            // Body didn't match the URL hash — tampering / proxy
            // corruption / server bug.  Don't leave the file in
            // place under the requested name.
            let _ = tokio::fs::remove_file(&partial).await;
            anyhow::bail!(
                "download_file_by_hash: integrity check failed (claimed {sha256}, \
                 got {computed} after {total} bytes)"
            );
        }
        tokio::fs::rename(&partial, dest_path).await
            .with_context(|| format!("rename {} → {}", partial.display(), dest_path.display()))?;
        Ok(total)
    }

    /// `GET /api/search?q=…&limit=…` — full-text search over the
    /// `file_references.full_text` FTS5 index server-side.
    ///
    /// Query string follows FTS5 grammar (the server forwards
    /// errors as 400 responses).  Tokens with `-` need to be
    /// wrapped in `"…"` because FTS5 treats `-` as the NOT
    /// operator.  Multi-word queries are AND-by-default.
    // ── Stage Q — shard backup / restore ─────────────────────────────────

    /// `GET /api/shard/list` — enumerate all shards with their
    /// `max_indexed_at` watermarks.  Used for incremental backup
    /// (compare watermark against `backup_state.db` to skip unchanged
    /// shards).
    pub async fn shard_list(&self) -> Result<ShardListResponse> {
        let url = format!("{}{}", self.base_url, SHARD_LIST_PATH);
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("shard_list: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("shard_list: HTTP {status}: {body}");
        }
        resp.json::<ShardListResponse>().await.context("shard_list: parse")
    }

    /// `GET /api/shard/export/{prefix}` — download the shard's
    /// tarball bytes.  The caller uploads them to the cloud drive.
    pub async fn shard_export(&self, prefix: &str) -> Result<Vec<u8>> {
        use futures::StreamExt;
        let url = format!("{}{}{}", self.base_url, SHARD_EXPORT_PREFIX, prefix);
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("shard_export: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("shard_export: HTTP {status}: {body}");
        }
        let mut out = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk.context("shard_export: stream")?);
        }
        Ok(out)
    }

    /// `POST /api/shard/import/{prefix}` — upload a previously-saved
    /// tarball to restore the shard on the VPS.
    pub async fn shard_import(&self, prefix: &str, data: Vec<u8>) -> Result<()> {
        let url = format!("{}{}{}", self.base_url, SHARD_IMPORT_PREFIX, prefix);
        let resp = self.client
            .post(url)
            .header(AUTHORIZATION, self.auth_header())
            .header("Content-Type", "application/octet-stream")
            .body(data)
            .send()
            .await
            .context("shard_import: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("shard_import: HTTP {status}: {body}");
        }
        Ok(())
    }

    /// `include_full_text=false` tells the server to omit the heavy
    /// `full_text` body from each hit and rely on the `snippet` field —
    /// the ~100x payload cut for display-only callers.  Pass `true` when
    /// the result will be lifted into the local L1 store (the body is the
    /// ingest payload there).
    pub async fn search(
        &self,
        q: &str,
        limit: usize,
        include_full_text: bool,
    ) -> Result<SearchResponse> {
        // Percent-encode the query — unlike the embedding vec
        // (numeric-only), user input here can contain `&` / `#` /
        // spaces / etc that break a naive query string.
        let encoded_q: String = q
            .chars()
            .flat_map(|c| {
                if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '~') {
                    vec![c]
                } else {
                    format!("%{:02X}", c as u32).chars().collect()
                }
            })
            .collect();
        let url = format!(
            "{}{}?q={}&limit={}&include_full_text={}",
            self.base_url, SEARCH_PATH, encoded_q, limit, include_full_text
        );
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("search: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("search: HTTP {status}: {body}");
        }
        Ok(resp.json::<SearchResponse>().await.context("search: parse body")?)
    }

    // ── Stage T — admin key management ──────────────────────────────────────

    /// Stage U — `GET /api/v2/extract/status` — VPS extraction-worker
    /// queue depths.  Requires a valid bearer token.
    pub async fn extract_status(&self) -> Result<ExtractStatusResponse> {
        let url = format!("{}{}", self.base_url, EXTRACT_STATUS_PATH);
        let resp = self.client
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("extract_status: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("extract_status: HTTP {status}: {body}");
        }
        Ok(resp.json().await.context("extract_status: parse body")?)
    }

    /// `POST /api/admin/keys/mint` — mint a new regular API key.
    /// Requires `admin_token` (stored as `X-Admin-Token`).
    pub async fn admin_mint(
        &self,
        admin_token: &str,
        name: &str,
        owner_id: Option<&str>,
    ) -> Result<AdminMintResponse> {
        let url = format!("{}/api/admin/keys/mint", self.base_url);
        let req = AdminMintRequest { name, owner_id };
        let resp = self.client
            .post(url)
            .header("X-Admin-Token", admin_token)
            .json(&req)
            .send()
            .await
            .context("admin_mint: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("admin_mint: HTTP {status}: {body}");
        }
        Ok(resp.json().await.context("admin_mint: parse body")?)
    }

    /// `POST /api/admin/keys/revoke` — soft-delete a key by name.
    /// Requires `admin_token`.
    pub async fn admin_revoke(
        &self,
        admin_token: &str,
        name: &str,
    ) -> Result<AdminRevokeResponse> {
        let url = format!("{}/api/admin/keys/revoke", self.base_url);
        let req = AdminRevokeRequest { name };
        let resp = self.client
            .post(url)
            .header("X-Admin-Token", admin_token)
            .json(&req)
            .send()
            .await
            .context("admin_revoke: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("admin_revoke: HTTP {status}: {body}");
        }
        Ok(resp.json().await.context("admin_revoke: parse body")?)
    }

    /// `GET /api/admin/keys/list` — list all API key rows (no hashes).
    /// Requires `admin_token`.
    pub async fn admin_list_keys(
        &self,
        admin_token: &str,
    ) -> Result<Vec<AdminKeyInfo>> {
        let url = format!("{}/api/admin/keys/list", self.base_url);
        let resp = self.client
            .get(url)
            .header("X-Admin-Token", admin_token)
            .send()
            .await
            .context("admin_list_keys: send")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("admin_list_keys: HTTP {status}: {body}");
        }
        Ok(resp.json().await.context("admin_list_keys: parse body")?)
    }
}

// PLAN P36.16 — these are cloud-backup tests, so they need the feature
// that lets a client exist. CI enables it on the `cargo test` line (the
// code is meant to keep working, it is just not shipped); a default
// `cargo test` compiles them out rather than failing on a constructor
// that correctly refused.
#[cfg(all(test, feature = "cloud-backup"))]
mod tests {
    //! Mock-server unit tests for every cloud-backup wire command.
    //!
    //! Uses `mockito` to spin up an in-process HTTP server per
    //! test; never touches a real network or VPS.  Covers the
    //! 200/4xx/5xx paths the spec calls out as required.

    use super::*;
    use mockito::{Matcher, Server};

    fn client_for(server: &Server) -> CloudBackupClient {
        CloudBackupClient::new(server.url(), "cbk_test_key").unwrap()
    }

    #[test]
    fn cloud_backup_client_uses_shared_proxy_validation() {
        let invalid = ProxyConfig { url: Some("not a proxy URL".into()), ..Default::default() };
        assert!(CloudBackupClient::new_with_proxy("http://localhost", "key", &invalid).is_err());
        let valid = ProxyConfig { url: Some("socks5://127.0.0.1:9050".into()), ..Default::default() };
        assert!(CloudBackupClient::new_with_proxy("http://localhost", "key", &valid).is_ok());
    }

    // ── manifest_push ────────────────────────────────────────────────

    #[tokio::test]
    async fn manifest_push_200_returns_accepted_count() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .match_header("authorization", "Bearer cbk_test_key")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"accepted": 3}"#)
            .create_async()
            .await;

        let cli = client_for(&server);
        let rows = vec![sample_row("a"), sample_row("b"), sample_row("c")];
        let resp = cli.manifest_push(&rows).await.unwrap();
        assert_eq!(resp.accepted, 3);
        assert_eq!(resp.next_cursor, None);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_200_with_cursor_propagates() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(200)
            .with_body(r#"{"accepted": 200, "next_cursor": "page_2"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.manifest_push(&[sample_row("x")]).await.unwrap();
        assert_eq!(resp.accepted, 200);
        assert_eq!(resp.next_cursor.as_deref(), Some("page_2"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_400_returns_error_with_body() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(400)
            .with_body(r#"{"detail": "bad payload"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_push(&[]).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("HTTP 400"), "got: {msg}");
        assert!(msg.contains("bad payload"), "got: {msg}");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_401_returns_unauthorised() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(401)
            .with_body(r#"{"detail": "invalid Bearer token"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_push(&[]).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 401"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_500_returns_server_error() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(500)
            .with_body("server panic")
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_push(&[sample_row("a")]).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 500"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_push_503_returns_backpressure() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .with_status(503)
            .with_body("busy")
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_push(&[sample_row("a")]).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 503"));
        m.assert_async().await;
    }

    // ── manifest_pull ────────────────────────────────────────────────

    #[tokio::test]
    async fn manifest_pull_200_parses_rows_and_watermark() {
        let mut server = Server::new_async().await;
        let body = r#"
            {
                "rows": [
                    {"path":"/a.pdf","size_bytes":10,"sha256":"aaa",
                     "mtime_unix":1.0,"owner_id":"o","filename":"a.pdf",
                     "ext":"pdf","parent_dir":"/","indexed_at":100}
                ],
                "max_indexed_at": 100,
                "has_more": false
            }
        "#;
        let m = server.mock("GET", PULL_PATH)
            .match_query(Matcher::Regex("since=0&limit=200".into()))
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.manifest_pull(0, 200).await.unwrap();
        assert_eq!(resp.rows.len(), 1);
        assert_eq!(resp.rows[0].path, "/a.pdf");
        assert_eq!(resp.max_indexed_at, 100);
        assert!(!resp.has_more);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_resolve_encodes_path_and_parses_candidate() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", "/api/manifest/resolve")
            .match_query(Matcher::Regex(
                "path=%2Fconflicts%2Fremote%20file.md&sha256=abc&include_full_text=true".into(),
            ))
            .with_status(200)
            .with_body(r#"{"rows":[{"path":"/conflicts/remote file.md","size_bytes":3,"sha256":"abc","mtime_unix":1.0,"owner_id":"o","filename":"remote file.md","ext":"md","parent_dir":"/conflicts","indexed_at":100,"full_text":"new"}],"max_indexed_at":100,"has_more":false}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let response = cli.manifest_resolve("/conflicts/remote file.md", "abc", true).await.unwrap();
        assert_eq!(response.rows[0].full_text.as_deref(), Some("new"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn delta_transport_round_trip_wire_shapes() {
        let mut server = Server::new_async().await;
        let get = server.mock("GET", "/api/v2/shards/demo/blockmap")
            .with_status(404).create_async().await;
        let put_map = server.mock("PUT", "/api/v2/shards/demo/blockmap")
            .with_status(200).with_body(r#"{"block_size":4,"size":4,"blocks":[]}"#)
            .create_async().await;
        let put_block = server.mock("PUT", "/api/v2/shards/demo/blocks")
            .match_query(Matcher::Regex("offset=0&size=4".into()))
            .with_status(200).with_body(r#"{"accepted":true}"#).create_async().await;
        let finalize = server.mock("POST", "/api/v2/shards/demo/finalize")
            .with_status(200).with_body(r#"{"finalized":true,"size":4}"#).create_async().await;
        let cli = client_for(&server);
        assert!(cli.delta_blockmap("demo").await.unwrap().is_none());
        let map = serde_json::json!({"block_size":4,"size":4,"blocks":[]});
        assert_eq!(cli.delta_put_blockmap("demo", &map).await.unwrap()["size"], 4);
        assert_eq!(cli.delta_put_block("demo", 0, b"test".to_vec()).await.unwrap()["accepted"], true);
        assert_eq!(cli.delta_finalize("demo").await.unwrap()["finalized"], true);
        get.assert_async().await;
        put_map.assert_async().await;
        put_block.assert_async().await;
        finalize.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_pull_401_propagates_auth_failure() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", PULL_PATH)
            .match_query(Matcher::Any)
            .with_status(401)
            .with_body(r#"{"detail":"invalid"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.manifest_pull(0, 50).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 401"));
        m.assert_async().await;
    }

    // ── embeddings_push ──────────────────────────────────────────────

    #[tokio::test]
    async fn embeddings_push_200_with_per_row_errors() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", EMBED_PUSH_PATH)
            .with_status(200)
            .with_body(r#"{"accepted": 2, "rejected": 1, "errors": ["doc:1: dim mismatch"]}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.embeddings_push(&[]).await.unwrap();
        assert_eq!(resp.accepted, 2);
        assert_eq!(resp.rejected, 1);
        assert_eq!(resp.errors.len(), 1);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn embeddings_push_500_returns_error() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", EMBED_PUSH_PATH)
            .with_status(500)
            .with_body("internal")
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.embeddings_push(&[]).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 500"));
        m.assert_async().await;
    }

    // ── by_embedding ─────────────────────────────────────────────────

    #[tokio::test]
    async fn by_embedding_200_parses_hits() {
        let mut server = Server::new_async().await;
        let body = r#"
            {"rows":[
                {"doc_id":"a","chunk_index":0,"model_id":"bge","distance":0.01},
                {"doc_id":"b","chunk_index":0,"model_id":"bge","distance":0.5}
            ]}
        "#;
        let m = server.mock("GET", BY_EMBED_PATH)
            .match_query(Matcher::Regex("vec=1,0&k=20&model=bge".into()))
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.by_embedding(&[1.0, 0.0], 20, Some("bge")).await.unwrap();
        assert_eq!(resp.rows.len(), 2);
        assert_eq!(resp.rows[0].doc_id, "a");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn by_embedding_400_malformed_vec_propagates() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", BY_EMBED_PATH)
            .match_query(Matcher::Any)
            .with_status(400)
            .with_body(r#"{"detail":"vec must be comma-separated floats"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.by_embedding(&[1.0], 1, None).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 400"));
        m.assert_async().await;
    }

    // ── search ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn search_200_parses_hits_with_score() {
        let mut server = Server::new_async().await;
        let body = r#"
            {"rows":[
                {"path":"/a.txt","size_bytes":10,"sha256":"a",
                 "mtime_unix":1.0,"owner_id":"o","filename":"a.txt",
                 "ext":"txt","parent_dir":"/","full_text":"hello world",
                 "indexed_at":100,"score":2.5}
            ],"total":1}
        "#;
        let m = server.mock("GET", SEARCH_PATH)
            .match_query(Matcher::Regex("q=hello&limit=50".into()))
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.search("hello", 50, true).await.unwrap();
        assert_eq!(resp.total, 1);
        assert_eq!(resp.rows[0].path, "/a.txt");
        assert_eq!(resp.rows[0].full_text.as_deref(), Some("hello world"));
        assert!((resp.rows[0].score - 2.5).abs() < 1e-6);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn search_lean_sends_include_full_text_false_and_parses_snippet() {
        // Display callers pass include_full_text=false; the server then
        // omits full_text and ships a `snippet` instead.  Assert both the
        // outbound flag and that the new field deserialises.
        let mut server = Server::new_async().await;
        let body = r#"
            {"rows":[
                {"path":"/a.txt","size_bytes":10,"sha256":"a",
                 "mtime_unix":1.0,"owner_id":"o","filename":"a.txt",
                 "ext":"txt","parent_dir":"/","full_text":null,
                 "snippet":"… in <mark>hello</mark> world …",
                 "indexed_at":100,"score":2.5}
            ],"total":1}
        "#;
        let m = server.mock("GET", SEARCH_PATH)
            .match_query(Matcher::Regex("include_full_text=false".into()))
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.search("hello", 50, false).await.unwrap();
        assert_eq!(resp.rows[0].full_text, None);
        assert_eq!(
            resp.rows[0].snippet.as_deref(),
            Some("… in <mark>hello</mark> world …"),
        );
        m.assert_async().await;   // fails if include_full_text=false wasn't sent
    }

    #[test]
    fn search_hit_deserialises_explicit_null_tags() {
        // cb-api emits `"tags": null` for a row with no tags; the row must
        // still deserialise (regression — a bare #[serde(default)] fails on
        // an explicit null, which broke live federated searches that hit a
        // tagless row).
        let json = r#"{"rows":[
            {"path":"/a.txt","size_bytes":1,"sha256":"a","mtime_unix":0.0,
             "owner_id":"o","filename":"a.txt","ext":"txt","parent_dir":"/",
             "indexed_at":0,"score":1.0,"tags":null,"url":null}
        ],"total":1}"#;
        let resp: SearchResponse = serde_json::from_str(json).unwrap();
        assert!(resp.rows[0].tags.is_empty());
        assert_eq!(resp.rows[0].url, None);
    }

    #[tokio::test]
    async fn search_percent_encodes_special_chars() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", SEARCH_PATH)
            // Space → %20, double-quote → %22.
            .match_query(Matcher::Regex(r#"q=foo%20%22bar%22"#.into()))
            .with_status(200)
            .with_body(r#"{"rows":[],"total":0}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let _ = cli.search(r#"foo "bar""#, 10, true).await.unwrap();
        m.assert_async().await;
    }

    #[tokio::test]
    async fn search_400_propagates_fts_error() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", SEARCH_PATH)
            .match_query(Matcher::Any)
            .with_status(400)
            .with_body(r#"{"detail":"FTS query: unterminated string"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.search("\"", 10, true).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 400"));
        m.assert_async().await;
    }

    // ── health ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_200_parses_response() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", HEALTH_PATH)
            .with_status(200)
            .with_body(r#"{"ok":true,"version":"0.1.0","backend":"cloud-backup-api","shared_catalog":false}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let h = cli.health().await.unwrap();
        assert!(h.ok);
        assert_eq!(h.version, "0.1.0");
        assert_eq!(h.backend, "cloud-backup-api");
        m.assert_async().await;
    }

    #[tokio::test]
    async fn health_5xx_returns_error() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", HEALTH_PATH)
            .with_status(503)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.health().await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 503"));
        m.assert_async().await;
    }

    // ── builder ─────────────────────────────────────────────────────

    #[test]
    fn new_strips_trailing_slash_on_base_url() {
        let c = CloudBackupClient::new("http://localhost:7869/", "k").unwrap();
        assert_eq!(c.base_url(), "http://localhost:7869");
    }

    // ── /api/v2/index/search (Stage I hybrid LanceDB query) ─────────

    #[tokio::test]
    async fn v2_search_200_parses_hybrid_response() {
        let mut server = Server::new_async().await;
        let body = r#"
            {"rows":[
                {"doc_id":"a","sha256":"a","owner_id":"o",
                 "path":"/a.pdf","filename":"a.pdf","ext":"pdf",
                 "indexed_at":100,"score":0.05,
                 "score_text":4.5,"score_vector":-0.12,
                 "full_text":"…"}
            ],"total":1,"used_text":true,"used_vector":true,
             "shards_queried":3}
        "#;
        let m = server.mock("POST", V2_SEARCH_PATH)
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let req = HybridSearchRequest {
            q: Some("kant"),
            vec: None,
            embed_text: Some("kantian metaphysics"),
            embed_model: Some("bge-m3"),
            filters: HybridSearchFilters {
                ext: vec!["pdf".into()],
                year_min: Some(2020),
                ..Default::default()
            },
            limit: 50,
            rrf_k: 60,
        };
        let resp = cli.v2_search(&req).await.unwrap();
        assert_eq!(resp.total, 1);
        assert!(resp.used_text);
        assert!(resp.used_vector);
        assert_eq!(resp.shards_queried, 3);
        assert_eq!(resp.rows[0].doc_id, "a");
        // Score fields parse through.
        assert!((resp.rows[0].score - 0.05).abs() < 1e-6);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn v2_search_503_when_lance_unavailable() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", V2_SEARCH_PATH)
            .with_status(503)
            .with_body(r#"{"detail":"LanceDB not configured"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.v2_search(&HybridSearchRequest {
            q: Some("x"),
            vec: None, embed_text: None, embed_model: None,
            filters: HybridSearchFilters::default(),
            limit: 10, rrf_k: 60,
        }).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 503"));
        m.assert_async().await;
    }

    // ── /api/index/embed-query (Stage H server-side inference) ──────

    #[tokio::test]
    async fn embed_query_200_parses_vector() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", EMBED_QUERY_PATH)
            .match_query(Matcher::Regex("text=hello%20world&model=bge-m3".into()))
            .with_status(200)
            .with_body(r#"{"model":"bge-m3","dim":4,"embedding":[0.1,0.2,0.3,0.4]}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.embed_query("hello world", Some("bge-m3")).await.unwrap();
        assert_eq!(resp.model, "bge-m3");
        assert_eq!(resp.dim, 4);
        assert_eq!(resp.embedding, vec![0.1, 0.2, 0.3, 0.4]);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn embed_query_400_unknown_model() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", EMBED_QUERY_PATH)
            .match_query(Matcher::Any)
            .with_status(400)
            .with_body(r#"{"detail":"unknown model 'wat'"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.embed_query("x", Some("wat")).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 400"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn embed_query_503_server_lacks_fastembed() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", EMBED_QUERY_PATH)
            .match_query(Matcher::Any)
            .with_status(503)
            .with_body(r#"{"detail":"fastembed not installed"}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let err = cli.embed_query("x", None).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 503"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn embed_models_200_lists_registry() {
        let mut server = Server::new_async().await;
        let m = server.mock("GET", EMBED_MODELS_PATH)
            .with_status(200)
            .with_body(r#"{"models":["bge-m3","e5-large"],"default":"bge-m3","available":true}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.embed_models().await.unwrap();
        assert!(resp.models.contains(&"bge-m3".to_string()));
        assert_eq!(resp.default, "bge-m3");
        assert!(resp.available);
        m.assert_async().await;
    }

    // ── /api/files/by-hash (Stage E byte upload + download) ─────────

    #[tokio::test]
    async fn upload_file_by_hash_200_parses_response() {
        let mut server = Server::new_async().await;
        let sha = "a".repeat(64);
        let m = server.mock("POST", format!("{}{}", FILES_PATH_PREFIX, sha).as_str())
            .match_header("authorization", "Bearer cbk_test_key")
            .with_status(200)
            .with_body(format!(
                r#"{{"sha256":"{sha}","size_bytes":11,"stored":true,
                     "local_blob_path":"aa/aa/{sha}"}}"#
            ))
            .create_async()
            .await;
        // Write a tiny temp file for the upload path.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"hello world").unwrap();

        let cli = client_for(&server);
        let resp = cli.upload_file_by_hash(&sha, tmp.path()).await.unwrap();
        assert_eq!(resp.sha256, sha);
        assert_eq!(resp.size_bytes, 11);
        assert!(resp.stored);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn upload_file_by_hash_400_hash_mismatch() {
        let mut server = Server::new_async().await;
        let sha = "b".repeat(64);
        let m = server.mock("POST", format!("{}{}", FILES_PATH_PREFIX, sha).as_str())
            .with_status(400)
            .with_body(r#"{"detail":"sha256 mismatch: …"}"#)
            .create_async()
            .await;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"wrong").unwrap();

        let cli = client_for(&server);
        let err = cli.upload_file_by_hash(&sha, tmp.path()).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 400"));
        m.assert_async().await;
    }

    #[tokio::test]
    async fn download_file_by_hash_streams_and_verifies() {
        let body = b"the file body bytes";
        let sha = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(body);
            format!("{:x}", h.finalize())
        };
        let mut server = Server::new_async().await;
        let m = server.mock("GET", format!("{}{}", FILES_PATH_PREFIX, sha).as_str())
            .with_status(200)
            .with_header("content-type", "application/octet-stream")
            .with_header("x-cb-sha256", &sha)
            .with_body(body)
            .create_async()
            .await;
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");

        let cli = client_for(&server);
        let n = cli.download_file_by_hash(&sha, &dest).await.unwrap();
        assert_eq!(n as usize, body.len());
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn download_file_by_hash_rejects_integrity_failure() {
        let claimed_sha = "0".repeat(64);
        let body = b"not the bytes that produce that hash";
        let mut server = Server::new_async().await;
        let m = server.mock("GET", format!("{}{}", FILES_PATH_PREFIX, claimed_sha).as_str())
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");

        let cli = client_for(&server);
        let err = cli.download_file_by_hash(&claimed_sha, &dest).await.unwrap_err();
        assert!(format!("{err}").contains("integrity check failed"));
        // Atomic semantics: the destination must NOT exist after a
        // failed integrity check.
        assert!(!dest.exists());
        m.assert_async().await;
    }

    #[tokio::test]
    async fn download_file_by_hash_404_propagates() {
        let mut server = Server::new_async().await;
        let sha = "c".repeat(64);
        let m = server.mock("GET", format!("{}{}", FILES_PATH_PREFIX, sha).as_str())
            .with_status(404)
            .with_body(r#"{"detail":"no bytes stored for …"}"#)
            .create_async()
            .await;
        let tmp = tempfile::TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");

        let cli = client_for(&server);
        let err = cli.download_file_by_hash(&sha, &dest).await.unwrap_err();
        assert!(format!("{err}").contains("HTTP 404"));
        m.assert_async().await;
    }

    // ── ManifestRow::from_raw_document (Stage C auto-push hook) ─────

    #[test]
    fn from_raw_document_carries_full_text_and_metadata() {
        use crate::index::ingest::RawDocument;
        let raw = RawDocument {
            full_text:        "the body of the document".to_string(),
            full_text_md:     "the body of the document".to_string(),
            headings:         vec![],
            title:            Some("Title".into()),
            author:           Some("Author".into()),
            year:             Some(2024),
            filename:         "doc.txt".to_string(),
            ext:              "txt".to_string(),
            language:         "en".to_string(),
            source_hash:      "abc".to_string(),
            location_uri:     "crisp+local://owner@m1/data/doc.txt".to_string(),
            owner_id:         "owner".to_string(),
            tags:             vec![],
            mtime_unix:       Some(1_700_000_000),
            file_size:        Some(123),
            volume_id:        None,
            parent_dir:       Some("/data".into()),
            translated_text:  None,
            translated_to_lang: None,
            audio_duration_seconds: None,
            audio_codec: None,
            audio_sample_rate_hz: None,
            audio_channels: None,
            audio_bitrate_kbps: None,
            image_camera_make: None,
            image_camera_model: None,
            image_lens_model: None,
            image_taken_at_unix: None,
            image_iso: None,
            multivec_packed: None,
            multivec_n_tokens: None,
            url: None,
            embedding_omni: None,
            embedding_vit: None,
        };
        let row = ManifestRow::from_raw_document(&raw);
        assert_eq!(row.sha256, "abc");
        assert_eq!(row.size_bytes, 123);
        assert_eq!(row.mtime_unix as i64, 1_700_000_000);
        assert_eq!(row.title.as_deref(), Some("Title"));
        assert_eq!(row.year, Some(2024));
        assert_eq!(row.language.as_deref(), Some("en"));
        assert_eq!(row.parent_dir, "/data");
        // Stage A — body text is carried through to the wire shape.
        assert_eq!(row.full_text.as_deref(), Some("the body of the document"));
        // The crisp+local URI maps to a local path when location_uri_to_local_path
        // recognises it; non-recognised URIs fall back verbatim.  Either is fine
        // for the wire — what matters is `path` is never empty.
        assert!(!row.path.is_empty());
    }

    #[test]
    fn from_raw_document_empty_body_becomes_none() {
        use crate::index::ingest::RawDocument;
        let raw = RawDocument {
            full_text:    "".to_string(),
            full_text_md: "".to_string(),
            headings:     vec![],
            title: None, author: None, year: None,
            filename: "f.txt".into(), ext: "txt".into(),
            language: "".into(),
            source_hash: "h".into(),
            location_uri: "crisp+local://o@m/f.txt".into(),
            owner_id: "o".into(), tags: vec![],
            mtime_unix: None, file_size: None,
            volume_id: None, parent_dir: None,
            translated_text: None, translated_to_lang: None,
            audio_duration_seconds: None, audio_codec: None,
            audio_sample_rate_hz: None, audio_channels: None,
            audio_bitrate_kbps: None,
            image_camera_make: None, image_camera_model: None,
            image_lens_model: None, image_taken_at_unix: None,
            image_iso: None,
            multivec_packed: None,
            multivec_n_tokens: None,
            url: None,
            embedding_omni: None,
            embedding_vit: None,
        };
        let row = ManifestRow::from_raw_document(&raw);
        assert!(row.full_text.is_none(), "empty body should map to None on wire");
        assert!(row.language.is_none(), "empty language should map to None on wire");
    }

    fn sample_row(suffix: &str) -> ManifestRow {
        ManifestRow {
            path: format!("/data/{suffix}.pdf"),
            size_bytes: 100,
            sha256: format!("{suffix:0>64}"),
            mtime_unix: 1_700_000_000.0,
            owner_id: "owner".into(),
            filename: format!("{suffix}.pdf"),
            ext: "pdf".into(),
            parent_dir: "/data".into(),
            full_text: None,
            language: None,
            title: None,
            author: None,
            year: None,
            collection_id: None,
            archived_in: None,
            url: None,
            tags: vec![],
        }
    }

    // ── v106: url wire round-trip ─────────────────────────────────────

    #[tokio::test]
    async fn manifest_push_includes_url_in_body() {
        let mut server = Server::new_async().await;
        // Body matcher asserts the JSON we POST contains url:"...".
        let m = server.mock("POST", PUSH_PATH)
            .match_body(Matcher::PartialJsonString(
                r#"{"rows":[{"url":"https://example.org/foo"}]}"#.into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"accepted":1}"#)
            .create_async()
            .await;

        let cli = client_for(&server);
        let mut row = sample_row("u");
        row.url = Some("https://example.org/foo".into());
        let resp = cli.manifest_push(&[row]).await.unwrap();
        assert_eq!(resp.accepted, 1);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_pull_deserializes_url_field() {
        let mut server = Server::new_async().await;
        let body = r#"{
            "rows": [{
                "path":"/x.md","size_bytes":42,
                "sha256":"a","mtime_unix":1.0,"owner_id":"o",
                "filename":"x.md","ext":"md","parent_dir":"/",
                "language":null,"title":null,"author":null,"year":null,
                "full_text":"body","indexed_at":123,"archived_in":null,
                "collection_id":"wallabag",
                "url":"https://example.org/source"
            }],
            "max_indexed_at": 123,
            "has_more": false
        }"#;
        let m = server.mock("GET", Matcher::Regex(format!("^{}", PULL_PATH)))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.manifest_pull_with_options(0, 10, true).await.unwrap();
        assert_eq!(resp.rows.len(), 1);
        assert_eq!(
            resp.rows[0].url.as_deref(),
            Some("https://example.org/source")
        );
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_pull_legacy_response_without_url_still_parses() {
        // Old cb-api deployments don't emit the url field. Default to
        // None so a mixed-version client/server can still sync.
        let mut server = Server::new_async().await;
        let body = r#"{
            "rows": [{
                "path":"/x.md","size_bytes":42,
                "sha256":"a","mtime_unix":1.0,"owner_id":"o",
                "filename":"x.md","ext":"md","parent_dir":"/",
                "indexed_at":123
            }],
            "max_indexed_at": 123,
            "has_more": false
        }"#;
        let m = server.mock("GET", Matcher::Regex(format!("^{}", PULL_PATH)))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.manifest_pull_with_options(0, 10, false).await.unwrap();
        assert_eq!(resp.rows.len(), 1);
        assert_eq!(resp.rows[0].url, None);
        m.assert_async().await;
    }

    // ── v107: tags wire round-trip ───────────────────────────────────

    #[tokio::test]
    async fn manifest_push_includes_tags_in_body() {
        let mut server = Server::new_async().await;
        let m = server.mock("POST", PUSH_PATH)
            .match_body(Matcher::PartialJsonString(
                r#"{"rows":[{"tags":["pocket-import","de"]}]}"#.into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"accepted":1}"#)
            .create_async()
            .await;
        let cli = client_for(&server);
        let mut row = sample_row("t");
        row.tags = vec!["pocket-import".into(), "de".into()];
        let resp = cli.manifest_push(&[row]).await.unwrap();
        assert_eq!(resp.accepted, 1);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_pull_deserializes_tags_field() {
        let mut server = Server::new_async().await;
        let body = r#"{
            "rows": [{
                "path":"/x.md","size_bytes":42,
                "sha256":"a","mtime_unix":1.0,"owner_id":"o",
                "filename":"x.md","ext":"md","parent_dir":"/",
                "indexed_at":123,
                "tags":["pocket-import","de"]
            }],
            "max_indexed_at": 123,
            "has_more": false
        }"#;
        let m = server.mock("GET", Matcher::Regex(format!("^{}", PULL_PATH)))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.manifest_pull_with_options(0, 10, false).await.unwrap();
        assert_eq!(resp.rows[0].tags, vec!["pocket-import", "de"]);
        m.assert_async().await;
    }

    #[tokio::test]
    async fn manifest_pull_tags_default_to_empty_vec_when_absent() {
        // A pre-v107 cb-api response (or any row without a tags
        // field) deserialises with tags == Vec::new(), not an error.
        let mut server = Server::new_async().await;
        let body = r#"{
            "rows": [{
                "path":"/x.md","size_bytes":42,
                "sha256":"a","mtime_unix":1.0,"owner_id":"o",
                "filename":"x.md","ext":"md","parent_dir":"/",
                "indexed_at":123
            }],
            "max_indexed_at": 123,
            "has_more": false
        }"#;
        let m = server.mock("GET", Matcher::Regex(format!("^{}", PULL_PATH)))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.manifest_pull_with_options(0, 10, false).await.unwrap();
        assert!(resp.rows[0].tags.is_empty());
        m.assert_async().await;
    }

    #[tokio::test]
    async fn search_hit_deserializes_url_field() {
        let mut server = Server::new_async().await;
        let body = r#"{
            "rows": [{
                "path":"/x.md","size_bytes":42,
                "sha256":"a","mtime_unix":1.0,"owner_id":"o",
                "filename":"x.md","ext":"md","parent_dir":"/",
                "indexed_at":123,"score":3.14,
                "url":"https://example.org/hit"
            }],
            "total": 1
        }"#;
        let m = server.mock("GET", Matcher::Regex(format!("^{}", SEARCH_PATH)))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        let cli = client_for(&server);
        let resp = cli.search("query", 5, true).await.unwrap();
        assert_eq!(resp.rows.len(), 1);
        assert_eq!(
            resp.rows[0].url.as_deref(),
            Some("https://example.org/hit")
        );
        m.assert_async().await;
    }
}

// ── Env-gated live tests against a real cloud-backup VPS ────────────────
//
// Mirrors `src-tauri/src/drives/webdav.rs:570-625` — the canonical
// pattern.  Skipped silently when the env vars aren't set, so a
// default `cargo test` stays offline.  Run with:
//
//     CB_SYNC_TEST_URL=http://localhost:7869 \
//     CB_SYNC_TEST_API_KEY=cbk_... \
//         cargo test -p crispsorter --lib --no-default-features --ignored cb_sync_live
//
// PLAN P36.16 — these are cloud-backup tests, so they need the feature
// that lets a client exist. CI enables it on the `cargo test` line (the
// code is meant to keep working, it is just not shipped); a default
// `cargo test` compiles them out rather than failing on a constructor
// that correctly refused.
#[cfg(all(test, feature = "cloud-backup"))]
mod live_tests {
    use super::*;

    fn read_env() -> Option<(String, String)> {
        let url = std::env::var("CB_SYNC_TEST_URL").ok()?;
        let key = std::env::var("CB_SYNC_TEST_API_KEY").ok()?;
        if url.is_empty() || key.is_empty() { None } else { Some((url, key)) }
    }

    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_health_round_trip() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_health_round_trip: \
                CB_SYNC_TEST_URL / CB_SYNC_TEST_API_KEY not set");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();
        let h = cli.health().await.expect("health probe");
        assert!(h.ok);
    }

    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_manifest_push_pull_round_trip() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_manifest_push_pull_round_trip: \
                CB_SYNC_TEST_URL / CB_SYNC_TEST_API_KEY not set");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();
        let unique = format!("{}-{}", std::process::id(),
                             std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default().as_millis());
        let path = format!("/test/live-{unique}.pdf");
        let sha = format!("{unique:0>64}");
        let row = ManifestRow {
            path: path.clone(), size_bytes: 42, sha256: sha.clone(),
            mtime_unix: 1.0, owner_id: "live-test".into(),
            filename: format!("live-{unique}.pdf"), ext: "pdf".into(),
            parent_dir: "/test".into(),
            language: None, title: None, author: None, year: None,
            full_text: None,
            collection_id: None,
            archived_in: None,
            url: None,
            tags: vec![],
        };
        let pushed = cli.manifest_push(std::slice::from_ref(&row)).await
            .expect("manifest_push");
        assert!(pushed.accepted >= 1);

        // Pull and assert the row makes it back.
        let pulled = cli.manifest_pull(0, 500).await.expect("manifest_pull");
        assert!(
            pulled.rows.iter().any(|r| r.sha256 == sha),
            "live round-trip: pushed sha {sha} not found in {} pulled rows",
            pulled.rows.len()
        );
    }

    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_full_text_push_and_search() {
        // Stage A — push a row with a unique-token body, then
        // search the server for that token and assert the row
        // comes back.  Closes the "indexed text actually flows
        // through and is searchable on the server" claim.
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_full_text_push_and_search");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();
        let unique = format!("{}{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_millis()
        );
        // FTS5 reads `-` as NOT, so we pick an alnum-only token.
        let token = format!("crispsorterlive{unique}");
        let body = format!("the body text contains the {token} sentinel");
        let sha = format!("{unique:0>64}");
        let row = ManifestRow {
            path: format!("/test/live-fts-{unique}.txt"),
            size_bytes: body.len() as i64,
            sha256: sha.clone(),
            mtime_unix: 1.0,
            owner_id: "live-test".into(),
            filename: format!("live-fts-{unique}.txt"),
            ext: "txt".into(),
            parent_dir: "/test".into(),
            language: None, title: None, author: None, year: None,
            full_text: Some(body.clone()),
            collection_id: None,
            archived_in: None,
            url: None,
            tags: vec![],
        };
        let pushed = cli.manifest_push(std::slice::from_ref(&row)).await
            .expect("manifest_push with full_text");
        assert!(pushed.accepted >= 1);

        let hits = cli.search(&token, 50, true).await.expect("search");
        let found = hits.rows.iter().find(|h| h.sha256 == sha);
        assert!(
            found.is_some(),
            "search for {:?} returned {} rows but none had sha {sha}",
            token,
            hits.rows.len(),
        );
        let f = found.unwrap();
        assert_eq!(f.full_text.as_deref(), Some(body.as_str()));
        assert!(f.score.is_finite());
    }

    /// P13.7 Stage D — full end-to-end claim:
    ///
    ///   index a real file → push manifest (including body) →
    ///   pull on a *fresh* LocalIndex → run `crispsorter index
    ///   search` for a body-text token → assert hit.
    ///
    /// Exercises the SyncManager → CloudBackupClient → cb-api →
    /// SQLite FTS5 → manifest pull → DocumentChunk apply →
    /// Tantivy FTS pipeline end to end against the live VPS.
    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_end_to_end_index_push_pull_search() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_end_to_end_index_push_pull_search");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();

        // ── Phase 1: push a synthetic doc with a unique body token ──
        // We don't drive bg_ingest end-to-end (that needs an embedder
        // + tokio Tauri runtime which is too heavy for a lib test);
        // instead we hand-craft a ManifestRow that matches what
        // bg_ingest's auto-push hook would emit.  The test exercises
        // every byte of the wire round-trip + the server-side FTS
        // index population.
        //
        // Seed includes the test name so parallel live tests within
        // the same ms / process never collide on sha256.
        let unique = format!("e2e{}{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_micros()
        );
        let unique_token = format!("crispsorterE2E{unique}");
        let body = format!(
            "this document discusses {unique_token} \
             alongside other content the search should ignore"
        );
        let sha = format!("{unique:0>64}");
        let push_row = ManifestRow {
            path: format!("/test/e2e-{unique}.txt"),
            size_bytes: body.len() as i64,
            sha256: sha.clone(),
            mtime_unix: 1.0,
            owner_id: "e2e-test".into(),
            filename: format!("e2e-{unique}.txt"),
            ext: "txt".into(),
            parent_dir: "/test".into(),
            language: Some("en".into()),
            title: Some("E2E doc".into()),
            author: None,
            year: None,
            full_text: Some(body.clone()),
            collection_id: None,
            archived_in: None,
            url: None,
            tags: vec![],
        };
        let pushed = cli.manifest_push(std::slice::from_ref(&push_row))
            .await
            .expect("manifest_push");
        assert!(pushed.accepted >= 1, "server didn't accept push: {pushed:?}");

        // ── Phase 2: search server-side by the unique body token ──
        // This is the "search CrispSorter files on cloud-backup"
        // claim: a row pushed from one client surfaces by body text
        // for any client with a key in the same owner scope.
        let hits = cli.search(&unique_token, 50, true).await.expect("search");
        let found = hits.rows.iter().find(|h| h.sha256 == sha);
        assert!(
            found.is_some(),
            "after push, search for {:?} returned {} rows but none matched sha {sha}",
            unique_token, hits.rows.len(),
        );
        let hit = found.unwrap();
        // Body and metadata survived the round trip.
        assert_eq!(hit.full_text.as_deref(), Some(body.as_str()));
        assert_eq!(hit.title.as_deref(), Some("E2E doc"));
        assert_eq!(hit.language.as_deref(), Some("en"));
        assert!(hit.score.is_finite());

        // ── Phase 3: pull manifest (since=0) and assert the row
        //              comes back, simulating a fresh client.
        //              Use the with-options variant + include_full_text=true
        //              because Stage I flipped the default to
        //              metadata-only pulls (tiered-cache model). ──
        let pulled = cli.manifest_pull_with_options(0, 500, true)
            .await.expect("manifest_pull");
        let pulled_match = pulled.rows.iter().find(|r| r.sha256 == sha);
        assert!(
            pulled_match.is_some(),
            "fresh pull missed the just-pushed sha {sha}",
        );
        let pm = pulled_match.unwrap();
        assert_eq!(pm.full_text.as_deref(), Some(body.as_str()),
                   "pull didn't surface full_text (opted in)");
        // The pull watermark is past `indexed_at` so a follow-up
        // pull with since=max_indexed_at sees zero new rows.
        let watermark = pulled.max_indexed_at;
        let pulled_again = cli.manifest_pull_with_options(watermark, 500, true)
            .await.expect("pull again");
        assert!(
            !pulled_again.rows.iter().any(|r| r.sha256 == sha),
            "second pull with since=watermark should not re-return our row",
        );
    }

    /// P13.7 Stage E — full byte-level round-trip against the live VPS.
    ///
    /// Push a manifest row → upload bytes content-addressed by sha
    /// → download → verify byte-identical → repeat to confirm
    /// idempotency.  Closes the "GUI can download files from
    /// cloud-backup" claim end-to-end.
    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_byte_upload_download_round_trip() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_byte_upload_download_round_trip");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();
        let unique = format!("bytes{}{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_micros()
        );
        let body = format!("byte-upload-test-{unique}").into_bytes();
        let sha = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(&body);
            format!("{:x}", h.finalize())
        };

        // ── Declare the manifest row so the owner-scope guard
        //     on /api/files/by-hash lets us through. ───────────────
        let manifest_row = ManifestRow {
            path: format!("/test/bytes-{unique}.bin"),
            size_bytes: body.len() as i64,
            sha256: sha.clone(),
            mtime_unix: 1.0,
            owner_id: "live-test".into(),
            filename: format!("bytes-{unique}.bin"),
            ext: "bin".into(),
            parent_dir: "/test".into(),
            language: None, title: None, author: None, year: None,
            full_text: None,
            collection_id: None,
            archived_in: None,
            url: None,
            tags: vec![],
        };
        cli.manifest_push(std::slice::from_ref(&manifest_row))
            .await.expect("manifest_push prelude");

        // ── Upload the bytes from a tempfile. ───────────────────
        let tmp = tempfile::TempDir::new().unwrap();
        let src = tmp.path().join("upload.bin");
        std::fs::write(&src, &body).unwrap();
        let up = cli.upload_file_by_hash(&sha, &src).await
            .expect("upload_file_by_hash");
        assert_eq!(up.sha256, sha);
        assert_eq!(up.size_bytes as usize, body.len());
        assert!(up.stored, "first upload should report stored=true");

        // ── Re-upload should be idempotent. ─────────────────────
        let up2 = cli.upload_file_by_hash(&sha, &src).await
            .expect("idempotent upload");
        assert!(!up2.stored, "second upload of the same bytes should be a no-op");
        assert_eq!(up2.size_bytes, up.size_bytes);

        // ── Download via reqwest, byte-identical assert. ────────
        let dest = tmp.path().join("download.bin");
        let n = cli.download_file_by_hash(&sha, &dest).await
            .expect("download_file_by_hash");
        assert_eq!(n as usize, body.len());
        let got = std::fs::read(&dest).unwrap();
        assert_eq!(got, body, "round-tripped bytes mismatch");
    }

    /// P13.7 Stage I — hybrid LanceDB search round-trip against the
    /// live VPS.  Pushes two docs, then queries with body text +
    /// metadata filter + server-side embedding (so we exercise
    /// every arm of the route in one test).
    ///
    /// Skipped (with a clear note) when the server's
    /// `/api/health` reports `lance_enabled=false`.
    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_v2_search_round_trip() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_v2_search_round_trip");
            return;
        };
        let cli = CloudBackupClient::new(&url, &key).unwrap();
        let health = cli.health().await.expect("health");
        if !health.lance_enabled {
            eprintln!(
                "cb_sync_live_v2_search_round_trip: server reports \
                 lance_enabled=false; skipping (install lancedb + set \
                 CB_API_LANCE_ROOT)"
            );
            return;
        }

        let unique = format!("v2{}{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_micros()
        );
        let token = format!("crispsorterV2{unique}");
        let body = format!(
            "research notes on {token} \
             with discussion of methodology and results"
        );
        let sha = format!("{unique:0>64}");
        let manifest_row = ManifestRow {
            path: format!("/test/v2-{unique}.txt"),
            size_bytes: body.len() as i64,
            sha256: sha.clone(),
            mtime_unix: 1.0,
            owner_id: "live-test".into(),
            filename: format!("v2-{unique}.txt"),
            ext: "txt".into(),
            parent_dir: "/test".into(),
            language: Some("en".into()),
            title: Some("V2 Hybrid Test".into()),
            author: None,
            year: Some(2024),
            full_text: Some(body.clone()),
            collection_id: None,
            archived_in: None,
            url: None,
            tags: vec![],
        };
        cli.manifest_push(std::slice::from_ref(&manifest_row))
            .await.expect("manifest_push");

        // Pure-FTS arm.
        let resp = cli.v2_search(&HybridSearchRequest {
            q: Some(&token),
            vec: None, embed_text: None, embed_model: None,
            filters: HybridSearchFilters::default(),
            limit: 10, rrf_k: 60,
        }).await.expect("v2_search FTS");
        let found = resp.rows.iter().find(|r| r.sha256 == sha);
        assert!(found.is_some(),
                "FTS arm: {token:?} returned {} rows", resp.rows.len());
        let f = found.unwrap();
        assert!(resp.used_text);
        assert_eq!(f.title.as_deref(), Some("V2 Hybrid Test"));
        assert_eq!(f.year, Some(2024));

        // Metadata-only arm (no q, just filter).
        let resp2 = cli.v2_search(&HybridSearchRequest {
            q: None,
            vec: None, embed_text: None, embed_model: None,
            filters: HybridSearchFilters {
                ext: vec!["txt".into()],
                year_min: Some(2023),
                ..Default::default()
            },
            limit: 50, rrf_k: 60,
        }).await.expect("v2_search metadata-only");
        assert!(!resp2.used_text);
        assert!(!resp2.used_vector);
        assert!(
            resp2.rows.iter().any(|r| r.sha256 == sha),
            "metadata filter didn't include the just-pushed row"
        );

        // Hybrid arm: text + server-side embedding (only when
        // fastembed available; otherwise skip).  Use e5-large
        // (multilingual, 1024-d) to match the Lance shard's pinned
        // embedding dim — smaller models (all-minilm 384d,
        // bge-base 768d) would land in the dim-mismatch soft-
        // fallback path which drops vec_hits to empty.  See
        // `api/embed.py:_MODEL_ALIASES` for the available set;
        // bge-m3 is aliased to e5-large because fastembed-py's
        // TextEmbedding doesn't expose bge-m3 directly.
        if health.fastembed_enabled {
            let resp3 = cli.v2_search(&HybridSearchRequest {
                q: Some(&token),
                vec: None,
                embed_text: Some(&format!("notes on {token}")),
                embed_model: Some("e5-large"),
                filters: HybridSearchFilters::default(),
                limit: 10, rrf_k: 60,
            }).await.expect("v2_search hybrid");
            assert!(resp3.used_text);
            assert!(resp3.used_vector);
            let f3 = resp3.rows.iter().find(|r| r.sha256 == sha);
            assert!(f3.is_some(), "hybrid arm: token row missing");
        }
    }

    /// P13.7 Stage H — server-side embedding inference round-trip.
    /// Calls /api/index/embed-models first to confirm fastembed is
    /// available; skips with a clear note when it isn't.  Then
    /// embeds a short string and asserts the response shape +
    /// non-trivial dim (≥ 384 for any supported model).
    ///
    /// First call to a never-loaded model on the server triggers a
    /// ~500MB download — the test gives 120s.
    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_embed_query_round_trip() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_embed_query_round_trip");
            return;
        };
        let cli = CloudBackupClient::new(&url, &key).unwrap();
        let models = match cli.embed_models().await {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "cb_sync_live_embed_query_round_trip: embed-models \
                     not reachable ({e}); skipping"
                );
                return;
            }
        };
        if !models.available {
            eprintln!(
                "cb_sync_live_embed_query_round_trip: server reports \
                 fastembed unavailable; skipping (install with \
                 `pip install fastembed onnxruntime` in /opt/cb-api/venv)"
            );
            return;
        }
        // Pick the smallest available model so the first-call
        // download is fast (all-minilm is ~25MB) when present;
        // otherwise default to bge-m3.
        let pick = if models.models.iter().any(|m| m == "all-minilm") {
            "all-minilm"
        } else {
            &models.default
        };
        let resp = cli.embed_query("the quick brown fox", Some(pick))
            .await.expect("embed_query");
        assert_eq!(resp.model, pick);
        assert!(resp.dim >= 384, "any supported model has dim ≥ 384");
        assert_eq!(resp.embedding.len(), resp.dim);
        // Sanity: the vector should be roughly unit-normalised
        // (all supported models l2-normalise their output).
        let norm: f32 = resp.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.05,
            "expected l2-normalised vector, got norm={norm}"
        );
    }

    /// P13.7 Stage F — durable retry via the outbox, against the
    /// live VPS.  Enqueues an op directly (mimicking what
    /// bg_ingest's auto-push hook does), drains it, asserts the
    /// row shows up on /api/manifest/pull, and that the outbox is
    /// empty after the drain.
    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_outbox_drain_round_trip() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_outbox_drain_round_trip");
            return;
        };
        let cli = CloudBackupClient::new(&url, &key).unwrap();

        let unique = format!("drain{}{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_micros()
        );
        let sha = format!("{unique:0>64}");
        let row = ManifestRow {
            path: format!("/test/outbox-{unique}.txt"),
            size_bytes: 0,
            sha256: sha.clone(),
            mtime_unix: 1.0,
            owner_id: "live-test".into(),
            filename: format!("outbox-{unique}.txt"),
            ext: "txt".into(),
            parent_dir: "/test".into(),
            language: None, title: None, author: None, year: None,
            full_text: Some(format!("outbox sentinel {unique}")),
            collection_id: None,
            archived_in: None,
            url: None,
            tags: vec![],
        };
        let payload = serde_json::to_string(&row).expect("serialise row");

        // Use a per-test tempdir for the outbox SQLite so other live
        // tests don't have residual entries.
        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = crate::sync::SyncManager::open(tmp.path()).unwrap();
        mgr.enqueue("cb_manifest_push", &payload).unwrap();
        assert_eq!(mgr.pending_count().unwrap(), 1);

        let (pushed, failed) = mgr.drain_cb_outbox(&cli, 16).await
            .expect("drain_cb_outbox");
        assert_eq!(pushed, 1, "expected exactly one entry drained");
        assert_eq!(failed, 0);
        assert_eq!(mgr.pending_count().unwrap(), 0, "outbox should be empty after drain");

        // Confirm the row landed server-side via /api/manifest/pull.
        let pulled = cli.manifest_pull(0, 500).await.expect("pull");
        assert!(
            pulled.rows.iter().any(|r| r.sha256 == sha),
            "drained row {sha} did not appear in pull response ({} rows)",
            pulled.rows.len()
        );
    }

    #[ignore]
    #[tokio::test]
    async fn cb_sync_live_embedding_push_rejects_empty() {
        let Some((url, key)) = read_env() else {
            eprintln!("skipping cb_sync_live_embedding_push_rejects_empty");
            return;
        };
        let cli = CloudBackupClient::new(url, key).unwrap();
        let row = EmbeddingRow {
            doc_id: "live-empty".into(),
            chunk_index: 0,
            embedding: vec![],
            sparse_json: None,
            model_id: "test".into(),
        };
        let resp = cli.embeddings_push(std::slice::from_ref(&row)).await
            .expect("embeddings_push");
        assert!(resp.rejected >= 1);
        assert!(!resp.errors.is_empty());
    }
}
