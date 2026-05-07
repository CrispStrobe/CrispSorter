pub mod embedder;
pub mod fts_index;
pub mod fts_query;
pub mod hf_prefetch;
pub mod ingest;
pub mod l2_metadata;
pub mod local_index;
pub mod reranker;
pub mod task_failure;
/// CrispSorter search / RAG index module.
///
/// Sub-modules:
///   location    — FileLocation URI model (crisp+local/vps/internxt/internxt-zip)
///   schema      — Arrow schema, DocumentChunk, SearchResult, SearchFilters
///   embedder    — fastembed-rs wrapper (bge-m3 / multilingual-e5 / …)
///   fts_query   — query translator → Tantivy query tree
///   fts_index   — Tantivy index CRUD + search
///   local_index — LanceDB local backend
///   remote_client — HTTP client to VPS server
///   ingest      — orchestration pipeline
///   search      — unified search with RRF reranking
pub mod location;
pub mod remote_client;
pub mod schema;
pub mod search;
pub mod tauri_commands;

#[cfg(test)]
pub mod benchmarks;

// Re-export the most commonly used types.
pub use embedder::{
    chunk_text, EmbedRole, Embedder, EmbedderBackend, EmbedderConfig, EmbedderDevice,
    EmbedderModel, TextChunk,
};
pub use fts_index::FtsIndex;
pub use ingest::{IngestConfig, IngestPipeline, IngestStats, RawDocument};
pub use local_index::LocalIndex;
pub use location::{FileLocation, RetrievalCost};
pub use reranker::{Reranker, RerankerHandle, RerankerModel};
pub use schema::{build_schema, DocumentChunk, SearchFilters, SearchResult};
pub use search::SearchEngine;

use anyhow::Result;
use async_trait::async_trait;

/// Abstraction over local and remote index backends.
///
/// Both `LocalIndex` and `RemoteClient` implement this trait. Tauri commands
/// delegate to whichever `Arc<dyn IndexBackend>` is active in `AppState`.
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

    /// Update location URI by matching the old URI (no doc_id required).
    async fn update_location_by_uri(&self, old_uri: &str, new_uri: &str) -> Result<()>;
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
    /// Cross-encoder reranker handle. `None` when `config.reranker_model`
    /// is `None`; otherwise a cheap-to-clone handle that lazy-loads the
    /// GGUF on first scoring call.
    pub reranker: Option<RerankerHandle>,
    pub config: IndexConfig,
    /// Last observed remote queue depth during a foreground remote ingest.
    /// Used by the shared queue-depth chip when no local writer pipeline exists.
    pub remote_queue_depth: usize,
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
            reranker: None,
            config: IndexConfig::default(),
            remote_queue_depth: 0,
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
    /// Where embedding computation happens for remote-backend ingest.
    /// `Client` (default): embed locally before posting.
    /// `Server`: post raw text; server embeds (needs P11 step 5 on the server).
    /// Ignored when `use_vector = false` or `backend_type = Local`.
    #[serde(default)]
    pub embedder_location: EmbedderLocation,
    /// Cross-encoder reranker model. `None` disables reranking.
    /// GGUF-only via CrispEmbed (requires the `crispembed` cargo feature).
    #[serde(default)]
    pub reranker_model: Option<RerankerModel>,
    /// How many top candidates to score with the reranker after RRF.
    /// Default 50; smaller is faster, larger trades latency for recall.
    #[serde(default = "default_rerank_top_n")]
    pub rerank_top_n: usize,
    /// Override location for downloaded model weights (ONNX + GGUF).
    /// `None` (= empty in the UI) defaults to `{data_dir}/models/`. Useful
    /// for pointing at an external volume shared with CrispEmbed CLI or
    /// other projects, so models don't get re-downloaded per app install.
    /// The env var `CRISPSORTER_MODEL_CACHE_DIR` overrides this when set.
    #[serde(default)]
    pub model_cache_dir: Option<String>,
    /// Matryoshka truncation dim. `None` or `Some(0)` = model default.
    /// Only applied on the CrispEmbed (GGUF) backend; ignored otherwise.
    /// The LanceDB schema is built around the effective dim, so changing
    /// this on an existing index requires re-ingestion.
    #[serde(default)]
    pub matryoshka_dim: Option<u32>,
}

