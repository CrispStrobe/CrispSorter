/// CrispSorter search / RAG index module.
///
/// Sub-modules:
///   location    — FileLocation URI model (crisp+local/vps/internxt/internxt-zip)
///   schema      — Arrow schema, DocumentChunk, SearchResult, SearchFilters
///   embedder    — fastembed-rs wrapper (bge-m3 / multilingual-e5 / …)
///   fts_query   — dtSearch-style query translator → Tantivy query tree
///   fts_index   — Tantivy index CRUD + search
///   local_index — LanceDB local backend (coming in P5)
///   remote_client — HTTP client to VPS server (coming in P9)
///   ingest      — orchestration pipeline (coming in P6)
///   search      — unified search with RRF reranking (coming in P5)

pub mod location;
pub mod schema;
pub mod embedder;
pub mod fts_query;
pub mod fts_index;
pub mod local_index;
pub mod remote_client;
pub mod search;
pub mod ingest;
pub mod tauri_commands;

// Re-export the most commonly used types.
pub use location::{FileLocation, RetrievalCost};
pub use schema::{DocumentChunk, SearchResult, SearchFilters, build_schema};
pub use embedder::{Embedder, EmbedderConfig, EmbedderModel, EmbedderDevice, TextChunk, chunk_text};
pub use fts_index::FtsIndex;
pub use local_index::LocalIndex;
pub use search::SearchEngine;
pub use ingest::{IngestPipeline, IngestConfig, RawDocument, IngestStats};

use async_trait::async_trait;
use anyhow::Result;

/// Abstraction over local and remote index backends.
///
/// Both `LocalIndex` (P5) and `RemoteClient` (P9) will implement this trait.
/// Tauri commands delegate to whichever `Arc<dyn IndexBackend>` is active in `AppState`.
#[async_trait]
pub trait IndexBackend: Send + Sync {
    async fn ingest(&self, doc: DocumentChunk) -> Result<()>;

    async fn search_text(
        &self,
        query: &str,
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>>;

    async fn search_vector(
        &self,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>>;

    async fn search_hybrid(
        &self,
        query: &str,
        embedding: &[f32],
        filters: &SearchFilters,
        limit: usize,
    ) -> Result<Vec<SearchResult>>;

    async fn delete_doc(&self, doc_id: &str) -> Result<()>;

    /// Update the stored location URI for a document (called when Sort moves a file).
    async fn update_location(&self, doc_id: &str, new_uri: &str) -> Result<()>;
}

/// Active index configuration held in Tauri `AppState`.
pub struct IndexState {
    pub backend: Option<std::sync::Arc<dyn IndexBackend>>,
    /// Raw `LocalIndex` kept separately so `index_build_ivf_pq` can call it.
    pub local:   Option<std::sync::Arc<LocalIndex>>,
    pub fts: Option<std::sync::Arc<FtsIndex>>,
    /// Embedder behind Mutex because fastembed 5.x embed() takes &mut self.
    pub embedder: Option<std::sync::Arc<tokio::sync::Mutex<Embedder>>>,
    /// Unified search engine (set alongside `backend` when `BackendType::Local`).
    pub engine: Option<std::sync::Arc<SearchEngine>>,
    /// Active ingest pipeline.
    pub pipeline: Option<std::sync::Arc<IngestPipeline>>,
    pub config: IndexConfig,
}

impl IndexState {
    pub fn disabled() -> Self {
        IndexState {
            backend:  None,
            local:    None,
            fts:      None,
            embedder: None,
            engine:   None,
            pipeline: None,
            config:   IndexConfig::default(),
        }
    }
}

/// Index configuration mirroring the Settings UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexConfig {
    pub enabled: bool,
    pub mode: SearchMode,
    pub backend_type: BackendType,
    pub remote_url: Option<String>,
    pub remote_api_key: Option<String>,
    pub embedder_model: EmbedderModel,
    pub embedder_device: EmbedderDevice,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            enabled: false,
            mode: SearchMode::Hybrid,
            backend_type: BackendType::Local,
            remote_url: None,
            remote_api_key: None,
            embedder_model: EmbedderModel::BgeM3,
            embedder_device: EmbedderDevice::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    TextOnly,
    VectorOnly,
    #[default]
    Hybrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendType {
    #[default]
    Local,
    Remote,
}
