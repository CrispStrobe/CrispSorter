//! Minimal app transport types; the HTTP client below is the production source.
use anyhow::Result;
use async_trait::async_trait;

pub mod schema {
    pub use crisp_index_protocol::{SearchFilters, SearchHit as SearchResult};
}
pub mod fts_query {
    pub use crisp_search_query::has_explicit_syntax;
}

pub struct DocumentChunk {
    pub doc_id: String,
    pub owner_id: String,
    pub chunk_index: i32,
    pub chunk_total: i32,
    pub chunk_start_char: Option<i32>,
    pub chunk_end_char: Option<i32>,
    pub full_text: Option<String>,
    pub headings_text: Option<String>,
    pub full_text_md: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub filename: Option<String>,
    pub ext: Option<String>,
    pub language: Option<String>,
    pub location_uri: String,
    pub source_hash: String,
    pub tags: Vec<String>,
}
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
