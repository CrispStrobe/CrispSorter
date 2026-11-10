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
    BatchIngestResponse, IngestBatch, IngestChunk, SearchCapabilities, SearchRequest,
    TaskStatusResponse, UpdateLocationBody, UpdateLocationByUriBody, QUERY_LANGUAGE_VERSION,
    QUERY_VERSION_HEADER,
};
use reqwest::Client;
use serde::Serialize;
use std::time::Duration;

use super::schema::{SearchFilters, SearchResult};
use super::{DocumentChunk, IndexBackend};
use crate::sync::proxy::ProxyConfig;

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
            client: crate::sync::proxy::build_async_client_with_timeout(
                proxy,
                Duration::from_secs(30),
            )?,
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

    /// Negotiate lexical meaning before sending syntax that an older server
    /// could interpret differently. Plain queries remain available on legacy
    /// servers; explicit syntax requires the shared versioned parser.
    pub async fn query_language_version(&self, query: &str, hybrid: bool) -> Result<Option<u32>> {
        let explicit = super::fts_query::has_explicit_syntax(query)?;
        let response = self
            .client
            .get(self.url("/v1/capabilities"))
            .bearer_auth(&self.api_key)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            if explicit {
                return Err(anyhow!("This server does not advertise versioned lexical search. Upgrade the server before using Boolean, field, phrase, wildcard, fuzzy, or proximity queries."));
            }
            return Ok(None);
        }
        if !response.status().is_success() {
            let status = response.status();
            return Err(anyhow!(
                "Remote search capability check failed {status}: {}",
                response.text().await.unwrap_or_default()
            ));
        }
        let capabilities = response.json::<SearchCapabilities>().await?;
        if !capabilities.supports_strict_search(hybrid) {
            return Err(anyhow!("The remote full-text index is not ready or does not support the required query language and hybrid constraints. Finish its rebuild or upgrade the server."));
        }
        Ok(Some(QUERY_LANGUAGE_VERSION))
    }

    async fn post_search(&self, mut request: SearchRequest) -> Result<reqwest::Response> {
        let mode = request.mode.as_deref().unwrap_or("hybrid");
        if mode != "vector" {
            let query = request
                .query
                .as_deref()
                .ok_or_else(|| anyhow!("A lexical query is required"))?;
            request.query_language_version =
                self.query_language_version(query, mode == "hybrid").await?;
        }
        let response = self.post_json("/v1/search", &request).await?;
        if let Some(version) = request.query_language_version {
            let reported = response
                .headers()
                .get(QUERY_VERSION_HEADER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u32>().ok());
            if reported != Some(version) {
                return Err(anyhow!("The server did not confirm the negotiated query language version; its search results were not accepted."));
            }
        }
        Ok(response)
    }

    fn document_url(&self,doc_id:&str,location:bool,owner_id:Option<&str>)->Result<reqwest::Url> {
        let mut url=reqwest::Url::parse(&self.url("/v1/docs/"))?;
        {
            let mut path=url.path_segments_mut().map_err(|_|anyhow!("Remote index URL cannot contain path segments"))?;
            path.pop_if_empty().push(doc_id);
            if location {path.push("location");}
        }
        if let Some(owner)=owner_id {url.query_pairs_mut().append_pair("owner_id",owner);}
        Ok(url)
    }

    /// Select an owner when a content identifier is shared across collections.
    pub async fn delete_doc_scoped(&self,doc_id:&str,owner_id:Option<&str>)->Result<()> {
        let response=self.client.delete(self.document_url(doc_id,false,owner_id)?)
            .bearer_auth(&self.api_key).send().await?;
        if !response.status().is_success() {
            let status=response.status();
            return Err(anyhow!("Remote document deletion failed {status}: {}",response.text().await.unwrap_or_default()));
        }
        Ok(())
    }

    /// Update one owner's location without affecting another copy of the file.
    pub async fn update_location_scoped(&self,doc_id:&str,new_uri:&str,owner_id:Option<&str>)->Result<()> {
        let response=self.client.post(self.document_url(doc_id,true,owner_id)?)
            .bearer_auth(&self.api_key).json(&UpdateLocationBody{new_uri:new_uri.into()}).send().await?;
        if !response.status().is_success() {
            let status=response.status();
            return Err(anyhow!("Remote location update failed {status}: {}",response.text().await.unwrap_or_default()));
        }
        Ok(())
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
        let resp = self.post_search(payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
    }

    pub async fn search_hybrid_server(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = build_search_request(Some(query), None, "hybrid", filters, limit);
        let resp = self.post_search(payload).await?;
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
            chunk_total: Some(doc.chunk_total),
            chunk_start_char: doc.chunk_start_char,
            chunk_end_char: doc.chunk_end_char,
            full_text: doc.full_text.clone().unwrap_or_default(),
            full_text_md: doc.full_text_md.clone(),
            headings: doc.headings_text.as_ref().map(|headings| vec![headings.clone()]),
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
        let resp = self.post_search(payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
    }

    async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = build_search_request(None, Some(embedding), "vector", filters, limit);
        let resp = self.post_search(payload).await?;
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
        let resp = self.post_search(payload).await?;
        Ok(resp.json::<Vec<SearchResult>>().await?)
    }

    async fn delete_doc(&self, doc_id: &str) -> Result<()> {
        self.delete_doc_scoped(doc_id,None).await
    }

    async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()> {
        self.update_location_scoped(doc_id,new_uri,None).await
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
        query_language_version: None,
        owner_id: filters.owner_id.clone(),
        language: filters.language.clone(),
        year_min: filters.year_min,
        year_max: filters.year_max,
    }
}
