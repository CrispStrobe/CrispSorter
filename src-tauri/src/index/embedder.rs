/// Embedding model wrapper around `fastembed-rs` 5.x.
///
/// Model reality (as of fastembed 5.13 / 2026-03):
///
/// Dense models available:
///   BGEM3               — BAAI/bge-m3, 1024d, 8192-token context, 100+ languages ← primary
///   MultilingualE5Large — intfloat/multilingual-e5-large, 1024d, 512-token context
///   MultilingualE5Base  — intfloat/multilingual-e5-base, 768d, 512-token context
///   ParaphraseMLMiniLML12V2 — 384d, fast, good for VPS CPU
///   BGESmallENV15       — 384d, English-only
///
/// Sparse models available:
///   SparseModel::BGEM3      — BAAI/bge-m3 sparse head, multilingual ← use with BgeM3
///   SparseModel::SPLADEPPV1 — English-only SPLADE
///
/// INT8 / Q4 quantization:
///   fastembed provides pre-quantized INT8 ONNX for: BGESmallENV15Q, BGEBaseENV15Q,
///   BGELargeENV15Q, NomicEmbedTextV15Q, ParaphraseMLMiniLML12V2Q.
///   No BGEM3Q exists in the hub yet. For bge-m3 INT8, load a custom ONNX via
///   `try_new_from_user_defined` (see rag_plan.md §2).
///
/// Device selection: runtime-configurable (CoreML / CUDA / CPU).
/// Model weights cached in `{data_dir}/models/` and re-used across restarts.
use std::path::PathBuf;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use fastembed::{
    EmbeddingModel,
    TextInitOptions,
    TextEmbedding,
    SparseTextEmbedding,
    SparseInitOptions,
    SparseModel,
    ExecutionProviderDispatch,
};

// ── Model selection ────────────────────────────────────────────────────────

/// The embedding models offered in the Settings UI dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedderModel {
    /// BAAI/bge-m3 — 1024d, 8192-token context, 100+ languages (de+en excellent).
    /// Produces dense vectors + multilingual sparse vectors (SparseModel::BGEM3).
    /// Best overall choice for large German+English academic corpora.
    #[default]
    BgeM3,

    /// intfloat/multilingual-e5-large — 1024d, 512-token context.
    /// Good multilingual quality, lower memory than bge-m3.
    MultilingualE5Large,

    /// intfloat/multilingual-e5-base — 768d, 512-token context.
    /// Faster than Large, still solid multilingual.
    MultilingualE5Base,

    /// paraphrase-multilingual-MiniLM-L12-v2 — 384d, fast.
    /// Recommended for CPU-only VPS deployments.
    MultilingualMiniLm,

    /// BAAI/bge-small-en-v1.5 — 384d, English-only, smallest/fastest.
    /// Only suitable for English-only collections.
    BgeSmallEn,
}

impl EmbedderModel {
    pub fn display_name(&self) -> &'static str {
        match self {
            EmbedderModel::BgeM3               => "bge-m3 (de+en, 8192 ctx, recommended)",
            EmbedderModel::MultilingualE5Large  => "multilingual-e5-large (1024d)",
            EmbedderModel::MultilingualE5Base   => "multilingual-e5-base (768d)",
            EmbedderModel::MultilingualMiniLm   => "multilingual-MiniLM-L12 (384d, fast)",
            EmbedderModel::BgeSmallEn           => "bge-small-en (384d, English only)",
        }
    }

    /// Dense embedding output dimension.
    pub fn dims(&self) -> usize {
        match self {
            EmbedderModel::BgeM3              => 1024,
            EmbedderModel::MultilingualE5Large => 1024,
            EmbedderModel::MultilingualE5Base  => 768,
            EmbedderModel::MultilingualMiniLm  => 384,
            EmbedderModel::BgeSmallEn          => 384,
        }
    }

    /// Max useful input token count before truncation.
    pub fn max_tokens(&self) -> usize {
        match self {
            EmbedderModel::BgeM3 => 8192,
            _                    => 512,
        }
    }

    /// Whether this model has a matching multilingual sparse head.
    /// Only bge-m3 provides a multilingual SparseModel (SparseModel::BGEM3).
    /// Other models fall back to English-only SPLADE or no sparse.
    pub fn has_multilingual_sparse(&self) -> bool {
        matches!(self, EmbedderModel::BgeM3)
    }

    /// Map to fastembed EmbeddingModel (dense).
    fn to_fastembed_dense(&self) -> EmbeddingModel {
        match self {
            EmbedderModel::BgeM3               => EmbeddingModel::BGEM3,
            EmbedderModel::MultilingualE5Large  => EmbeddingModel::MultilingualE5Large,
            EmbedderModel::MultilingualE5Base   => EmbeddingModel::MultilingualE5Base,
            EmbedderModel::MultilingualMiniLm   => EmbeddingModel::ParaphraseMLMiniLML12V2,
            EmbedderModel::BgeSmallEn           => EmbeddingModel::BGESmallENV15,
        }
    }

    /// Map to fastembed SparseModel, if one is appropriate for this dense model.
    /// Returns None if no suitable sparse model exists.
    fn to_fastembed_sparse(&self) -> Option<SparseModel> {
        match self {
            // bge-m3 has its own multilingual sparse head — use it.
            EmbedderModel::BgeM3 => Some(SparseModel::BGEM3),
            // English-only models: SPLADE is acceptable for English content.
            EmbedderModel::BgeSmallEn => Some(SparseModel::SPLADEPPV1),
            // For multilingual models without a matching sparse head: no sparse.
            // Using English SPLADE against German text gives poor recall.
            _ => None,
        }
    }
}

