/// HTTP client for the self-hosted `crisp-index-server` VPS backend.
///
/// Implements `IndexBackend` so the rest of the app is agnostic about whether
/// it talks to a local LanceDB or a remote Axum server.
///
/// ### Wire format
///
/// POST /v1/ingest
///   body  → IngestPayload (includes pre-computed embedding)
///   200   ← { chunk_count, write_time_ms }
///
/// POST /v1/search
///   body  → SearchPayload (text query + optional pre-computed embedding)
///   200   ← Vec<SearchResult>
///
/// POST /v1/docs/:id/location
///   body  → { new_uri }
///   200   ← {}
///
/// DELETE /v1/docs/:id
///   200   ← { deleted: true }
///
/// GET /v1/stats
///   200   ← { row_count, doc_count }
///
/// GET /health  (no auth)
///   200   ← { status: "ok" }
///
/// All authenticated requests carry `Authorization: Bearer <api_key>`.
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;

use super::schema::{SearchFilters, SearchResult};
use super::{DocumentChunk, IndexBackend};

// ── Request payload types ─────────────────────────────────────────────────

/// Sent for every indexed chunk.  The embedding is pre-computed on the client
/// side so the server does not need a GPU or fastembed.
#[derive(Debug, Serialize)]
struct IngestPayload<'a> {
    doc_id: &'a str,
    chunk_index: i32,
    full_text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_text_md: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headings: Option<&'a [String]>,
    embedding: &'a [f32], // pre-computed dense vector
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    year: Option<i32>,
    filename: &'a str,
    ext: &'a str,
    language: &'a str,
    location_uri: &'a str,
    owner_id: &'a str,
    source_hash: &'a str,
    tags: &'a [String],
}

/// Search request — one of three modes.
/// `embedding` is required for `mode = vector | hybrid`.
#[derive(Debug, Serialize)]
struct SearchPayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    embedding: Option<&'a [f32]>,
    mode: &'a str, // "text" | "vector" | "hybrid"
    limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    year_min: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    year_max: Option<i32>,
}

#[derive(Debug, Serialize)]
struct UpdateLocationPayload<'a> {
    new_uri: &'a str,
}

// ── RemoteClient ──────────────────────────────────────────────────────────────

pub struct RemoteClient {
    base_url: String,
    api_key: String,
    client: Client,
}

impl RemoteClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        RemoteClient {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            client: Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Result<reqwest::Response> {
        let resp = self
            .client
            .post(self.url(path))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("remote index {status}: {text}"));
        }
        Ok(resp)
    }
}

// ── IndexBackend impl ─────────────────────────────────────────────────────────

#[async_trait]
impl IndexBackend for RemoteClient {
    /// Send a pre-embedded chunk to the server for storage.
    ///
    /// The embedding must be populated before calling this (by the local
    /// `Embedder`).  If the embedding is missing the server will reject the
    /// request with 422.
    async fn ingest(&self, doc: DocumentChunk) -> Result<()> {
        let embedding = doc.embedding.as_deref().unwrap_or(&[]);
        let payload = IngestPayload {
            doc_id: &doc.doc_id,
            chunk_index: doc.chunk_index,
            full_text: doc.full_text.as_deref().unwrap_or(""),
            full_text_md: doc.full_text_md.as_deref(),
            headings: None, // headings stored in full_text_md already
            embedding,
            title: doc.title.as_deref(),
            author: doc.author.as_deref(),
            year: doc.year,
            filename: doc.filename.as_deref().unwrap_or(""),
            ext: doc.ext.as_deref().unwrap_or(""),
            language: doc.language.as_deref().unwrap_or(""),
            location_uri: &doc.location_uri,
            owner_id: &doc.owner_id,
            source_hash: &doc.source_hash,
            tags: &doc.tags,
        };
        self.post_json("/v1/ingest", &payload).await?;
        Ok(())
    }

    async fn search_text(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = SearchPayload {
            query: Some(query),
            embedding: None,
            mode: "text",
            limit,
            owner_id: filters.owner_id.as_deref(),
            language: filters.language.as_deref(),
            year_min: filters.year_min,
            year_max: filters.year_max,
        };
        let resp = self.post_json("/v1/search", &payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
    }

    async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = SearchPayload {
            query: None,
            embedding: Some(embedding),
            mode: "vector",
            limit,
            owner_id: filters.owner_id.as_deref(),
            language: filters.language.as_deref(),
            year_min: filters.year_min,
            year_max: filters.year_max,
        };
        let resp = self.post_json("/v1/search", &payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
    }

    async fn search_hybrid(
        &self,
        query: &str,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = SearchPayload {
            query: Some(query),
            embedding: Some(embedding),
            mode: "hybrid",
            limit,
            owner_id: filters.owner_id.as_deref(),
            language: filters.language.as_deref(),
            year_min: filters.year_min,
            year_max: filters.year_max,
        };
        let resp = self.post_json("/v1/search", &payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
    }

    async fn delete_doc(&self, doc_id: &str) -> Result<()> {
        let resp = self
            .client
            .delete(self.url(&format!("/v1/docs/{doc_id}")))
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("delete_doc {doc_id} failed {status}: {text}"));
        }
        Ok(())
    }

    async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()> {
        let payload = UpdateLocationPayload { new_uri };
        self.post_json(&format!("/v1/docs/{doc_id}/location"), &payload)
            .await?;
        Ok(())
    }
}
