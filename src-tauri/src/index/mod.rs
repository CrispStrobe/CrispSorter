pub mod embedder;
pub mod fts_index;
pub mod fts_query;
pub mod synonyms;
pub mod hf_prefetch;
pub mod ingest;
pub mod license_consent;
pub mod l2_metadata;
pub mod local_index;
pub mod ner;
pub mod omni_embed;
pub mod reranker;
pub mod skeleton;
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
pub mod config_persist;
pub mod migrations;
pub mod schema;
pub mod search;
pub mod snippet;
pub mod token_highlight;
pub mod summary;
pub mod nl_query;
pub mod result_cache;
pub mod barcode;
pub mod tauri_commands;
pub mod translate_commands;

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
pub use ner::{NerHandle, NerModel};
pub use reranker::{Reranker, RerankerHandle, RerankerModel};
pub use schema::{build_schema, DocumentChunk, SearchFilters, SearchResult};
pub use search::SearchEngine;
pub use snippet::{highlight_snippet, SNIPPET_WINDOW};

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
    /// A `.cidx` archive mounted for read-only offline browse. Set by
    /// `index_mount_cidx`, cleared by `index_unmount_cidx`. The Übersicht
    /// "Archiv" tab queries this instead of `local` when non-null.
    pub mounted_cidx: Option<std::sync::Arc<LocalIndex>>,
    /// Path of the currently mounted `.cidx` (for display in the UI).
    pub mounted_cidx_path: Option<String>,
    /// FTS index companion for the mounted `.cidx` (loaded from `{cidx}/fts/`
    /// if present). `None` when the .cidx was exported without `--include-fts`.
    pub mounted_cidx_fts: Option<std::sync::Arc<FtsIndex>>,
}