// ── Device selection ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedderDevice {
    /// ORT picks the best available EP (CoreML on macOS, CUDA on Windows/Linux, CPU fallback).
    #[default]
    Auto,
    /// Force CPU-only (ONNX Runtime default).
    Cpu,
    /// Apple CoreML / Metal / Neural Engine (macOS only).
    Metal,
    /// NVIDIA CUDA.
    Cuda,
}

impl EmbedderDevice {
    pub fn display_name(&self) -> &'static str {
        match self {
            EmbedderDevice::Auto  => "Auto (recommended)",
            EmbedderDevice::Cpu   => "CPU",
            EmbedderDevice::Metal => "Metal (macOS)",
            EmbedderDevice::Cuda  => "CUDA (NVIDIA)",
        }
    }

    /// Build the ORT execution provider list for fastembed.
    /// ORT falls back gracefully to the next EP if the requested one is unavailable.
    pub fn execution_providers(&self) -> Vec<ExecutionProviderDispatch> {
        match self {
            EmbedderDevice::Cpu  => vec![],           // empty → ORT default = CPU
            EmbedderDevice::Auto  => ep_auto(),
            EmbedderDevice::Metal => ep_metal(),
            EmbedderDevice::Cuda  => ep_cuda(),
        }
    }
}

fn ep_auto() -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "macos")]   { ep_metal() }
    #[cfg(not(target_os = "macos"))] { ep_cuda() }
}

fn ep_metal() -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "macos")]
    { use ort::execution_providers::CoreMLExecutionProvider;
      vec![CoreMLExecutionProvider::default().build()] }
    #[cfg(not(target_os = "macos"))]
    { vec![] }
}

fn ep_cuda() -> Vec<ExecutionProviderDispatch> {
    #[cfg(not(target_os = "macos"))]
    { use ort::execution_providers::CUDAExecutionProvider;
      vec![CUDAExecutionProvider::default().build()] }
    #[cfg(target_os = "macos")]
    { vec![] }
}

// ── Config ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedderConfig {
    pub model:      EmbedderModel,
    pub device:     EmbedderDevice,
    /// Directory where ONNX model files are downloaded and cached.
    pub cache_dir:  PathBuf,
    /// Documents per forward pass. Larger = faster throughput, more RAM.
    pub batch_size: usize,
}

impl EmbedderConfig {
    pub fn new(model: EmbedderModel, device: EmbedderDevice, cache_dir: PathBuf) -> Self {
        EmbedderConfig { model, device, cache_dir, batch_size: 32 }
    }
}

// ── Output types ───────────────────────────────────────────────────────────

pub struct DenseEmbedding {
    /// One float32 vector per input text, length = model.dims().
    pub vectors: Vec<Vec<f32>>,
}

/// Sparse vector (bge-m3 SPLADE/lexical head output).
/// Stored as JSON in the `embedding_sparse` LanceDB column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseVector {
    pub indices: Vec<u32>,
    pub values:  Vec<f32>,
}

impl SparseVector {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "indices": self.indices, "values": self.values })
    }
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(SparseVector {
            indices: serde_json::from_value(v["indices"].clone()).ok()?,
            values:  serde_json::from_value(v["values"].clone()).ok()?,
        })
    }
}