fn default_use_vector() -> bool {
    true
}

fn default_rerank_top_n() -> usize {
    50
}

/// Pick the effective model-cache directory, in priority order:
///   1. `CRISPSORTER_MODEL_CACHE_DIR` env var (machine-wide override)
///   2. `IndexConfig.model_cache_dir` (UI setting)
///   3. `{data_dir}/models/` (default, app-data-relative)
///
/// Always returns a path; creates the directory if absent so downstream
/// hf-hub / fastembed callers don't fail on missing dirs.
pub fn resolve_model_cache_dir(
    config: &IndexConfig,
    data_dir: &std::path::Path,
) -> std::path::PathBuf {
    let path = std::env::var("CRISPSORTER_MODEL_CACHE_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            config
                .model_cache_dir
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
        })
        .unwrap_or_else(|| data_dir.join("models"));
    if let Err(e) = std::fs::create_dir_all(&path) {
        eprintln!(
            "[index] Could not create model cache dir {}: {} — fastembed/hf-hub will retry",
            path.display(),
            e
        );
    }
    path
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
            embedder_location: EmbedderLocation::Client,
            reranker_model: None,
            rerank_top_n: default_rerank_top_n(),
            model_cache_dir: None,
            matryoshka_dim: None,
        }
    }
}

#[cfg(test)]
mod resolve_cache_tests {
    use super::*;

    fn cfg_with(model_cache_dir: Option<String>) -> IndexConfig {
        IndexConfig {
            model_cache_dir,
            ..IndexConfig::default()
        }
    }

    /// Env var beats UI override beats default. We can't easily mutate
    /// process env in parallel tests, so the env-var arm is exercised
    /// only when the variable is already set (skip otherwise).
    #[test]
    fn falls_through_to_default() {
        if std::env::var_os("CRISPSORTER_MODEL_CACHE_DIR").is_some() {
            return; // env var present — covered by env_override test
        }
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with(None);
        let resolved = resolve_model_cache_dir(&cfg, tmp.path());
        assert_eq!(resolved, tmp.path().join("models"));
        assert!(resolved.is_dir(), "models dir should be created");
    }

    #[test]
    fn ui_override_wins_over_default() {
        if std::env::var_os("CRISPSORTER_MODEL_CACHE_DIR").is_some() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("custom-cache");
        let cfg = cfg_with(Some(target.to_string_lossy().into_owned()));
        let resolved = resolve_model_cache_dir(&cfg, tmp.path());
        assert_eq!(resolved, target);
        assert!(resolved.is_dir());
    }

    #[test]
    fn empty_or_whitespace_override_falls_back_to_default() {
        if std::env::var_os("CRISPSORTER_MODEL_CACHE_DIR").is_some() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        for s in ["", "   ", "\t"] {
            let cfg = cfg_with(Some(s.to_owned()));
            let resolved = resolve_model_cache_dir(&cfg, tmp.path());
            assert_eq!(resolved, tmp.path().join("models"));
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

/// Where embedding computation happens for ingest.
///
/// `Client` (default): this machine loads the model and embeds before writing.
///   Works for both Local and Remote backends; privacy-preserving since text
///   never leaves the device unembedded.
///
/// `Server`: the remote `crisp-index-server` embeds on arrival; this machine
///   only chunks text and posts raw strings. Requires `backend_type = Remote`
///   and a server build that has an embedder loaded (P11 step 5). When
///   `backend_type = Local`, setting this to `Server` is a no-op and the
///   local pipeline embeds as usual (local writes always go through the
///   IngestPipeline which owns the embedder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EmbedderLocation {
    #[default]
    Client,
    Server,
}