impl IndexState {
    pub fn disabled() -> Self {
        IndexState {
            backend: None,
            local: None,
            mounted_cidx: None,
            mounted_cidx_path: None,
            mounted_cidx_fts: None,
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
    /// Stage Z — alternate reranker for non-Latin-script queries (CJK,
    /// Arabic, Cyrillic, Devanagari, …).  When set AND the query is
    /// detected as predominantly non-Latin-script (≥ 25% of non-whitespace
    /// chars outside Latin + Latin Extended Unicode blocks), the search
    /// pipeline routes to this handle instead of `reranker_model`.
    /// If `reranker_model` is absent, this fires for all queries.
    /// `None` (default) = no script-aware routing.
    #[serde(default)]
    pub reranker_model_multilingual: Option<RerankerModel>,
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
    /// P13.5 follow-up — index-time translation target language
    /// (ISO 639-1, e.g. `"en"`).  When set, `bg_ingest` passes it
    /// to `ExtractOptions::translate_to`, the extractor's MT pass
    /// runs after text-LID, and the resulting translation is
    /// stored in the LanceDB `text_translated` + `text_translated_lang`
    /// columns (added by the `AddTextTranslatedColumns` migration).
    /// `None` (default) skips the translation pass entirely —
    /// existing behaviour, no overhead for users not opting in.
    ///
    /// Only the canonical pure-language codes are meaningful here
    /// (`en` / `de` / `ja` etc.); the MT backend is fixed at
    /// `m2m100` for the index-time path today — switching it
    /// out is a follow-up that exposes `translate_backend` /
    /// `translate_model` too.
    #[serde(default)]
    pub translate_to: Option<String>,
    /// Bi-encoder reranking via the loaded dense embedder.  When
    /// `true` AND no dedicated [`Self::reranker_model`] is set, the
    /// search pipeline reranks top-N RRF candidates by cosine
    /// similarity against the query — using the already-loaded
    /// dense backend's `rerank_biencoder` path (zero extra disk /
    /// memory).
    ///
    /// Falls back to no-rerank when neither this flag NOR a
    /// dedicated reranker is configured (preserves the historical
    /// default).  When BOTH are set, the dedicated cross-encoder
    /// reranker wins — it's more accurate per pair, this flag is
    /// for users who haven't / won't download a separate model.
    #[serde(default)]
    pub use_embedder_as_reranker: bool,

    // ── P19 — GLiNER named-entity recognition (index::ner) ──────────────
    /// Master switch for index-time NER.  When `true`, each document's
    /// `full_text` runs through the GLiNER model once (truncated to
    /// [`Self::ner_max_chars`]) and the resulting `"<label>:<text>"` entity
    /// tags are merged into the document's `tags` column.  Default `false`
    /// (opt-in) — NER adds per-doc latency.  GGUF-only via CrispEmbed; a
    /// no-op on builds without the `crispembed` feature.
    #[serde(default)]
    pub ner_enabled: bool,
    /// Which GLiNER model to use.  `None` falls back to
    /// [`NerModel::default`] (`sauerkraut-gliner-lfm`, German-tuned) when
    /// [`Self::ner_enabled`] is on.
    #[serde(default)]
    pub ner_model: Option<NerModel>,
    /// Zero-shot entity labels to extract.  Empty = [`ner::default_labels`].
    #[serde(default)]
    pub ner_labels: Vec<String>,
    /// Confidence threshold in `[0, 1]`; entities below this score are
    /// dropped.  Default 0.5.
    #[serde(default = "default_ner_threshold")]
    pub ner_threshold: f32,
    /// Cap on entity tags kept per document (top-N by score after dedup;
    /// 0 = unlimited).  Default 30 — prevents tag explosion.
    #[serde(default = "default_ner_max_entities")]
    pub ner_max_entities: usize,
    /// Truncate `full_text` to this many bytes before extraction (latency
    /// cap; 0 = no truncation).  Default 8000.
    #[serde(default = "default_ner_max_chars")]
    pub ner_max_chars: usize,
    /// P13.6 — master switch for audio + video extraction.  When
    /// `false`, bg_ingest skips audio/video extensions entirely
    /// (L1 metadata-only path).  When `true`, the audio extractor
    /// runs symphonia decode + CrispASR transcription per the
    /// canonical extractor pipeline.  Default `true` so binaries
    /// built with `crispasr-*` features get audio out of the box;
    /// users on feature-disabled builds can leave it on (the
    /// dispatcher's `is_audio_extraction_available()` gate kicks
    /// in first and falls back to L1 with a clear failure reason).
    #[serde(default = "default_audio_extraction_enabled")]
    pub audio_extraction_enabled: bool,
    /// ASR backend name (any string from `crispasr::list_known_models()`).
    /// `"whisper"` is the default — multilingual, 99 langs, base ~150 MB.
    /// Override to `"whisper-large-v3"` (3 GB) for higher accuracy or to
    /// `"parakeet"` / `"qwen3-omni"` for the alternative non-whisper
    /// backends.  Wired into `extractors::audio::shared_asr_handle()`
    /// — the value flows through `AsrConfig::new(...)` so registry
    /// resolution + auto-download happen on first use.
    #[serde(default = "default_audio_asr_backend")]
    pub audio_asr_backend: String,
    /// Audio LID method.  `"whisper"` (default) uses the loaded ASR
    /// model's built-in LID head and auto-resolves a model when
    /// needed (per `2b80345`).  `"silero"` / `"ecapa"` / `"firered"`
    /// require an explicit `--lid-model` path because those models
    /// aren't in CrispASR's registry yet — listed for forward
    /// compatibility when those entries land upstream.
    #[serde(default = "default_audio_lid_method")]
    pub audio_lid_method: String,
    /// P13.6 Step 6 — master switch for image extraction (OCR).
    /// When `false`, bg_ingest skips image extensions entirely
    /// (L1 metadata-only path).  Defaults to `true`; the actual
    /// OCR tier + per-language settings live in the `bg_ingest`
    /// fields (`ocr_enabled` / `ocr_tier` / `ocr_rec_lang`) which
    /// already have their own Settings panel.  This flag is the
    /// "single multimodal toggle" parallel to
    /// `audio_extraction_enabled` for users who want a one-knob
    /// shut-off.  Wired into bg_ingest in a follow-up; the field
    /// lands here now so the persisted IndexConfig shape is
    /// stable before the wire-up.
    #[serde(default = "default_image_extraction_enabled")]
    pub image_extraction_enabled: bool,
    /// P13.6 Step 9 placeholder — when enabled, images get a
    /// CrispEmbed embedding (BidirLM-Omni or fallback OCR-text
    /// embedding) so semantic image search hits work.  Today
    /// images only go through OCR + the text embedder.  Field
    /// reserved so the persisted IndexConfig shape stays stable
    /// when Step 9's pipeline lands.  `false` (default) preserves
    /// current behaviour.
    #[serde(default)]
    pub image_indexing_enabled: bool,
    /// P13.6 Step 7c — how deep the index goes on audio/video files.
    /// `L1` writes only filesystem metadata (path / size / mtime);
    /// `L2` additionally runs the cheap symphonia probe to populate
    /// the audio_* columns (no ASR); `L3` (default) runs the full
    /// decode → transcribe pipeline.  Mirrors the P11 cloud-drive
    /// L1/L2/L3 progression — promotes from L1 to L3 via the
    /// "Transcribe" search-result action (Step 8).
    #[serde(default)]
    pub ingest_audio_level: IngestAudioLevel,
    /// P13.7 Step 1 — how deep the index goes on image files.
    /// `L1` writes only filesystem metadata; `L2` runs the EXIF
    /// probe (kamadak-exif) and populates the image_* columns
    /// without OCR; `L3` (default) runs OCR + EXIF.  Promote via
    /// the "Re-OCR" search-result action (Step 2).  Parallel
    /// shape to `ingest_audio_level`.
    #[serde(default)]
    pub ingest_image_level: IngestImageLevel,
    /// P13.7 Step 4 — when enabled, the bg_ingest image path
    /// also pushes each indexed image to the configured CrispLens
    /// server (POST /api/ingest/upload-local) so the server's
    /// face-detection + people-clustering pipeline picks it up.
    /// Requires a working CrispLens Tier 2 session (login via the
    /// images_crisplens_login Tauri command).  Default `false`:
    /// users opt in explicitly because pushing every image
    /// upstream is a privacy-sensitive action.
    #[serde(default)]
    pub crisplens_image_enrichment_enabled: bool,
    /// P13.7 Step 5 — cloud-backup HTTP API base URL (e.g.
    /// `https://<crisplens-host>/cb`).  When set + the matching
    /// API key is in the OS keychain (per the
    /// [`crate::sync::secret`] module), the SyncManager can push
    /// manifests / embeddings to the VPS and pull deltas from
    /// other clients.  `None` (default) leaves the feature
    /// disabled — no network round-trips.
    #[serde(default)]
    pub cloud_backup_url: Option<String>,
    /// When true, the bg_ingest pipeline pushes each indexed
    /// document's L1 metadata to the configured cloud-backup VPS
    /// via [`crate::sync::cloud_backup::CloudBackupClient::manifest_push`].
    /// Default false — opt-in because uploading file paths +
    /// hashes to a remote server is a privacy-sensitive action.
    #[serde(default)]
    pub cloud_backup_push_manifests_enabled: bool,
    /// When true, the SyncManager also pushes already-computed
    /// embeddings (dense + sparse) to the VPS.  Default false:
    /// 1024-d × f32 × 100k chunks ≈ 400 MB, which is bandwidth-
    /// sensitive and only useful for the cross-device "phone
    /// hits the VPS for vector search" workflow.
    #[serde(default)]
    pub cloud_backup_push_embeddings_enabled: bool,
    /// When true, the SyncManager periodically pulls manifest
    /// deltas from the VPS and writes them as L1 rows into the
    /// local index (same shape as the existing P11 `sync_pull`
    /// for crisp-index-server).  Default false.
    #[serde(default)]
    pub cloud_backup_pull_manifests_enabled: bool,
    /// P13.7 Stage I — tiered-cache model.  When `false` (default),
    /// `sync_cb_manifest_pull` pulls metadata only — file paths,
    /// hashes, sizes, language, title, author, year — but omits
    /// the `full_text` body and embeddings, keeping the local
    /// LanceDB small enough to hold near-full metadata for a
    /// massive remote corpus.  When `true`, the body text rides
    /// along on every pull (heavier; OK on a low-row-count
    /// catalog or for users who want offline FTS over the full
    /// VPS catalog).  Search hits from `/api/v2/index/search`
    /// always carry `full_text` regardless of this flag — they're
    /// the on-demand promotion path into the local cache.
    #[serde(default)]
    pub cloud_backup_pull_full_text_enabled: bool,
    /// P13.7 Stage P — soft cap on the local LanceDB on-disk
    /// footprint in bytes.  `None` (default) = unbounded.  When set,
    /// a background 1-hour timer runs `purge_to_size`: oldest rows
    /// lose their `full_text` + `embedding` columns first; rows still
    /// over the cap are evicted entirely.  The CLI flag
    /// `crispsorter index purge --max-size N` runs an immediate pass.
    #[serde(default)]
    pub local_max_size_bytes: Option<u64>,
    /// Stage U — "thin client" switch.  When `false`, bg_ingest skips
    /// all extraction (text, audio, OCR) and writes L1-only rows:
    /// path + sha256 + mtime + size.  When cloud-backup push is also
    /// enabled, bg_ingest additionally enqueues a `cb_file_upload`
    /// outbox entry for each file so the actual bytes are shipped to
    /// the VPS; the VPS extraction worker then extracts full_text and
    /// pushes the enriched row back.  Default `true` (extraction on).
    #[serde(default = "default_local_extraction_enabled")]
    pub local_extraction_enabled: bool,

    /// Registry-driven embedder model override.  When non-empty and the
    /// backend is `Gguf`, the crispembed library resolves this name (a
    /// registry alias, e.g. "nomic-embed-text-v2.0" or "qwen3-0.6b") to
    /// a cached GGUF file and loads it, bypassing the `EmbedderModel` enum.
    /// The actual output dim is discovered at load time via `Embedder::dims()`.
    #[serde(default)]
    pub embedder_model_name: Option<String>,

    /// Stage W — skeleton-only mode.  When `true`, bg_ingest writes
    /// ONLY the two lightweight KV tables in `skeleton_index.db`
    /// (author_index + parent_dir_index).  No LanceDB rows, no FTS,
    /// no embedder.  Designed for laptops where the full corpus lives
    /// on the VPS and only quick author/dir hints are wanted locally.
    /// Implies `local_extraction_enabled = false` in practice; the
    /// bg_ingest early-return fires before the thin-client branch.
    #[serde(default)]
    pub local_skeleton_only: bool,
}

/// P13.7 Step 1 — how deeply bg_ingest processes images.
/// Parallel to [`IngestAudioLevel`].  Default L3 preserves the
/// existing OCR-on-image behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IngestImageLevel {
    /// Filesystem metadata only.  Image extractor is skipped
    /// entirely — no EXIF, no OCR.  Fastest path; rows are
    /// findable by filename / path but image_* + full_text stay
    /// NULL.
    L1,
    /// Run the EXIF probe only (kamadak-exif streams the header
    /// without touching pixel data).  Populates image_camera_*
    /// / image_taken_at_unix / image_iso.  No OCR — full_text
    /// stays "".
    L2,
    /// Full pipeline: EXIF + OCR (Tier 3 paddle → Tier 2 ocrs →
    /// Tier 1 tesseract per `bg_ingest.ocr_tier`).  Default,
    /// matches the pre-P13.7 behaviour.
    #[default]
    L3,
}

/// P13.6 Step 7c — how deeply bg_ingest processes audio/video.
/// Cheap-to-deep progression; UI dropdown surfaces `serde(rename_all
/// = "kebab-case")` strings ("l1" / "l2" / "l3") so the persisted
/// JSON stays human-readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IngestAudioLevel {
    /// Filesystem metadata only.  Audio extractor is skipped entirely
    /// — no probe, no transcribe.  Fastest path; rows are
    /// findable by filename / path but the audio_* columns + the
    /// full_text transcript stay NULL.
    L1,
    /// Run the symphonia probe (sub-ms) to populate the audio_*
    /// L2 columns (duration / codec / sample rate / channels /
    /// bitrate).  Still no ASR — the full_text stays NULL.
    L2,
    /// Full pipeline: symphonia probe AND ASR transcription.
    /// Default.  Matches the pre-P13.6 behaviour.
    #[default]
    L3,
}