// ── Embedder ───────────────────────────────────────────────────────────────

pub struct Embedder {
    config: EmbedderConfig,
    dense:  TextEmbedding,
    sparse: Option<SparseTextEmbedding>,
}

// fastembed 5.x changed TextEmbedding::embed / SparseTextEmbedding::embed to &mut self,
// so the public methods below must take &mut self.

impl Embedder {
    /// Initialise embedder, downloading ONNX weights on first run.
    /// Call from a Tauri async background task — may be slow on first launch.
    pub fn new(config: EmbedderConfig) -> Result<Self> {
        let eps = config.device.execution_providers();

        let dense_opts = TextInitOptions::new(config.model.to_fastembed_dense())
            .with_cache_dir(config.cache_dir.clone())
            .with_show_download_progress(true)
            .with_execution_providers(eps.clone());

        let dense = TextEmbedding::try_new(dense_opts)?;

        // Sparse model: only load if a suitable one exists for the chosen dense model.
        let sparse = match config.model.to_fastembed_sparse() {
            Some(sparse_model) => {
                let sparse_opts = SparseInitOptions::new(sparse_model)
                    .with_cache_dir(config.cache_dir.clone())
                    .with_show_download_progress(true)
                    .with_execution_providers(eps);
                match SparseTextEmbedding::try_new(sparse_opts) {
                    Ok(m)  => Some(m),
                    Err(e) => {
                        eprintln!("[embedder] sparse model load failed, continuing without: {e}");
                        None
                    }
                }
            }
            None => None,
        };

        Ok(Embedder { config, dense, sparse })
    }

    pub fn embed_dense(&mut self, texts: Vec<String>) -> Result<DenseEmbedding> {
        let vectors = self.dense.embed(texts, Some(self.config.batch_size))?;
        Ok(DenseEmbedding { vectors })
    }

    pub fn embed_sparse(&mut self, texts: Vec<String>) -> Result<Vec<Option<SparseVector>>> {
        let n = texts.len();
        let Some(ref mut sm) = self.sparse else {
            return Ok(vec![None; n]);
        };
        let results = sm.embed(texts, Some(self.config.batch_size))?;
        Ok(results.into_iter().map(|sv| Some(SparseVector {
            indices: sv.indices.into_iter().map(|i| i as u32).collect(),
            values:  sv.values,
        })).collect())
    }

    /// Embed dense + sparse in one call.
    pub fn embed_full(
        &mut self,
        texts: Vec<String>,
    ) -> Result<(DenseEmbedding, Vec<Option<SparseVector>>)> {
        let t2 = texts.clone();
        let dense  = self.embed_dense(texts)?;
        let sparse = self.embed_sparse(t2)?;
        Ok((dense, sparse))
    }

    pub fn dims(&self) -> usize          { self.config.model.dims() }
    pub fn model(&self) -> EmbedderModel { self.config.model }
    pub fn has_sparse(&self) -> bool     { self.sparse.is_some() }
}

// ── Chunking ───────────────────────────────────────────────────────────────

/// A single text chunk ready for embedding.
#[derive(Debug, Clone)]
pub struct TextChunk {
    pub text:        String,
    pub start_char:  usize,
    pub end_char:    usize,
    pub chunk_index: i32,
}

