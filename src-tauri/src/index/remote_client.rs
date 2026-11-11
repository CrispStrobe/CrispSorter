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
use anyhow::{anyhow, ensure, Result};
use async_trait::async_trait;
use crisp_index_protocol::{
    BatchIngestResponse, IngestBatch, IngestChunk, SearchCapabilities, SearchRequest,
    TaskStatusResponse, UpdateLocationBody, UpdateLocationByUriBody, QUERY_LANGUAGE_VERSION,
    QUERY_VERSION_HEADER, METADATA_FILTER_VERSION, METADATA_FILTER_VERSION_HEADER,
    StoredPassage, StoredPassageRequest, STORED_PASSAGE_VERSION, STORED_PASSAGE_VERSION_HEADER,
    MAX_STORED_PASSAGE_RESPONSE_BYTES,
    CompleteSearchScope, CompleteSearchRequest, CompleteSearchSummary, CompleteGeneration,
    CompletePageRequest, CompletePage, CompleteCancelResponse, CompleteDestinationRequest,
    CompleteDestinationResponse, COMPLETE_SEARCH_VERSION, COMPLETE_SEARCH_VERSION_HEADER,
    MAX_COMPLETE_RESPONSE_BYTES, METADATA_SCHEMA_VERSION,
    ContainerMemberLocator, ContainerSourceReceipt, ContainerCoverage, ContainerMemberRequest, ContainerMemberResponse,
    CONTAINER_MEMBERS_VERSION, CONTAINER_MEMBERS_VERSION_HEADER, MAX_CONTAINER_MEMBER_RESPONSE_BYTES,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{sync::OnceLock, time::Duration};

use super::schema::{SearchFilters, SearchResult};
use super::{DocumentChunk, IndexBackend};
use crate::sync::proxy::ProxyConfig;

// ── RemoteClient ──────────────────────────────────────────────────────────────

static CONNECTION_TOKEN_SALT: OnceLock<[u8; 16]> = OnceLock::new();

#[derive(Clone)]
pub struct RemoteClient {
    base_url: String,
    api_key: String,
    client: Client,
}

async fn bounded_response_json<T: serde::de::DeserializeOwned>(mut response: reqwest::Response, limit: usize) -> Result<T> {
    let status = response.status();
    let limit = if status.is_success() { limit } else { limit.min(16 * 1024) };
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(anyhow!("The remote response exceeds the supported byte limit"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(anyhow!("The remote response exceeds the supported byte limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(anyhow!("remote index {status}: {}", String::from_utf8_lossy(&bytes)));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Default, Deserialize)]
struct ContainerFields {
    #[serde(default)] container_member: Option<ContainerMemberLocator>,
    #[serde(default)] container_source: Option<ContainerSourceReceipt>,
    #[serde(default)] container_coverage: Option<ContainerCoverage>,
}
impl ContainerFields {
    fn any(&self) -> bool { self.container_member.is_some() || self.container_source.is_some() || self.container_coverage.is_some() }
    fn validate(&self, owner: &str, doc: &str, uri: &str, source_hash: &str) -> Result<()> {
        if let Some(locator) = &self.container_member {
            locator.validate_identity(owner,doc,uri).map_err(anyhow::Error::msg)?;
            ensure!(locator.entries.last().is_some_and(|entry| entry.sha256.eq_ignore_ascii_case(source_hash)),
                "The remote member fingerprint differs from its locator receipt");
        } else { ensure!(!uri.starts_with("crisp+member:"), "The remote member is missing its typed source locator"); }
        if let Some(source) = &self.container_source {
            source.validate().map_err(anyhow::Error::msg)?;
            ensure!(source.sha256.eq_ignore_ascii_case(source_hash), "The remote container fingerprint differs from its source receipt");
        }
        if let Some(coverage) = &self.container_coverage { coverage.validate().map_err(anyhow::Error::msg)?; }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RemoteSearchResult {
    #[serde(flatten)] result: SearchResult,
    #[serde(flatten)] container: ContainerFields,
}

async fn read_search_results(response: reqwest::Response) -> Result<Vec<SearchResult>> {
    response.json::<Vec<RemoteSearchResult>>().await?.into_iter().map(|hit| {
        let mut result = hit.result;
        hit.container.validate(&result.owner_id,&result.doc_id,&result.location_uri,&result.source_hash)?;
        if hit.container.any() {
            let mut metadata = result.metadata_json.as_deref().map(serde_json::from_str::<serde_json::Value>).transpose()?
                .unwrap_or_else(|| serde_json::json!({}));
            let metadata = metadata.as_object_mut().ok_or_else(|| anyhow!("The remote result metadata is not an object"))?;
            for (name,value) in [("container_member",serde_json::to_value(hit.container.container_member)?),
                ("container_source",serde_json::to_value(hit.container.container_source)?),
                ("container_coverage",serde_json::to_value(hit.container.container_coverage)?)] {
                if value.is_null() { continue; }
                if let Some(previous) = metadata.get(name) {
                    ensure!(*previous == value, "The remote result has conflicting container metadata");
                }
                metadata.insert(name.into(),value);
            }
            result.metadata_json = Some(serde_json::to_string(metadata)?);
        }
        Ok(result)
    }).collect()
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

    /// Bind the server receipt to this configured connection without exposing
    /// its address or credentials in a report or a UI snapshot token.
    pub fn bind_generation(&self, generation: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        // A process-private random salt keeps exported receipts from becoming
        // an offline credential verifier. Snapshot tokens are live UI state.
        hash.update(CONNECTION_TOKEN_SALT.get_or_init(|| *uuid::Uuid::new_v4().as_bytes()));
        hash.update(self.base_url.len().to_le_bytes()); hash.update(self.base_url.as_bytes());
        hash.update(self.api_key.len().to_le_bytes()); hash.update(self.api_key.as_bytes());
        let identity = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        format!("remote-v1:{identity}:{generation}")
    }

    pub fn unbind_generation(&self, expected: &str) -> Result<String> {
        let prefix = self.bind_generation("");
        let generation = expected.strip_prefix(&prefix).ok_or_else(|| anyhow!(
            "The configured remote index changed after the snapshot was captured. Search again before exporting."))?;
        ensure!(generation.len() == 64 && generation.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "The captured remote index generation is invalid. Search again before exporting.");
        Ok(generation.to_owned())
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
        let capabilities: SearchCapabilities = bounded_response_json(response, 64 * 1024).await?;
        if !capabilities.supports_strict_search(hybrid) {
            return Err(anyhow!("The remote full-text index is not ready or does not support the required query language and hybrid constraints. Finish its rebuild or upgrade the server."));
        }
        Ok(Some(QUERY_LANGUAGE_VERSION))
    }

    async fn post_search(&self, mut request: SearchRequest) -> Result<reqwest::Response> {
        let filters = request.filters();
        filters.validate().map_err(|error| anyhow!(error))?;
        let required = filters.required_metadata_filter_version();
        if required > 0 || request.embedding_query.is_some() || request.query_plan.is_some() {
            let capabilities = self.search_capabilities().await
                .map_err(|error| anyhow!("Cannot verify remote metadata filters; upgrade the server or clear these filters: {error}"))?;
            validate_metadata_capabilities(&capabilities, &filters)?;
            if required > 0 { request.metadata_filter_version = Some(required); }
            if request.query_plan.is_some() {
                ensure!(capabilities.query_plan_version == crisp_index_protocol::QUERY_PLAN_VERSION,
                    "This server does not support the selected query planning. Upgrade it, or choose Advanced syntax with fuzzy and synonym expansion off.");
            }
            if request.embedding_query.is_some() {
                ensure!(capabilities.embedding_query_version == 1,
                    "This server cannot preserve the original semantic query while expanding lexical terms. Upgrade the server or turn off fuzzy and synonym expansion for Hybrid search.");
                request.embedding_query_version = Some(1);
            }
        }
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
        if let Some(version) = request.metadata_filter_version {
            let reported = response.headers().get(METADATA_FILTER_VERSION_HEADER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u32>().ok());
            if reported != Some(version) {
                return Err(anyhow!("The server did not confirm the negotiated metadata filters; its search results were not accepted."));
            }
        }
        if request.embedding_query.is_some() {
            confirm_version(&response, "x-crisp-embedding-query-version", 1, "original semantic query")?;
        }
        if request.query_plan.is_some() {
            confirm_version(&response, crisp_index_protocol::QUERY_PLAN_VERSION_HEADER, crisp_index_protocol::QUERY_PLAN_VERSION, "query intent")?;
        }
        Ok(response)
    }

    async fn search_capabilities(&self) -> Result<SearchCapabilities> {
        let response = self.client.get(self.url("/v1/capabilities"))
            .bearer_auth(&self.api_key).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!("This server does not advertise complete lexical search. Upgrade the server to count or export every matching document."));
        }
        bounded_response_json(response, 64 * 1024).await
    }

    pub async fn require_complete_search(&self, scope: &CompleteSearchScope) -> Result<()> {
        scope.validate().map_err(|error| anyhow!(error))?;
        let capabilities = self.search_capabilities().await?;
        ensure!(capabilities.api_version == 1 && capabilities.fts_ready
            && capabilities.complete_search_version == COMPLETE_SEARCH_VERSION
            && capabilities.query_language_version == QUERY_LANGUAGE_VERSION
            && (if capabilities.active_fts_schema_version == 0 { capabilities.fts_schema_version } else { capabilities.active_fts_schema_version }) == crisp_index_protocol::FTS_SCHEMA_VERSION
            && capabilities.metadata_filter_version.max(capabilities.metadata_filter_max_version) >= METADATA_FILTER_VERSION
            && capabilities.metadata_schema_version >= METADATA_SCHEMA_VERSION,
            "The remote index is not ready or does not support complete lexical search. Finish its rebuild or upgrade the server before counting or exporting all matches.");
        validate_metadata_capabilities(&capabilities, &scope.filters)
    }

    async fn post_complete<T: Serialize, R: serde::de::DeserializeOwned>(&self, path: &str, body: &T, scan: bool) -> Result<R> {
        let mut request = self.client.post(self.url(path)).bearer_auth(&self.api_key).json(body);
        if scan { request = request.timeout(Duration::from_secs(130)); }
        let response = request.send().await?;
        if response.status().is_success() {
            confirm_version(&response, COMPLETE_SEARCH_VERSION_HEADER, COMPLETE_SEARCH_VERSION, "complete lexical search")?;
        }
        bounded_response_json(response, MAX_COMPLETE_RESPONSE_BYTES).await
    }

    /// The generation is bound to the effective query and exact metadata scope.
    pub async fn complete_generation(&self, scope: &CompleteSearchScope) -> Result<CompleteGeneration> {
        self.require_complete_search(scope).await?;
        let receipt: CompleteGeneration = self.post_complete("/v1/search/generation", scope, false).await?;
        receipt.validate_for(scope).map_err(|error| anyhow!(error))?;
        Ok(receipt)
    }

    pub async fn complete_search(&self, request: &CompleteSearchRequest) -> Result<CompleteSearchSummary> {
        request.validate().map_err(|error| anyhow!(error))?;
        self.require_complete_search(&request.scope).await?;
        let summary: CompleteSearchSummary = self.post_complete("/v1/search/complete", request, true).await?;
        summary.validate_for(request).map_err(|error| anyhow!(error))?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
        ensure!(summary.expires_at_ms as u128 > now, "The server returned an expired complete-search snapshot. Search again.");
        Ok(summary)
    }

    pub async fn complete_page(&self, scope: &CompleteSearchScope, request: &CompletePageRequest, total: u64) -> Result<CompletePage> {
        request.validate().map_err(|error| anyhow!(error))?;
        let page: CompletePage = self.post_complete("/v1/search/complete/page", request, false).await?;
        page.validate_for(scope, request).map_err(|error| anyhow!(error))?;
        ensure!(page.matching_documents == total, "The remote complete-search total changed between pages; no complete report was accepted.");
        Ok(page)
    }

    pub async fn complete_destination(&self, request: &CompleteDestinationRequest) -> Result<()> {
        request.validate().map_err(|error| anyhow!(error))?;
        let receipt: CompleteDestinationResponse = self.post_complete("/v1/search/complete/destination", request, true).await?;
        ensure!(receipt.version == COMPLETE_SEARCH_VERSION && receipt.generation == request.generation,
            "The server did not confirm the captured source snapshot before report publication.");
        ensure!(!receipt.protected, "A search report cannot replace an indexed source document. Choose another destination.");
        Ok(())
    }

    pub async fn close_complete_search(&self, request_id: &str) -> Result<bool> {
        ensure!(crisp_index_protocol::complete::valid_request_id(request_id), "Invalid complete search request ID");
        let mut url = reqwest::Url::parse(&self.url("/v1/search/complete/"))?;
        url.path_segments_mut().map_err(|_| anyhow!("Invalid remote index URL"))?.pop_if_empty().push(request_id);
        let response = self.client.delete(url).bearer_auth(&self.api_key).timeout(Duration::from_secs(5)).send().await?;
        if response.status().is_success() { confirm_version(&response, COMPLETE_SEARCH_VERSION_HEADER, COMPLETE_SEARCH_VERSION, "complete-search cancellation")?; }
        let receipt: CompleteCancelResponse = bounded_response_json(response, 64 * 1024).await?;
        ensure!(receipt.version == COMPLETE_SEARCH_VERSION && receipt.request_id == request_id,
            "The server returned a different complete-search cancellation identity");
        Ok(receipt.cancelled)
    }

    /// Read exact stored source text only after negotiating its version and
    /// validating the response identity. A legacy server never receives a
    /// request it could silently interpret as an unscoped document lookup.
    pub async fn read_passage(&self, doc_id: &str, owner_id: &str, chunk_index: i32) -> Result<StoredPassage> {
        let request = StoredPassageRequest { passage_version: STORED_PASSAGE_VERSION,
            doc_id: doc_id.to_owned(), owner_id: owner_id.to_owned(), chunk_index };
        request.validate().map_err(|error| anyhow!(error))?;
        let capabilities = self.client.get(self.url("/v1/capabilities"))
            .bearer_auth(&self.api_key).send().await?;
        if capabilities.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!("This remote server does not advertise stored passage inspection. Upgrade the server to inspect its stored text."));
        }
        let capabilities: SearchCapabilities = bounded_response_json(capabilities, 64 * 1024).await?;
        if capabilities.api_version != 1 || !capabilities.fts_ready
            || capabilities.stored_passage_version != STORED_PASSAGE_VERSION {
            return Err(anyhow!("The remote index is not ready or does not support versioned stored passage inspection. Finish its rebuild or upgrade the server."));
        }
        let response = self.client.post(self.url("/v1/passages/read"))
            .bearer_auth(&self.api_key).json(&request).send().await?;
        if response.status().is_success() {
            let reported = response.headers().get(STORED_PASSAGE_VERSION_HEADER)
                .and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u32>().ok());
            if reported != Some(STORED_PASSAGE_VERSION) {
                return Err(anyhow!("The server did not confirm the negotiated stored passage version; its text was not accepted."));
            }
        }
        let passage: StoredPassage = bounded_response_json(response, MAX_STORED_PASSAGE_RESPONSE_BYTES).await?;
        passage.validate_for(&request).map_err(|error| anyhow!(error))?;
        Ok(passage)
    }

    async fn require_container_members(&self) -> Result<()> {
        let response = self.client.get(self.url("/v1/capabilities")).bearer_auth(&self.api_key).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!("This server does not advertise independently indexed container members. Upgrade the server first."));
        }
        let capabilities: SearchCapabilities = bounded_response_json(response,64*1024).await?;
        ensure!(capabilities.api_version == 1 && capabilities.fts_ready && capabilities.container_members_version == CONTAINER_MEMBERS_VERSION,
            "The remote index is not ready or does not support versioned container members. Finish its rebuild or upgrade the server.");
        Ok(())
    }

    /// Fetch actual document-clause truth with a separate capability and exact
    /// request receipts. Legacy servers retain ordinary passage inspection.
    pub async fn explain_passage(&self, request: &crisp_index_protocol::QueryExplanationRequest)
        -> Result<crisp_index_protocol::QueryExplanation> {
        use crisp_index_protocol::{QueryExplanation,QUERY_EXPLANATION_VERSION,
            QUERY_EXPLANATION_VERSION_HEADER,MAX_QUERY_EXPLANATION_RESPONSE_BYTES};
        request.validate().map_err(|error|anyhow!(error))?;
        let response=self.client.get(self.url("/v1/capabilities")).bearer_auth(&self.api_key).send().await?;
        if response.status()==reqwest::StatusCode::NOT_FOUND {
            return Err(anyhow!("This server does not advertise query explanations. Upgrade the server to inspect lexical proof."));
        }
        let capabilities:SearchCapabilities=bounded_response_json(response,64*1024).await?;
        ensure!(capabilities.supports_strict_search(false)
            && capabilities.stored_passage_version==STORED_PASSAGE_VERSION
            && capabilities.query_explanation_version==QUERY_EXPLANATION_VERSION,
            "The remote index is not ready or does not support versioned query explanations. Finish its rebuild or upgrade the server.");
        let response=self.client.post(self.url("/v1/passages/explain"))
            .bearer_auth(&self.api_key).json(request).send().await?;
        if response.status().is_success() {
            let version=response.headers().get(QUERY_EXPLANATION_VERSION_HEADER)
                .and_then(|value|value.to_str().ok()).and_then(|value|value.parse::<u32>().ok());
            ensure!(version==Some(QUERY_EXPLANATION_VERSION),
                "The server did not confirm the negotiated query explanation version; its evidence was not accepted");
        }
        let result:QueryExplanation=bounded_response_json(response,MAX_QUERY_EXPLANATION_RESPONSE_BYTES).await?;
        result.validate_for(request).map_err(|error|anyhow!(error))?;
        Ok(result)
    }

    /// Owner-scoped persisted metadata, never a locator supplied by the webview.
    pub async fn read_container_member(&self, doc_id: &str, owner_id: &str) -> Result<Option<ContainerMemberLocator>> {
        let request = ContainerMemberRequest { version:CONTAINER_MEMBERS_VERSION,owner_id:owner_id.into(),doc_id:doc_id.into() };
        request.validate().map_err(anyhow::Error::msg)?;
        self.require_container_members().await?;
        let response = self.client.post(self.url("/v1/members/read")).bearer_auth(&self.api_key).json(&request).send().await?;
        if response.status().is_success() { confirm_version(&response,CONTAINER_MEMBERS_VERSION_HEADER,CONTAINER_MEMBERS_VERSION,"container member metadata")?; }
        let receipt: ContainerMemberResponse = bounded_response_json(response,MAX_CONTAINER_MEMBER_RESPONSE_BYTES).await?;
        receipt.validate_for(&request).map_err(anyhow::Error::msg)?;
        Ok(receipt.locator)
    }

    fn document_url(
        &self,
        doc_id: &str,
        location: bool,
        owner_id: Option<&str>,
    ) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.url("/v1/docs/"))?;
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| anyhow!("Remote index URL cannot contain path segments"))?;
            path.pop_if_empty().push(doc_id);
            if location {
                path.push("location");
            }
        }
        if let Some(owner) = owner_id {
            url.query_pairs_mut().append_pair("owner_id", owner);
        }
        Ok(url)
    }

    /// Select an owner when a content identifier is shared across collections.
    pub async fn delete_doc_scoped(&self, doc_id: &str, owner_id: Option<&str>) -> Result<()> {
        let response = self
            .client
            .delete(self.document_url(doc_id, false, owner_id)?)
            .bearer_auth(&self.api_key)
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(anyhow!(
                "Remote document deletion failed {status}: {}",
                response.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    /// Update one owner's location without affecting another copy of the file.
    pub async fn update_location_scoped(
        &self,
        doc_id: &str,
        new_uri: &str,
        owner_id: Option<&str>,
    ) -> Result<()> {
        let response = self
            .client
            .post(self.document_url(doc_id, true, owner_id)?)
            .bearer_auth(&self.api_key)
            .json(&UpdateLocationBody {
                new_uri: new_uri.into(),
            })
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(anyhow!(
                "Remote location update failed {status}: {}",
                response.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    pub async fn ingest_batch(&self, batch: &IngestBatch) -> Result<BatchIngestResponse> {
        let grouped = batch.container_group_version.is_some() || batch.chunks.iter().any(|chunk|
            chunk.container_member.is_some() || chunk.container_source.is_some() || chunk.container_coverage.is_some());
        if grouped {
            ensure!(batch.container_group_version.is_none_or(|version| version == CONTAINER_MEMBERS_VERSION),
                "Unsupported container group publication version");
            self.require_container_members().await?;
            let mut batch = batch.clone(); batch.container_group_version = Some(CONTAINER_MEMBERS_VERSION);
            let response = self.client.post(self.url("/v1/ingest/containers")).bearer_auth(&self.api_key).json(&batch).send().await?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(anyhow!("This server cannot publish root and member documents atomically. Upgrade the server before indexing containers."));
            }
            if response.status().is_success() { confirm_version(&response,CONTAINER_MEMBERS_VERSION_HEADER,CONTAINER_MEMBERS_VERSION,"container group publication")?; }
            return bounded_response_json(response,64*1024).await;
        }
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
        let payload = build_search_request(Some(query), None, "vector", filters, limit)?;
        let resp = self.post_search(payload).await?;
        read_search_results(resp).await
    }

    pub async fn search_hybrid_server(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = build_search_request(Some(query), None, "hybrid", filters, limit)?;
        let resp = self.post_search(payload).await?;
        read_search_results(resp).await
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
        let container: ContainerFields = doc.metadata_json.as_deref().map(serde_json::from_str).transpose()?.unwrap_or_default();
        container.validate(&doc.owner_id,&doc.doc_id,&doc.location_uri,&doc.source_hash)?;
        ensure!(!container.any(), "Container documents must be published atomically with their root source through batch ingestion");
        let payload = IngestChunk {
            doc_id: doc.doc_id.clone(),
            chunk_index: doc.chunk_index,
            chunk_total: Some(doc.chunk_total),
            chunk_start_char: doc.chunk_start_char,
            chunk_end_char: doc.chunk_end_char,
            source_identity_version: None,
            full_text: doc.full_text.clone().unwrap_or_default(),
            full_text_md: doc.full_text_md.clone(),
            headings: doc
                .headings_text
                .as_ref()
                .map(|headings| vec![headings.clone()]),
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
            parent_dir: doc.parent_dir.clone(), volume_id: doc.volume_id.clone(), url: doc.url.clone(),
            text_translated: doc.text_translated.clone(), text_translated_lang: doc.text_translated_lang.clone(),
            doc_status: doc.doc_status.clone(), audio_duration_seconds: doc.audio_duration_seconds,
            image_camera_make: doc.image_camera_make.clone(), image_camera_model: doc.image_camera_model.clone(),
            container_member: container.container_member, container_source: container.container_source,
            container_coverage: container.container_coverage,
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
        let payload = build_search_request(Some(query), None, "text", filters, limit)?;
        let resp = self.post_search(payload).await?;
        read_search_results(resp).await
    }

    async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = build_search_request(None, Some(embedding), "vector", filters, limit)?;
        let resp = self.post_search(payload).await?;
        read_search_results(resp).await
    }

    async fn search_hybrid(
        &self,
        query: &str,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let payload = build_search_request(Some(query), Some(embedding), "hybrid", filters, limit)?;
        let resp = self.post_search(payload).await?;
        read_search_results(resp).await
    }

    async fn delete_doc(&self, doc_id: &str) -> Result<()> {
        self.delete_doc_scoped(doc_id, None).await
    }

    async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()> {
        self.update_location_scoped(doc_id, new_uri, None).await
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
    query: Option<&str>, embedding: Option<&[f32]>, mode: &str,
    filters: &SearchFilters, limit: usize,
) -> Result<SearchRequest> {
    let wire = wire_filters(filters)?;
    let effective = query.map(|query| if mode == "vector" { Ok(query.to_owned()) } else { expanded_query(query, filters) }).transpose()?;
    // Generated fuzzy/OR syntax is not a user-authored mandatory constraint.
    // Declare the original intent for natural language and expanded hybrid
    // searches so the server derives the same policy as the local engine.
    let needs_plan = mode != "vector" && (filters.query_intent.as_deref() == Some("natural")
        || (mode == "hybrid" && effective.as_deref() != query));
    let query_plan = if needs_plan {
        Some(crisp_index_protocol::SearchQueryPlan { version: crisp_index_protocol::QUERY_PLAN_VERSION,
            intent: filters.query_intent.as_deref().unwrap_or("advanced").into(),
            original_query: query.ok_or_else(|| anyhow!("A query is required"))?.to_owned(),
            fuzzy: filters.fuzzy, synonyms: filters.synonyms })
    } else { None };
    let embedding_query = if mode == "hybrid" && embedding.is_none()
        && effective.as_deref() != query { query.map(str::to_owned) } else { None };
    Ok(SearchRequest {
        query_plan,
        query: effective, embedding: embedding.map(|value| value.to_vec()), mode: Some(mode.into()), limit: Some(limit),
        query_language_version: None, metadata_filter_version: None, embedding_query, embedding_query_version: None,
        owner_id: wire.owner_id, language: wire.language, year_min: wire.year_min, year_max: wire.year_max,
        ext: wire.ext, tag: wire.tag, source_hash_prefix: wire.source_hash_prefix, doc_id_scope: wire.doc_id_scope,
        tags: wire.tags, parent_dir_prefix: wire.parent_dir_prefix, url_domain: wire.url_domain,
        audio_duration_min_seconds: wire.audio_duration_min_seconds, audio_duration_max_seconds: wire.audio_duration_max_seconds,
        image_camera_make: wire.image_camera_make, image_camera_model: wire.image_camera_model,
        prefer_translated_lang: wire.prefer_translated_lang, doc_status: wire.doc_status, mounted_volume_ids: wire.mounted_volume_ids,
    })
}

/// Shared by interactive retrieval, exact counts, generation checks and export.
pub fn complete_search_scope(query: &str, filters: &SearchFilters) -> Result<CompleteSearchScope> {
    ensure!(!query.trim().is_empty() && query.len() <= 8192, "Enter a search query of at most 8192 bytes");
    let scope = CompleteSearchScope { version: COMPLETE_SEARCH_VERSION,
        query: expanded_query(query, filters)?, filters: wire_filters(filters)? };
    scope.validate().map_err(|error| anyhow!(error))?;
    super::fts_query::has_explicit_syntax(&scope.query)?;
    Ok(scope)
}

fn expanded_query(query: &str, filters: &SearchFilters) -> Result<String> {
    Ok(filters.query_plan(query)?.lexical_query)
}

pub fn wire_filters(filters: &SearchFilters) -> Result<crisp_index_protocol::SearchFilters> {
    ensure!(!filters.omni_search && !filters.colbert_rerank,
        "The remote server does not support the selected cross-modal or multi-vector ranking option. Turn it off or use the local index.");
    filters.validate().map_err(|error| anyhow!(error))?;
    let wire = crisp_index_protocol::SearchFilters {
        owner_id: filters.owner_id.clone(), language: filters.language.clone(),
        year_min: filters.year_min, year_max: filters.year_max, ext: filters.ext.clone(),
        tag: filters.tag.clone(), source_hash_prefix: filters.source_hash_prefix.clone(), doc_id_scope: filters.doc_id_scope.clone(),
        tags: filters.tags.clone(), parent_dir_prefix: filters.parent_dir_prefix.clone(), url_domain: filters.url_domain.clone(),
        audio_duration_min_seconds: filters.audio_duration_min_seconds, audio_duration_max_seconds: filters.audio_duration_max_seconds,
        image_camera_make: filters.image_camera_make.clone(), image_camera_model: filters.image_camera_model.clone(),
        prefer_translated_lang: filters.prefer_translated_lang.clone(), doc_status: filters.doc_status.clone(),
        mounted_volume_ids: filters.mounted_volume_ids.clone(),
    };
    wire.validate().map_err(|error| anyhow!(error))?;
    Ok(wire)
}

fn validate_metadata_capabilities(capabilities: &SearchCapabilities, filters: &crisp_index_protocol::SearchFilters) -> Result<()> {
    let required = filters.required_metadata_filter_version();
    ensure!(capabilities.api_version == 1 && capabilities.fts_ready
        && capabilities.metadata_filter_version.max(capabilities.metadata_filter_max_version) >= required
        && capabilities.metadata_filter_version <= METADATA_FILTER_VERSION,
        "This server does not support the required metadata filters. Upgrade the server or clear those filters.");
    if required >= 2 {
        ensure!(capabilities.metadata_schema_version >= METADATA_SCHEMA_VERSION,
            "The remote stored metadata schema does not support these predicates. Upgrade and rebuild the server index.");
        let values = serde_json::to_value(filters)?;
        let fields = values.as_object().ok_or_else(|| anyhow!("Invalid metadata filter object"))?;
        for field in fields.keys() {
            ensure!(capabilities.metadata_fields.contains(field),
                "The remote server does not advertise the {field} metadata predicate. Upgrade the server or clear that filter.");
        }
    }
    Ok(())
}

fn confirm_version(response: &reqwest::Response, header: &str, expected: u32, feature: &str) -> Result<()> {
    let reported = response.headers().get(header).and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u32>().ok());
    ensure!(reported == Some(expected),
        "The server did not confirm the negotiated {feature} version; its response was not accepted.");
    Ok(())
}