fn default_use_vector() -> bool {
    true
}

fn default_rerank_top_n() -> usize {
    50
}

/// P19 — default GLiNER confidence threshold (Q6).
fn default_ner_threshold() -> f32 {
    0.5
}

/// P19 — default cap on entity tags per document (Q6).
fn default_ner_max_entities() -> usize {
    30
}

/// P19 — default `full_text` truncation before NER (Q5).
fn default_ner_max_chars() -> usize {
    8000
}

/// P13.6 — audio extraction is on by default on feature-enabled builds.
/// The runtime dispatcher's `is_audio_extraction_available()` gate
/// also fires for feature-disabled builds, so a `true` default here
/// is safe — no false-positive extraction attempts.
fn default_audio_extraction_enabled() -> bool {
    true
}

/// `whisper` — multilingual, 99 languages, base ~150 MB download.
/// Same default the legacy `AsrConfig::default()` carries; this
/// function exists so serde-defaulting on missing-field works
/// without instantiating an `AsrConfig` in a const context.
fn default_audio_asr_backend() -> String {
    "whisper".to_string()
}

/// `whisper` — reuses the loaded ASR model's LID head.  Auto-resolves
/// a whisper ggml via the helper added in `2b80345` when the ASR
/// backend is non-whisper-family.
fn default_audio_lid_method() -> String {
    "whisper".to_string()
}