/// Split `text` into overlapping chunks aligned to word boundaries.
///
/// `max_tokens` — approximate word count per chunk (word-based proxy for tokens)
/// `stride`     — word overlap between consecutive chunks (must be < max_tokens)
///
/// When bge-m3 is the active model, use `max_tokens = 1500` (≈ 2000 BPE tokens)
/// and `stride = 200`. For 512-token models use `max_tokens = 350`, `stride = 50`.
///
/// `_heading_offsets`: reserved for future heading-boundary alignment (byte offsets
/// of section headings in the original text, used to snap chunk starts).
pub fn chunk_text(
    text: &str,
    max_tokens: usize,
    stride: usize,
    _heading_offsets: &[usize],
) -> Vec<TextChunk> {
    // Collect (start_byte, end_byte) for each whitespace-delimited word.
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut search_from = 0usize;
    for word in text.split_ascii_whitespace() {
        if let Some(rel) = text[search_from..].find(word) {
            let start = search_from + rel;
            let end   = start + word.len();
            words.push((start, end));
            search_from = end;
        }
    }

    if words.is_empty() { return vec![]; }

    let step = max_tokens.saturating_sub(stride).max(1);
    let mut chunks = Vec::new();
    let mut word_pos = 0usize;
    let mut idx = 0i32;

    while word_pos < words.len() {
        let end_word  = (word_pos + max_tokens).min(words.len()) - 1;
        let start_char = words[word_pos].0;
        let end_char   = words[end_word].1;

        chunks.push(TextChunk {
            text:        text[start_char..end_char].to_owned(),
            start_char,
            end_char,
            chunk_index: idx,
        });

        idx      += 1;
        word_pos += step;
    }

    chunks
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_dims_correct() {
        assert_eq!(EmbedderModel::BgeM3.dims(),              1024);
        assert_eq!(EmbedderModel::MultilingualE5Large.dims(), 1024);
        assert_eq!(EmbedderModel::MultilingualE5Base.dims(),  768);
        assert_eq!(EmbedderModel::MultilingualMiniLm.dims(),  384);
        assert_eq!(EmbedderModel::BgeSmallEn.dims(),          384);
    }

    #[test]
    fn max_tokens_correct() {
        assert_eq!(EmbedderModel::BgeM3.max_tokens(),              8192);
        assert_eq!(EmbedderModel::MultilingualE5Large.max_tokens(), 512);
        assert_eq!(EmbedderModel::BgeSmallEn.max_tokens(),          512);
    }

    #[test]
    fn sparse_mapping_correct() {
        // bge-m3 gets multilingual sparse
        assert_eq!(EmbedderModel::BgeM3.to_fastembed_sparse(), Some(SparseModel::BGEM3));
        // English-only small gets SPLADE
        assert_eq!(EmbedderModel::BgeSmallEn.to_fastembed_sparse(), Some(SparseModel::SPLADEPPV1));
        // Multilingual E5 models: no suitable sparse model (English SPLADE hurts German recall)
        assert_eq!(EmbedderModel::MultilingualE5Large.to_fastembed_sparse(), None);
        assert_eq!(EmbedderModel::MultilingualMiniLm.to_fastembed_sparse(), None);
    }

    #[test]
    fn dense_mapping_correct() {
        assert!(matches!(EmbedderModel::BgeM3.to_fastembed_dense(), EmbeddingModel::BGEM3));
        assert!(matches!(
            EmbedderModel::MultilingualE5Large.to_fastembed_dense(),
            EmbeddingModel::MultilingualE5Large
        ));
        assert!(matches!(
            EmbedderModel::MultilingualE5Base.to_fastembed_dense(),
            EmbeddingModel::MultilingualE5Base
        ));
        assert!(matches!(
            EmbedderModel::MultilingualMiniLm.to_fastembed_dense(),
            EmbeddingModel::ParaphraseMLMiniLML12V2
        ));
        assert!(matches!(
            EmbedderModel::BgeSmallEn.to_fastembed_dense(),
            EmbeddingModel::BGESmallENV15
        ));
    }

    #[test]
    fn chunking_overlap() {
        let words: Vec<_> = (0..200).map(|i| format!("w{:04}", i)).collect();
        let text = words.join(" ");
        // max=100 stride=20 → step=80
        let chunks = chunk_text(&text, 100, 20, &[]);
        assert!(chunks.len() >= 2, "should produce multiple chunks");
        // Chunk 1 starts later than chunk 0 but has overlap
        assert!(chunks[1].start_char > chunks[0].start_char);
        assert!(chunks[1].start_char < chunks[0].end_char,
            "chunks should overlap");
    }

    #[test]
    fn chunking_single_chunk_for_short_text() {
        let chunks = chunk_text("hello world foo bar", 100, 20, &[]);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn chunking_preserves_text() {
        let text = "the quick brown fox jumps over the lazy dog";
        let chunks = chunk_text(text, 5, 1, &[]);
        // Every chunk should be a substring of the original
        for c in &chunks {
            assert!(text.contains(c.text.as_str()));
        }
    }

    #[test]
    fn sparse_vector_json_roundtrip() {
        let sv  = SparseVector { indices: vec![1, 5, 10], values: vec![0.8, 0.3, 0.5] };
        let sv2 = SparseVector::from_json(&sv.to_json()).unwrap();
        assert_eq!(sv.indices, sv2.indices);
        assert_eq!(sv.values,  sv2.values);
    }

    #[test]
    fn cpu_device_empty_eps() {
        assert!(EmbedderDevice::Cpu.execution_providers().is_empty());
    }
}
