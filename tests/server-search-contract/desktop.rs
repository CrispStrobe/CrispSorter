//! Minimal app transport types; the HTTP client below is the production source.
use anyhow::Result;
use async_trait::async_trait;

#[path = "../../src-tauri/src/index/schema.rs"]
mod local_schema;
pub mod schema {
    pub use super::local_schema::SearchFilters;
    pub use crisp_index_protocol::SearchHit as SearchResult;
}
pub use local_schema::DocumentChunk;
pub mod fts_query { pub use crisp_search_query::*; }
#[path = "../../src-tauri/src/index/synonyms.rs"]
pub mod synonyms;
#[path = "../../src-tauri/src/index/search_report.rs"]
pub mod search_report;
#[path = "../../src-tauri/src/index/report_output.rs"]
pub mod report_output;
#[async_trait]
pub trait IndexBackend: Send + Sync {
    async fn ingest(&self, doc: DocumentChunk) -> Result<()>;
    async fn search_text(
        &self,
        query: &str,
        filters: &schema::SearchFilters,
        limit: usize,
    ) -> Result<Vec<schema::SearchResult>>;
    async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &schema::SearchFilters,
        limit: usize,
    ) -> Result<Vec<schema::SearchResult>>;
    async fn search_hybrid(
        &self,
        query: &str,
        embedding: &[f32],
        filters: &schema::SearchFilters,
        limit: usize,
    ) -> Result<Vec<schema::SearchResult>>;
    async fn delete_doc(&self, doc_id: &str) -> Result<()>;
    async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()>;
    async fn update_location_by_uri(&self, old_uri: &str, new_uri: &str) -> Result<()>;
}
#[path = "../../src-tauri/src/index/remote_client.rs"]
pub mod remote_client;

#[path = "../../src-tauri/src/index/remote_summary.rs"]
pub mod remote_summary;
