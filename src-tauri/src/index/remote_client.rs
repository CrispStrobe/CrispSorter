/// HTTP client for the self-hosted `crisp-index-server` VPS backend.
///
/// Implements `IndexBackend` so the rest of the app is agnostic about whether
/// it talks to a local LanceDB or a remote Axum server.
///
/// ### Wire format
///
/// All wire types (`IngestChunk`, `SearchRequest`, `UpdateLocationBody`,
/// etc.) live in the `crisp-index-protocol` workspace crate so that the
/// server (../crisp-index-server) and this client cannot drift. The
/// response shape for `/v1/search` is `Vec<SearchHit>` on the wire — we
/// deserialize directly into the richer client-side `SearchResult`,
/// which is a strict superset (extra optional fields default to `None`).
///
/// POST /v1/ingest                    crisp_index_protocol::IngestChunk
///   200 ← IngestResponse
/// POST /v1/search                    crisp_index_protocol::SearchRequest
///   200 ← Vec<SearchHit>             (deserialized as Vec<SearchResult>)
/// POST /v1/docs/:id/location         crisp_index_protocol::UpdateLocationBody
///   200 ← UpdateLocationResponse
/// POST /v1/docs/location/by-uri      crisp_index_protocol::UpdateLocationByUriBody
///   200 ← UpdateLocationResponse
/// DELETE /v1/docs/:id
///   200 ← DeleteResponse
/// GET  /v1/stats
///   200 ← StatsResponse
/// GET  /health  (no auth)
///   200 ← HealthResponse
///
/// All authenticated requests carry `Authorization: Bearer <api_key>`.
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use crisp_index_protocol::{
    BatchIngestResponse, IngestBatch, IngestChunk, SearchRequest, TaskStatusResponse,
    UpdateLocationBody, UpdateLocationByUriBody,
};
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;

use super::schema::{SearchFilters, SearchResult};
use crate::sync::proxy::ProxyConfig;
use super::{DocumentChunk, IndexBackend};

// ── RemoteClient ──────────────────────────────────────────────────────────────

pub struct RemoteClient {
    base_url: String,
    api_key: String,
    client: Client,
}

impl RemoteClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::new_with_proxy(base_url, api_key, &ProxyConfig::default())
            .expect("default remote HTTP client must build")
    }

    /// Construct the remote index client with the shared proxy policy.
    pub fn new_with_proxy(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        proxy: &ProxyConfig,
    ) -> Result<Self> {
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            client: crate::sync::proxy::build_async_client_with_timeout(proxy, Duration::from_secs(30))?,
        })
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

    async fn get(&self, path: &str) -> Result<reqwest::Response> {
        let resp = self
            .client
            .get(self.url(path))
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("remote index {status}: {text}"));
        }
        Ok(resp)
    }

    pub async fn ingest_batch(&self, batch: &IngestBatch) -> Result<BatchIngestResponse> {
        let resp = self.post_json("/v1/ingest/batch", batch).await?;
        Ok(resp.json::<BatchIngestResponse>().await?)
    }

    pub async fn task_status(&self, task_id: &str) -> Result<TaskStatusResponse> {
        let resp = self.get(&format!("/v1/tasks/{task_id}")).await?;
        Ok(resp.json::<TaskStatusResponse>().await?)
    }

    pub async fn ingest_batch_and_wait(&self, batch: &IngestBatch) -> Result<TaskStatusResponse> {
        let accepted = self.ingest_batch(batch).await?;
        loop {
            let status = self.task_status(&accepted.task_id).await?;
            match status.state.as_str() {
                "queued" | "processing" => {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                "done" => return Ok(status),
                "failed" => {
                    let msg = status
                        .error
                        .unwrap_or_else(|| format!("remote task {} failed", accepted.task_id));
                    return Err(anyhow!(msg));
                }
                other => return Err(anyhow!("unknown remote task state: {other}")),
            }
        }
    }

    pub async fn search_vector_server(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = build_search_request(Some(query), None, "vector", filters, limit);
        let resp = self.post_json("/v1/search", &payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
    }

    pub async fn search_hybrid_server(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = build_search_request(Some(query), None, "hybrid", filters, limit);
        let resp = self.post_json("/v1/search", &payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
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
        // Build the wire payload from the local document chunk. The
        // protocol struct is owned (not borrowed) — one allocation per
        // chunk, dwarfed by the HTTP body serialisation cost. Optional
        // metadata that the local chunk leaves as `None` is serialised
        // away by the protocol's `skip_serializing_if = Option::is_none`
        // attributes, so the wire bytes are byte-identical to the
        // previous lifetime-borrowed `IngestPayload`.
        let payload = IngestChunk {
            doc_id: doc.doc_id.clone(),
            chunk_index: doc.chunk_index,
            full_text: doc.full_text.clone().unwrap_or_default(),
            full_text_md: doc.full_text_md.clone(),
            headings: None, // headings stored in full_text_md already
            embedding: doc.embedding.clone().unwrap_or_default(),
            title: doc.title.clone(),
            author: doc.author.clone(),
            year: doc.year,
            filename: doc.filename.clone().unwrap_or_default(),
            ext: doc.ext.clone().unwrap_or_default(),
            language: doc.language.clone().unwrap_or_default(),
            location_uri: doc.location_uri.clone(),
            owner_id: doc.owner_id.clone(),
            source_hash: doc.source_hash.clone(),
            tags: doc.tags.clone(),
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
        let payload = build_search_request(Some(query), None, "text", filters, limit);
        let resp = self.post_json("/v1/search", &payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
    }

    async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = build_search_request(None, Some(embedding), "vector", filters, limit);
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
        let payload = build_search_request(Some(query), Some(embedding), "hybrid", filters, limit);
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
        let payload = UpdateLocationBody { new_uri: new_uri.to_owned() };
        self.post_json(&format!("/v1/docs/{doc_id}/location"), &payload)
            .await?;
        Ok(())
    }

    async fn update_location_by_uri(&self, old_uri: &str, new_uri: &str) -> Result<()> {
        let payload = UpdateLocationByUriBody {
            old_uri: old_uri.to_owned(),
            new_uri: new_uri.to_owned(),
        };
        self.post_json("/v1/docs/location/by-uri", &payload).await?;
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build a `crisp_index_protocol::SearchRequest` from the local
/// `SearchFilters` plus the per-call mode-specific arguments. Owns its
/// strings and embedding (one clone per call) — search is rare enough
/// vs ingest that the allocation cost is negligible.
fn build_search_request(
    query: Option<&str>,
    embedding: Option<&[f32]>,
    mode: &str,
    filters: &SearchFilters,
    limit: usize,
) -> SearchRequest {
    SearchRequest {
        query: query.map(str::to_owned),
        embedding: embedding.map(|e| e.to_vec()),
        mode: Some(mode.to_owned()),
        limit: Some(limit),
        owner_id: filters.owner_id.clone(),
        language: filters.language.clone(),
        year_min: filters.year_min,
        year_max: filters.year_max,
    }
}