/// P13.6 Step 6 — image OCR is on by default.  Matching the pattern
/// for [`default_audio_extraction_enabled`].
fn default_image_extraction_enabled() -> bool {
    true
}

fn default_local_extraction_enabled() -> bool {
    true
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
            reranker_model_multilingual: None,
            rerank_top_n: default_rerank_top_n(),
            ner_enabled: false,
            ner_model: None,
            ner_labels: Vec::new(),
            ner_threshold: default_ner_threshold(),
            ner_max_entities: default_ner_max_entities(),
            ner_max_chars: default_ner_max_chars(),
            model_cache_dir: None,
            matryoshka_dim: None,
            translate_to: None,
            use_embedder_as_reranker: false,
            audio_extraction_enabled: default_audio_extraction_enabled(),
            audio_asr_backend: default_audio_asr_backend(),
            audio_lid_method: default_audio_lid_method(),
            image_extraction_enabled: default_image_extraction_enabled(),
            image_indexing_enabled: false,
            ingest_audio_level: IngestAudioLevel::default(),
            ingest_image_level: IngestImageLevel::default(),
            crisplens_image_enrichment_enabled: false,
            cloud_backup_url: None,
            cloud_backup_push_manifests_enabled: false,
            cloud_backup_push_embeddings_enabled: false,
            cloud_backup_pull_manifests_enabled: false,
            cloud_backup_pull_full_text_enabled: false,
            local_max_size_bytes: None,
            local_extraction_enabled: true,
            local_skeleton_only: false,
            embedder_model_name: None,
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

    #[test]
    fn backend_type_serializes_to_documented_strings() {
        // These strings are what get persisted in app settings — changing
        // them silently would orphan every existing user's config.
        assert_eq!(serde_json::to_value(&BackendType::Local).unwrap(),  "local");
        assert_eq!(serde_json::to_value(&BackendType::Remote).unwrap(), "remote");
        assert_eq!(serde_json::to_value(&BackendType::Hybrid).unwrap(), "hybrid");

        // Parse back.
        let local: BackendType  = serde_json::from_str("\"local\"").unwrap();
        let remote: BackendType = serde_json::from_str("\"remote\"").unwrap();
        let hybrid: BackendType = serde_json::from_str("\"hybrid\"").unwrap();
        assert_eq!(local,  BackendType::Local);
        assert_eq!(remote, BackendType::Remote);
        assert_eq!(hybrid, BackendType::Hybrid);
    }

    #[test]
    fn backend_type_default_is_local() {
        let b: BackendType = Default::default();
        assert_eq!(b, BackendType::Local);
    }

    #[test]
    fn search_mode_serde_round_trip() {
        for m in [SearchMode::TextOnly, SearchMode::VectorOnly, SearchMode::Hybrid] {
            let json = serde_json::to_string(&m).unwrap();
            let back: SearchMode = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn search_mode_default_is_hybrid() {
        let m: SearchMode = Default::default();
        assert_eq!(m, SearchMode::Hybrid);
    }

    #[test]
    fn embedder_location_default_is_client() {
        let l: EmbedderLocation = Default::default();
        assert_eq!(l, EmbedderLocation::Client);
    }

    #[test]
    fn index_config_defaults_are_safe() {
        let cfg = IndexConfig::default();
        assert!(!cfg.enabled,                          "index disabled by default");
        assert_eq!(cfg.backend_type, BackendType::Local);
        assert_eq!(cfg.mode,         SearchMode::Hybrid);
        assert!(cfg.use_vector,                        "vector capabilities on by default");
        assert!(cfg.remote_url.is_none());
        assert!(cfg.reranker_model.is_none());
        assert_eq!(cfg.rerank_top_n, 50);
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

/// Runtime operating mode.
///
/// | Mode       | Reads              | Writes                     | When to use                      |
/// |------------|--------------------|----------------------------|----------------------------------|
/// | `Local`    | local LanceDB      | local only                 | single-machine (default)         |
/// | `Remote`   | remote server      | remote only via HTTP       | index on a VPS / GPU box         |
/// | `Hybrid`   | local-first        | local + mirror to remote   | laptop + VPS, offline capable    |
///
/// `Hybrid` reads prefer the local cache; on a cache miss it falls through to
/// the remote.  Writes go to the local store and are mirrored to the remote via
/// the SyncManager outbox (P11 Pillar 6).  Until SyncManager ships, `Hybrid`
/// behaves identically to `Local` — the variant is reserved so Settings can
/// persist it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendType {
    #[default]
    Local,
    Remote,
    /// Hybrid (local cache + remote authoritative). Reads local-first;
    /// writes mirror to remote via SyncManager outbox once that ships.
    Hybrid,
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
