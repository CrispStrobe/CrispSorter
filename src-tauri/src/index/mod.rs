pub mod embedder;
pub mod fts_index;
pub mod fts_query;
pub mod hf_prefetch;
pub mod ingest;
pub mod l2_metadata;
pub mod local_index;
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
pub mod remote_client;
pub mod schema;
pub mod search;
pub mod tauri_commands;

#[cfg(test)]
pub mod benchmarks;

// Re-export the most commonly used types.
pub use embedder::{
    chunk_text, Embedder, EmbedderBackend, EmbedderConfig, EmbedderDevice, EmbedderModel,
    TextChunk,
};
pub use fts_index::FtsIndex;
pub use ingest::{IngestConfig, IngestPipeline, IngestStats, RawDocument};
pub use local_index::LocalIndex;
pub use location::{FileLocation, RetrievalCost};
pub use schema::{build_schema, DocumentChunk, SearchFilters, SearchResult};
pub use search::SearchEngine;

use anyhow::Result;
use async_trait::async_trait;

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
    pub local: Option<std::sync::Arc<LocalIndex>>,
    pub fts: Option<std::sync::Arc<FtsIndex>>,
    /// Embedder behind Mutex because fastembed 5.x embed() takes &mut self.
    pub embedder: Option<std::sync::Arc<tokio::sync::Mutex<Embedder>>>,
    /// Unified search engine (set alongside `backend` when `BackendType::Local`).
    pub engine: Option<std::sync::Arc<SearchEngine>>,
    /// Active ingest pipeline.
    pub pipeline: Option<std::sync::Arc<IngestPipeline>>,
    pub config: IndexConfig,
    /// Set to `true` while an `index_init` is running so we can reject
    /// concurrent re-init attempts (each download is multi-GB; we don't want
    /// two of them racing on the same cache).
    pub initializing: bool,
}

impl IndexState {
    pub fn disabled() -> Self {
        IndexState {
            backend: None,
            local: None,
            fts: None,
            embedder: None,
            engine: None,
            pipeline: None,
            config: IndexConfig::default(),
            initializing: false,
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
    #[serde(default)]
    pub embedder_backend: EmbedderBackend,
    /// Master switch for vector capabilities. When `false`, init never
    /// loads an embedder model — the catalog can still scan + store
    /// filesystem metadata (L1) and embedded file metadata (L2),
    /// Tantivy still does full-text indexing on extracted L3 text.
    /// Saves multi-GB downloads + hundreds of MB of resident memory
    /// when the user only wants offline file cataloguing.
    #[serde(default = "default_use_vector")]
    pub use_vector: bool,
}

fn default_use_vector() -> bool {
    true
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
            embedder_backend: EmbedderBackend::Onnx,
            use_vector: true,
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
