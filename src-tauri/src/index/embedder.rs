/// Embedding model wrapper.
///
/// Two backends are used depending on the model:
///
///  1. **Fastembed** (`TextEmbedding`) — for fastembed native models (BgeM3,
///     MultilingualMiniLm) and custom models whose ONNX file is fully
///     self-contained (e.g. JinaV2Small/Base, SnowflakeArcticLv2
///     model_quantized, Octen INT4/INT8).
///
///  2. **OrtPath** (`OrtPathEmbedder`) — for any model whose ONNX file uses
///     external initializers (a companion `.onnx_data` file).  Loading from
///     bytes breaks those models; ORT must open the file on disk so it can
///     resolve the companion file by relative path.
///     Affected models: PixieRune-v1, JinaV5 Small/Nano, JinaV3,
///     Qwen3Embedding (fp32 variant).
///
/// The `OrtPathEmbedder` implements tokenisation with the `tokenizers` crate
/// (same library fastembed uses internally, so no extra download), runs the
/// ONNX session via `ort`, and applies mean-pooling + L2-normalisation.
///
/// Device / execution-provider selection is identical for both backends.
use std::path::{Path, PathBuf};
use anyhow::{Result, Context, bail};
use serde::{Deserialize, Serialize};
use fastembed::{
    EmbeddingModel,
    TextInitOptions,
    TextEmbedding,
    SparseTextEmbedding,
    SparseInitOptions,
    SparseModel,
    ExecutionProviderDispatch,
    UserDefinedEmbeddingModel,
    InitOptionsUserDefined,
    TokenizerFiles,
};
use hf_hub::api::tokio::ApiBuilder;
use tokenizers::{
    PaddingDirection, PaddingParams, PaddingStrategy,
    TruncationDirection, TruncationParams, TruncationStrategy,
};

// ── Model selection ────────────────────────────────────────────────────────

/// The embedding models offered in the Settings UI dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedderModel {
    /// BAAI/bge-m3 — 1024d, 8192-token context, multilingual.
    #[default]
    BgeM3,

    /// telepix/PIXIE-Rune-v1.0 — 1024d, 6144 ctx, 74 languages.
    PixieRuneV1,

    /// cstr/PIXIE-Rune-v1.0-ONNX INT8 — 1024d, 6k ctx, 74 languages, 542 MB.
    PixieRuneV1Q,

    /// cstr/PIXIE-Rune-v1.0-ONNX INT4+INT8 emb — 1024d, 6k ctx, 74 languages, 434 MB.
    PixieRuneV1Int4,

    /// cstr/PIXIE-Rune-v1.0-ONNX INT4 full — 1024d, 6k ctx, 74 languages, 337 MB.
    PixieRuneV1Int4Full,

    /// Snowflake/snowflake-arctic-embed-l-v2.0 INT8 quantized — 1024d, 8192 ctx.
    SnowflakeArcticLv2,

    /// Snowflake/snowflake-arctic-embed-l-v2.0 FP16 — 1024d, 8192 ctx.
    SnowflakeArcticLv2Fp16,

    /// Snowflake/snowflake-arctic-embed-l-v2.0 INT8 (model_int8) — 1024d, 8192 ctx.
    SnowflakeArcticLv2Int8,

    /// Snowflake/snowflake-arctic-embed-l-v2.0 Q4 — 1024d, 8192 ctx (smallest).
    SnowflakeArcticLv2Q4,

    /// Snowflake/snowflake-arctic-embed-l-v2.0 Q4F16 — 1024d, 8192 ctx.
    SnowflakeArcticLv2Q4F16,

    /// Snowflake/snowflake-arctic-embed-l-v2.0 O4 optimized — 1024d, 8192 ctx.
    SnowflakeArcticLv2O4,

    /// Snowflake/snowflake-arctic-embed-l-v2.0 FP32 — 1024d, 8192 ctx (reference, ~1.7 GB).
    SnowflakeArcticLv2Fp32,

    /// jinaai/jina-embeddings-v2-base-en — 768d, 8192 ctx.
    JinaV2Base,

    /// jinaai/jina-embeddings-v2-small-en — 512d, 8192 ctx.
    JinaV2Small,

    /// jinaai/jina-embeddings-v3 — 1024d, 8192 ctx, multilingual.
    JinaV3,

    /// jinaai/jina-embeddings-v5-text-small-retrieval — 1024d, 32k ctx.
    JinaV5Small,

    /// jinaai/jina-embeddings-v5-text-nano-retrieval — 768d, 8192 ctx.
    JinaV5Nano,

    /// paraphrase-multilingual-MiniLM-L12-v2 — 384d, fast.
    MultilingualMiniLm,

    // ── Qwen3-Embedding-0.6B (base model, decoder with KV-cache) ──────────────
    /// onnx-community/Qwen3-Embedding-0.6B-ONNX fp32 — 1024d, 32k ctx.
    Qwen3Embedding,

    /// onnx-community/Qwen3-Embedding-0.6B-ONNX int8 — 1024d, 32k ctx.
    Qwen3EmbeddingInt8,

    /// electroglyph/Qwen3-Embedding-0.6B-onnx-uint8 — 1024d, calibrated uint8.
    Qwen3EmbeddingUint8,

    // ── Octen-Embedding-0.6B — local torch.onnx.export of Octen/Octen-Embedding-0.6B ─
    /// Our own fp32 ONNX export — 1024d, 32k ctx, last-token pool.
    Octen06bFp32,

    /// Our own dynamic-INT8 ONNX export (MatMul-only quant, ~1.1 GB) — 1024d, 32k ctx.
    Octen06bInt8Local,

    /// Our own INT4 ONNX export (MatMulNBits block_size=32, ~900 MB) — 1024d, 32k ctx.
    Octen06bInt4Local,

    /// Our own dynamic-INT8 ONNX export including Gather (embedding table) — ~570 MB total.
    /// Smaller than Int8Local but the embedding lookup table is also quantized.
    Octen06bInt8FullLocal,
}

impl EmbedderModel {
    pub fn display_name(&self) -> &'static str {
        match self {
            EmbedderModel::BgeM3                => "BGE-M3 (8k ctx, Multilingual)",
            EmbedderModel::PixieRuneV1          => "PIXIE-Rune-v1.0 (6k ctx, 74 languages, FP32)",
            EmbedderModel::PixieRuneV1Q         => "PIXIE-Rune-v1.0 INT8 (6k ctx, 74 languages, 542 MB)",
            EmbedderModel::PixieRuneV1Int4      => "PIXIE-Rune-v1.0 INT4+INT8 emb (6k ctx, 74 languages, 434 MB)",
            EmbedderModel::PixieRuneV1Int4Full  => "PIXIE-Rune-v1.0 INT4 full (6k ctx, 74 languages, 337 MB)",
            EmbedderModel::SnowflakeArcticLv2       => "Snowflake Arctic-L v2.0 INT8-quant (8k ctx, 1024d)",
            EmbedderModel::SnowflakeArcticLv2Fp16   => "Snowflake Arctic-L v2.0 FP16 (8k ctx, 1024d)",
            EmbedderModel::SnowflakeArcticLv2Int8   => "Snowflake Arctic-L v2.0 INT8 (8k ctx, 1024d)",
            EmbedderModel::SnowflakeArcticLv2Q4     => "Snowflake Arctic-L v2.0 Q4 (8k ctx, 1024d, smallest)",
            EmbedderModel::SnowflakeArcticLv2Q4F16  => "Snowflake Arctic-L v2.0 Q4F16 (8k ctx, 1024d)",
            EmbedderModel::SnowflakeArcticLv2O4     => "Snowflake Arctic-L v2.0 O4 (8k ctx, 1024d)",
            EmbedderModel::SnowflakeArcticLv2Fp32   => "Snowflake Arctic-L v2.0 FP32 (8k ctx, 1024d, ~1.7 GB)",
            EmbedderModel::JinaV2Base           => "Jina-v2 Base EN (8k ctx, 768d)",
            EmbedderModel::JinaV2Small          => "Jina-v2 Small EN (8k ctx, 512d)",
            EmbedderModel::JinaV3               => "Jina-v3 (8k ctx, 1024d, Multilingual)",
            EmbedderModel::JinaV5Small          => "Jina-v5 Small (32k ctx, 1024d)",
            EmbedderModel::JinaV5Nano           => "Jina-v5 Nano (8k ctx, 768d)",
            EmbedderModel::MultilingualMiniLm   => "Multilingual MiniLM (Fast CPU, 384d)",
            EmbedderModel::Qwen3Embedding       => "Qwen3-Embedding-0.6B fp32 (32k ctx, 1024d)",
            EmbedderModel::Qwen3EmbeddingInt8   => "Qwen3-Embedding-0.6B int8 (32k ctx, 1024d)",
            EmbedderModel::Qwen3EmbeddingUint8  => "Qwen3-Embedding-0.6B uint8 calibrated (1024d)",
            EmbedderModel::Octen06bFp32         => "Octen-0.6B fp32 local export (1024d, last-token pool, ~2.4 GB)",
            EmbedderModel::Octen06bInt8Local    => "Octen-0.6B int8 local export (1024d, last-token pool, ~1.1 GB)",
            EmbedderModel::Octen06bInt4Local    => "Octen-0.6B int4 local export (1024d, last-token pool, ~900 MB)",
            EmbedderModel::Octen06bInt8FullLocal => "Octen-0.6B int8 full local export incl. embedding table (~570 MB)",
        }
    }

    pub fn dims(&self) -> usize {
        match self {
            EmbedderModel::MultilingualMiniLm => 384,
            EmbedderModel::JinaV2Small        => 512,
            EmbedderModel::JinaV2Base         => 768,
            EmbedderModel::JinaV5Nano         => 768,
            _                                 => 1024,
        }
    }

    pub fn max_tokens(&self) -> usize {
        match self {
            EmbedderModel::BgeM3              => 8192,
            EmbedderModel::PixieRuneV1
            | EmbedderModel::PixieRuneV1Q
            | EmbedderModel::PixieRuneV1Int4
            | EmbedderModel::PixieRuneV1Int4Full => 6144,
            EmbedderModel::SnowflakeArcticLv2
            | EmbedderModel::SnowflakeArcticLv2Fp16
            | EmbedderModel::SnowflakeArcticLv2Int8
            | EmbedderModel::SnowflakeArcticLv2Q4
            | EmbedderModel::SnowflakeArcticLv2Q4F16
            | EmbedderModel::SnowflakeArcticLv2O4
            | EmbedderModel::SnowflakeArcticLv2Fp32 => 8192,
            EmbedderModel::JinaV2Base         => 8192,
            EmbedderModel::JinaV2Small        => 8192,
            EmbedderModel::JinaV3             => 8192,
            EmbedderModel::JinaV5Small        => 32768,
            EmbedderModel::JinaV5Nano         => 8192,
            EmbedderModel::MultilingualMiniLm => 512,
            // Qwen3 base + Octen finetune: 32k ctx
            EmbedderModel::Qwen3Embedding
            | EmbedderModel::Qwen3EmbeddingInt8
            | EmbedderModel::Qwen3EmbeddingUint8
            | EmbedderModel::Octen06bFp32
            | EmbedderModel::Octen06bInt8Local
            | EmbedderModel::Octen06bInt4Local
            | EmbedderModel::Octen06bInt8FullLocal => 32768,
        }
    }

    pub fn has_multilingual_sparse(&self) -> bool {
        matches!(self, EmbedderModel::BgeM3)
    }

    /// Native fastembed models — loaded via `TextEmbedding::try_new` (no hf-hub fetch needed).
    pub fn is_native(&self) -> bool {
        matches!(self, EmbedderModel::BgeM3 | EmbedderModel::MultilingualMiniLm)
    }

    pub fn to_model_spec(&self) -> Option<ModelSpec> {
        match self {
            // ── external data (OrtPath backend) ─────────────────────────────
            EmbedderModel::PixieRuneV1 => Some(ModelSpec::new(
                "telepix/PIXIE-Rune-v1.0",
                "model.onnx",
            ).with_onnx_prefix("onnx/")
             .with_additional_files(vec!["model.onnx_data"])
             .with_onnx_data_prefix("onnx/")),

            EmbedderModel::JinaV3 => Some(ModelSpec::new(
                "jinaai/jina-embeddings-v3",
                "model.onnx",
            ).with_onnx_prefix("onnx/")
             .with_additional_files(vec!["model.onnx_data"])
             .with_onnx_data_prefix("onnx/")),

            EmbedderModel::JinaV5Small => Some(ModelSpec::new(
                "jinaai/jina-embeddings-v5-text-small-retrieval",
                "model.onnx",
            ).with_onnx_prefix("onnx/")
             .with_additional_files(vec!["model.onnx_data"])
             .with_onnx_data_prefix("onnx/")),

            EmbedderModel::JinaV5Nano => Some(ModelSpec::new(
                "jinaai/jina-embeddings-v5-text-nano-retrieval",
                "model_quantized.onnx",
            ).with_onnx_prefix("onnx/")
             .with_additional_files(vec!["model_quantized.onnx_data"])
             .with_onnx_data_prefix("onnx/")),

            // ── Qwen3-Embedding-0.6B (base decoder model, 28-layer KV-cache) ────────
            // onnx-community exports are full generative LM ONNX with past_key_values.
            // We pass empty [batch,8,0,128] KV tensors and use last-token pooling.
            EmbedderModel::Qwen3Embedding => Some(ModelSpec::new(
                "onnx-community/Qwen3-Embedding-0.6B-ONNX",
                "model.onnx",
            ).with_onnx_prefix("onnx/")
             .with_additional_files(vec!["model.onnx_data"])
             .with_onnx_data_prefix("onnx/")
             .with_kv_cache(8, 128)),

            EmbedderModel::Qwen3EmbeddingInt8 => Some(ModelSpec::new(
                "onnx-community/Qwen3-Embedding-0.6B-ONNX",
                "model_int8.onnx",
            ).with_onnx_prefix("onnx/")
             .with_kv_cache(8, 128)),

            // electroglyph's calibrated uint8 — pre-pooled uint8 output.
            // Dequant: range [-0.3009805, 0.3952634] → scale=0.002730, zero_point=110.
            EmbedderModel::Qwen3EmbeddingUint8 => Some(ModelSpec::new(
                "electroglyph/Qwen3-Embedding-0.6B-onnx-uint8",
                "dynamic_uint8.onnx",
            ).with_uint8_dequant(0.0027303685f32, 110)),

            // ── Octen-Embedding-0.6B (Qwen3 finetune by geoffsee) ────────────────────
            // These are encoder-style ONNX exports with built-in pooling.
            // Output: pre-pooled `embeddings [batch, 1024]` — no KV-cache needed.
            //
            // Octen06bFp32 / Octen06bInt8Local: our own torch.onnx.export of
            // Octen/Octen-Embedding-0.6B.  Inputs: input_ids, attention_mask →
            // last_hidden_state [batch, seq, 1024].  Uses last-token pooling.
            EmbedderModel::Octen06bFp32 => Some(ModelSpec::new(
                "Octen/Octen-Embedding-0.6B",  // informational only (local_subdir used instead)
                "model.onnx",
            ).with_local_subdir("octen-embedding-0.6b-onnx")
             .with_additional_files(vec!["model.onnx.data"])
             .force_last_token_pool()),

            // Dynamic INT8 quantisation of the same export (MatMul-only, ~1.1 GB).
            // Architecture identical to Fp32 — last-token pool, external data.
            EmbedderModel::Octen06bInt8Local => Some(ModelSpec::new(
                "Octen/Octen-Embedding-0.6B",  // informational only
                "model.int8.onnx",
            ).with_local_subdir("octen-embedding-0.6b-int8")
             .with_additional_files(vec!["model.int8.onnx.data"])
             .force_last_token_pool()),

            // MatMulNBits INT4 (block_size=32, symmetric) — ~900 MB.
            // Uses contrib op MatMulNBits; ORT resolves it automatically.
            EmbedderModel::Octen06bInt4Local => Some(ModelSpec::new(
                "Octen/Octen-Embedding-0.6B",  // informational only
                "model.int4.onnx",
            ).with_local_subdir("octen-embedding-0.6b-int4")
             .with_additional_files(vec!["model.int4.onnx.data"])
             .force_last_token_pool()),

            // INT8 with ALL node types quantized (MatMul + Gather) — ~570 MB total.
            // The embedding table (~600 MB FP32) is also quantized, saving ~450 MB vs Int8Local.
            // Stored inside the int8 directory so the tokenizer and config are shared.
            EmbedderModel::Octen06bInt8FullLocal => Some(ModelSpec::new(
                "Octen/Octen-Embedding-0.6B",  // informational only
                "model.int8_full.onnx",
            ).with_local_subdir("octen-embedding-0.6b-int8/model_int8_full")
             .with_additional_files(vec!["model.int8_full.onnx.data"])
             .force_last_token_pool()),

            // ── PIXIE-Rune-v1.0 quantized variants (cstr HF repo, self-contained) ─────
            EmbedderModel::PixieRuneV1Q => Some(ModelSpec::new(
                "cstr/PIXIE-Rune-v1.0-ONNX",
                "model_quantized.onnx",
            ).with_onnx_prefix("onnx/")),

            EmbedderModel::PixieRuneV1Int4 => Some(ModelSpec::new(
                "cstr/PIXIE-Rune-v1.0-ONNX",
                "model_int4.onnx",
            ).with_onnx_prefix("onnx/")),

            EmbedderModel::PixieRuneV1Int4Full => Some(ModelSpec::new(
                "cstr/PIXIE-Rune-v1.0-ONNX",
                "model_int4_full.onnx",
            ).with_onnx_prefix("onnx/")),

            // ── Snowflake Arctic Embed L v2.0 variants ────────────────────────────────
            EmbedderModel::SnowflakeArcticLv2 => Some(ModelSpec::new(
                "Snowflake/snowflake-arctic-embed-l-v2.0",
                "model_quantized.onnx",
            ).with_onnx_prefix("onnx/")),

            EmbedderModel::SnowflakeArcticLv2Fp16 => Some(ModelSpec::new(
                "Snowflake/snowflake-arctic-embed-l-v2.0",
                "model_fp16.onnx",
            ).with_onnx_prefix("onnx/")),

            EmbedderModel::SnowflakeArcticLv2Int8 => Some(ModelSpec::new(
                "Snowflake/snowflake-arctic-embed-l-v2.0",
                "model_int8.onnx",
            ).with_onnx_prefix("onnx/")),

            EmbedderModel::SnowflakeArcticLv2Q4 => Some(ModelSpec::new(
                "Snowflake/snowflake-arctic-embed-l-v2.0",
                "model_q4.onnx",
            ).with_onnx_prefix("onnx/")),

            EmbedderModel::SnowflakeArcticLv2Q4F16 => Some(ModelSpec::new(
                "Snowflake/snowflake-arctic-embed-l-v2.0",
                "model_q4f16.onnx",
            ).with_onnx_prefix("onnx/")),

            EmbedderModel::SnowflakeArcticLv2O4 => Some(ModelSpec::new(
                "Snowflake/snowflake-arctic-embed-l-v2.0",
                "model_O4.onnx",
            ).with_onnx_prefix("onnx/")),

            EmbedderModel::SnowflakeArcticLv2Fp32 => Some(ModelSpec::new(
                "Snowflake/snowflake-arctic-embed-l-v2.0",
                "model.onnx",
            ).with_onnx_prefix("onnx/")
             .with_additional_files(vec!["model.onnx_data"])
             .with_onnx_data_prefix("onnx/")),

            EmbedderModel::JinaV2Base => Some(ModelSpec::new(
                "jinaai/jina-embeddings-v2-base-en",
                "model.onnx",
            )),

            EmbedderModel::JinaV2Small => Some(ModelSpec::new(
                "jinaai/jina-embeddings-v2-small-en",
                "model.onnx",
            )),

            // Native fastembed models — no spec needed.
            EmbedderModel::BgeM3 | EmbedderModel::MultilingualMiniLm => None,
        }
    }

    fn to_fastembed_dense(&self) -> EmbeddingModel {
        match self {
            EmbedderModel::BgeM3               => EmbeddingModel::BGEM3,
            EmbedderModel::MultilingualMiniLm   => EmbeddingModel::ParaphraseMLMiniLML12V2,
            _ => EmbeddingModel::BGEM3,
        }
    }

    fn to_fastembed_sparse(&self) -> Option<SparseModel> {
        match self {
            EmbedderModel::BgeM3 => Some(SparseModel::BGEM3),
            _ => None,
        }
    }
}

// ── ModelSpec ──────────────────────────────────────────────────────────────

pub struct ModelSpec {
    pub repo: String,
    /// Path of the ONNX file within the repo (may include a subdirectory prefix like "onnx/").
    pub file: String,
    pub tokenizer_file: String,
    pub config_file: String,
    pub special_tokens_map_file: Option<String>,
    pub tokenizer_config_file: Option<String>,
    pub config_repo: Option<String>,
    /// Additional files to download (e.g. `.onnx_data`, constants).
    /// Files marked as external ONNX data must end with `.onnx_data`.
    pub additional_files: Vec<String>,
    /// Force OrtPath backend even for self-contained ONNX files.
    /// Use when the repo has no `config.json` (incompatible with fastembed UserDefined).
    pub use_ort_path: bool,
    /// KV-cache config for decoder models (e.g. Qwen3-Embedding).
    /// Set to (num_kv_heads, head_dim); 0,0 means no KV-cache.
    pub kv_cache_kv_heads: usize,
    pub kv_cache_head_dim: usize,
    /// Model already outputs a pre-pooled embedding [batch, dim] — skip pooling in OrtPath.
    pub force_pre_pooled: bool,
    /// uint8 output dequantization: `f32 = (u8 - zero_point) * scale`.
    /// None means output is already f32.
    pub dequant: Option<(f32, u8)>,
    /// When set, files are read from `{cache_dir}/{local_subdir}/{file}` — no hf-hub fetch.
    pub local_subdir: Option<String>,
    /// Use last-token pooling instead of mean pooling (for causal/decoder models without KV-cache).
    pub last_token_pool: bool,
}

impl ModelSpec {
    pub fn new(repo: &str, file: &str) -> Self {
        Self {
            repo: repo.to_owned(),
            file: file.to_owned(),
            tokenizer_file: "tokenizer.json".to_owned(),
            config_file: "config.json".to_owned(),
            special_tokens_map_file: Some("special_tokens_map.json".to_owned()),
            tokenizer_config_file: Some("tokenizer_config.json".to_owned()),
            config_repo: None,
            additional_files: Vec::new(),
            use_ort_path: false,
            kv_cache_kv_heads: 0,
            kv_cache_head_dim: 0,
            force_pre_pooled: false,
            dequant: None,
            local_subdir: None,
            last_token_pool: false,
        }
    }

    pub fn with_local_subdir(mut self, subdir: &str) -> Self {
        self.local_subdir = Some(subdir.to_owned());
        self.use_ort_path = true;
        self
    }

    pub fn force_last_token_pool(mut self) -> Self {
        self.last_token_pool = true;
        self
    }

    pub fn with_kv_cache(mut self, kv_heads: usize, head_dim: usize) -> Self {
        self.kv_cache_kv_heads = kv_heads;
        self.kv_cache_head_dim = head_dim;
        self.use_ort_path = true;
        self
    }

    pub fn force_pre_pooled(mut self) -> Self {
        self.force_pre_pooled = true;
        self.use_ort_path = true;
        self
    }

    /// Configure asymmetric uint8 dequantization: `f32 = (u8 - zero_point) * scale`.
    pub fn with_uint8_dequant(mut self, scale: f32, zero_point: u8) -> Self {
        self.dequant = Some((scale, zero_point));
        self.force_pre_pooled = true;
        self.use_ort_path = true;
        self
    }

    pub fn force_ort_path(mut self) -> Self {
        self.use_ort_path = true;
        self
    }

    pub fn with_tokenizer_prefix(mut self, prefix: &str) -> Self {
        self.tokenizer_file = format!("{}{}", prefix, self.tokenizer_file);
        if let Some(f) = self.special_tokens_map_file {
            self.special_tokens_map_file = Some(format!("{}{}", prefix, f));
        }
        if let Some(f) = self.tokenizer_config_file {
            self.tokenizer_config_file = Some(format!("{}{}", prefix, f));
        }
        self
    }

    pub fn with_onnx_prefix(mut self, prefix: &str) -> Self {
        self.file = format!("{}{}", prefix, self.file);
        self
    }

    /// Apply a prefix to `.onnx_data` files in `additional_files`.
    pub fn with_onnx_data_prefix(mut self, prefix: &str) -> Self {
        self.additional_files = self.additional_files.into_iter()
            .map(|f| if f.ends_with(".onnx_data") { format!("{}{}", prefix, f) } else { f })
            .collect();
        self
    }

    pub fn with_config_repo(mut self, repo: &str) -> Self {
        self.config_repo = Some(repo.to_owned());
        self
    }

    pub fn with_additional_files(mut self, files: Vec<&str>) -> Self {
        self.additional_files = files.into_iter().map(|s| s.to_owned()).collect();
        self
    }

    /// True when any additional file is an external ONNX data companion.
    /// Matches both `.onnx_data` (fastembed-style) and `.onnx.data` (HuggingFace-style).
    pub fn has_external_onnx_data(&self) -> bool {
        self.additional_files.iter().any(|f| f.ends_with(".onnx_data") || f.ends_with(".onnx.data"))
    }

    /// True when this model must use the OrtPath backend (external data OR no config.json).
    pub fn needs_ort_path(&self) -> bool {
        self.has_external_onnx_data() || self.use_ort_path
    }
}

// ── Download helpers ────────────────────────────────────────────────────────

struct ModelPaths {
    onnx:               PathBuf,
    tokenizer:          PathBuf,
    config:             Option<PathBuf>,
    special_tokens_map: Option<PathBuf>,
    tokenizer_config:   Option<PathBuf>,
}

/// Ensure all model files are on disk via hf-hub (re-uses cache on repeat calls).
/// `config.json` is fetched best-effort — some repos (e.g. Octen) don't have one.
/// When `spec.local_subdir` is set, files are read directly from `{cache_dir}/{subdir}/`
/// without any hf-hub network access.
async fn ensure_model_on_disk(spec: &ModelSpec, cache_dir: &Path) -> Result<ModelPaths> {
    if let Some(ref subdir) = spec.local_subdir {
        let base = cache_dir.join(subdir);
        let onnx      = base.join(&spec.file);
        let tokenizer = base.join(&spec.tokenizer_file);
        if !onnx.exists() {
            bail!("Local ONNX not found at {:?} — run the export script first", onnx);
        }
        if !tokenizer.exists() {
            bail!("Local tokenizer not found at {:?}", tokenizer);
        }
        return Ok(ModelPaths { onnx, tokenizer, config: None,
                               special_tokens_map: None, tokenizer_config: None });
    }

    let api = ApiBuilder::new()
        .with_cache_dir(cache_dir.to_path_buf())
        .build()
        .context("Failed to build hf-hub Api")?;

    let model_api = api.model(spec.repo.clone());

    println!("[embedder] Fetching ONNX: {}/{} …", spec.repo, spec.file);
    let onnx = model_api.get(&spec.file).await
        .context("Failed to get ONNX file")?;

    let tokenizer = model_api.get(&spec.tokenizer_file).await
        .context("Failed to get tokenizer.json")?;

    // config.json may not exist in all repos — non-fatal.
    let config = {
        let cfg_src = spec.config_repo.as_deref().unwrap_or(spec.repo.as_str());
        let api_for_cfg = if cfg_src != spec.repo {
            api.model(cfg_src.to_owned())
        } else {
            api.model(spec.repo.clone())
        };
        api_for_cfg.get(&spec.config_file).await.ok()
    };

    for f in &spec.additional_files {
        println!("[embedder] Fetching extra file: {} …", f);
        model_api.get(f).await.context(format!("Failed to get {}", f))?;
    }

    let special_tokens_map = if let Some(ref f) = spec.special_tokens_map_file {
        model_api.get(f).await.ok()
    } else { None };

    let tokenizer_config = if let Some(ref f) = spec.tokenizer_config_file {
        model_api.get(f).await.ok()
    } else { None };

    Ok(ModelPaths { onnx, tokenizer, config, special_tokens_map, tokenizer_config })
}

/// Download files and return them as bytes (for self-contained fastembed UserDefined models).
async fn fetch_model_bytes(spec: &ModelSpec, cache_dir: &Path) -> Result<(Vec<u8>, TokenizerFiles)> {
    let paths = ensure_model_on_disk(spec, cache_dir).await?;

    let onnx_bytes = std::fs::read(&paths.onnx).context("reading ONNX bytes")?;
    if onnx_bytes.len() < 1_000_000 && spec.additional_files.is_empty() {
        bail!(
            "ONNX file for {} is suspiciously small ({} B). Git-LFS pointer?",
            spec.repo, onnx_bytes.len()
        );
    }

    let read_opt = |p: &Option<PathBuf>| -> Vec<u8> {
        p.as_ref().and_then(|f| std::fs::read(f).ok()).unwrap_or_default()
    };

    let tokenizer_files = TokenizerFiles {
        tokenizer_file:       std::fs::read(&paths.tokenizer).context("reading tokenizer.json")?,
        config_file:          read_opt(&paths.config),
        special_tokens_map_file: read_opt(&paths.special_tokens_map),
        tokenizer_config_file:   read_opt(&paths.tokenizer_config),
    };

    Ok((onnx_bytes, tokenizer_files))
}

// ── Device selection ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedderDevice {
    #[default]
    Auto,
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

    pub fn execution_providers(&self) -> Vec<ExecutionProviderDispatch> {
        match self {
            EmbedderDevice::Cpu   => vec![],
            EmbedderDevice::Auto  => ep_auto(),
            EmbedderDevice::Metal => ep_metal(),
            EmbedderDevice::Cuda  => ep_cuda(),
        }
    }
}

fn ep_auto() -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "macos")]       { ep_metal() }
    #[cfg(not(target_os = "macos"))]  { ep_cuda()  }
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

// ── Config ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedderConfig {
    pub model:      EmbedderModel,
    pub device:     EmbedderDevice,
    pub cache_dir:  PathBuf,
    pub batch_size: usize,
}

impl EmbedderConfig {
    pub fn new(model: EmbedderModel, device: EmbedderDevice, cache_dir: PathBuf) -> Self {
        EmbedderConfig { model, device, cache_dir, batch_size: 32 }
    }
}

// ── Output types ───────────────────────────────────────────────────────────

pub struct DenseEmbedding {
    pub vectors: Vec<Vec<f32>>,
}

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

// ── OrtPathEmbedder ────────────────────────────────────────────────────────
//
// Used for models with external ONNX data (`.onnx_data` companion file).
// Loads the ONNX session from the file path so ORT can resolve external data
// by itself, then tokenises with the `tokenizers` crate and applies standard
// mean-pooling + L2 normalisation.

struct OrtPathEmbedder {
    session:         ort::session::Session,
    tokenizer:       tokenizers::Tokenizer,
    batch_size:      usize,
    dims:            usize,
    /// Whether the model accepts `token_type_ids` as input.
    has_type_ids:    bool,
    /// Whether the model accepts a `task_id` input (e.g. jina-embeddings-v3 LoRA).
    has_task_id:     bool,
    /// Whether the model requires explicit `position_ids` (Qwen3-based models).
    has_position_ids: bool,
    /// Name of the first output (cached to avoid borrow conflict with `run()`).
    first_output:    String,
    /// True when the model outputs a pre-pooled `sentence_embedding` tensor.
    pre_pooled:      bool,
    /// Number of KV-cache layer pairs (0 = no KV-cache, encoder model).
    kv_cache_layers:   usize,
    /// Number of KV heads per layer (e.g. 8 for Qwen3-0.6B).
    kv_cache_kv_heads: usize,
    /// Head dimension (e.g. 128 for Qwen3-0.6B).
    kv_cache_head_dim: usize,
    /// uint8 dequantization params: (scale, zero_point). None = output is float32.
    dequant: Option<(f32, u8)>,
    /// Use last-token pooling instead of mean pooling (decoder/causal without KV-cache).
    last_token_pool_mode: bool,
}

impl OrtPathEmbedder {
    fn load(
        onnx_path:            &Path,
        tok_path:             &Path,
        max_tokens:           usize,
        dims:                 usize,
        batch_size:           usize,
        eps:                  Vec<ExecutionProviderDispatch>,
        kv_cache_kv_heads:    usize,
        kv_cache_head_dim:    usize,
        force_pre_pooled:     bool,
        dequant:              Option<(f32, u8)>,
        last_token_pool_mode: bool,
    ) -> Result<Self> {
        // Build ORT session from file — ORT resolves `.onnx_data` automatically.
        let builder = ort::session::Session::builder()
            .context("ORT session builder")?;
        let builder = if eps.is_empty() {
            builder
        } else {
            builder.with_execution_providers(eps).context("setting EPs")?
        };
        let session = builder
            .commit_from_file(onnx_path)
            .with_context(|| format!("ORT failed to load {:?}", onnx_path))?;

        // Load tokenizer and configure padding + truncation.
        let mut tokenizer = tokenizers::Tokenizer::from_file(tok_path)
            .map_err(|e| anyhow::anyhow!("tokenizer load error: {e}"))?;

        let _ = tokenizer.with_truncation(Some(TruncationParams {
            direction:  TruncationDirection::Right,
            max_length: max_tokens.min(512), // tokenizers crate cap
            strategy:   TruncationStrategy::LongestFirst,
            stride:     0,
        }));

        tokenizer.with_padding(Some(PaddingParams {
            strategy:          PaddingStrategy::BatchLongest,
            direction:         PaddingDirection::Right,
            pad_to_multiple_of: None,
            pad_id:            0,
            pad_type_id:       0,
            pad_token:         "[PAD]".to_string(),
        }));

        let has_type_ids = session.inputs().iter()
            .any(|i: &ort::value::Outlet| i.name() == "token_type_ids");
        let has_task_id = session.inputs().iter()
            .any(|i: &ort::value::Outlet| i.name() == "task_id");
        let has_position_ids = session.inputs().iter()
            .any(|i: &ort::value::Outlet| i.name() == "position_ids");
        let kv_cache_layers = session.inputs().iter()
            .filter(|i: &&ort::value::Outlet| {
                i.name().starts_with("past_key_values.") && i.name().ends_with(".key")
            })
            .count();
        // Pre-pooled: model outputs [batch, dim] directly (no further pooling needed).
        // Detected either by output name or by the spec flag (for models with non-standard names).
        let pre_pooled = force_pre_pooled
            || session.outputs().iter()
               .any(|o: &ort::value::Outlet| {
                   let n = o.name();
                   n == "sentence_embedding" || n == "embeddings"
               });
        let first_output = session.outputs()[0].name().to_owned();

        println!("[embedder] OrtPath session ready — inputs: {:?}  outputs: {:?}  kv_cache_layers: {}",
            session.inputs().iter().map(|i: &ort::value::Outlet| i.name()).collect::<Vec<_>>(),
            session.outputs().iter().map(|o: &ort::value::Outlet| o.name()).collect::<Vec<_>>(),
            kv_cache_layers,
        );

        Ok(OrtPathEmbedder { session, tokenizer, batch_size, dims,
                             has_type_ids, has_task_id, has_position_ids, pre_pooled, first_output,
                             kv_cache_layers, kv_cache_kv_heads, kv_cache_head_dim, dequant,
                             last_token_pool_mode })
    }

    fn embed(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(self.batch_size) {
            let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
            out.extend(self.embed_batch(&refs)?);
        }
        Ok(out)
    }

    fn embed_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let batch = texts.len();

        let encodings = self.tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow::anyhow!("tokenizer batch error: {e}"))?;

        let seq_len = encodings[0].get_ids().len();

        // Flatten tensors — shape [batch, seq_len].
        let mut input_ids:    Vec<i64> = Vec::with_capacity(batch * seq_len);
        let mut attn_mask:    Vec<i64> = Vec::with_capacity(batch * seq_len);
        let mut token_type_ids: Vec<i64> = Vec::with_capacity(batch * seq_len);

        for enc in &encodings {
            input_ids.extend(enc.get_ids().iter().map(|&x| x as i64));
            attn_mask.extend(enc.get_attention_mask().iter().map(|&x| x as i64));
            token_type_ids.extend(enc.get_type_ids().iter().map(|&x| x as i64));
        }

        let ids_t   = ort::value::Tensor::<i64>::from_array(
            ([batch, seq_len], input_ids)
        ).context("input_ids tensor")?;
        let mask_t  = ort::value::Tensor::<i64>::from_array(
            ([batch, seq_len], attn_mask.clone())
        ).context("attention_mask tensor")?;
        let types_t = ort::value::Tensor::<i64>::from_array(
            ([batch, seq_len], token_type_ids)
        ).context("token_type_ids tensor")?;

        // task_id=1 → retrieval.passage (Jina-v3 LoRA adapter selection).
        let task_id_t = ort::value::Tensor::<i64>::from_array(
            ([batch], vec![1i64; batch])
        ).context("task_id tensor")?;

        // position_ids: [[0,1,...,seq_len-1], ...] repeated for each batch item (Qwen3).
        let pos_ids: Vec<i64> = (0..batch)
            .flat_map(|_| (0..seq_len as i64))
            .collect();
        let pos_ids_t = ort::value::Tensor::<i64>::from_array(
            ([batch, seq_len], pos_ids)
        ).context("position_ids tensor")?;

        // ── KV-cache decoder models (Qwen3-Embedding style) ────────────────────
        // Pass empty past_key_values tensors [batch, kv_heads, 0, head_dim] and
        // use last-token pooling (EOS token position = last non-padding token).
        if self.kv_cache_layers > 0 {
            let mut inputs: Vec<(std::borrow::Cow<str>, ort::value::DynValue)> = vec![
                ("input_ids".into(),     ids_t.upcast().into()),
                ("attention_mask".into(), mask_t.upcast().into()),
                ("position_ids".into(),  pos_ids_t.upcast().into()),
            ];
            // Build empty KV-cache tensors [batch, kv_heads, 0, head_dim].
            // ndarray supports zero-sized dimensions; ort's raw-data path does not.
            for layer in 0..self.kv_cache_layers {
                let k_empty = ort::value::Tensor::from_array(
                    ndarray::Array4::<f32>::zeros((batch, self.kv_cache_kv_heads, 0usize, self.kv_cache_head_dim))
                ).context("kv key tensor")?;
                let v_empty = ort::value::Tensor::from_array(
                    ndarray::Array4::<f32>::zeros((batch, self.kv_cache_kv_heads, 0usize, self.kv_cache_head_dim))
                ).context("kv val tensor")?;
                inputs.push((format!("past_key_values.{}.key", layer).into(), k_empty.upcast().into()));
                inputs.push((format!("past_key_values.{}.value", layer).into(), v_empty.upcast().into()));
            }
            let outputs = self.session.run(inputs)?;
            let (_shape, data) = outputs[self.first_output.as_str()]
                .try_extract_tensor::<f32>()
                .context("last_hidden_state extract (kv-cache)")?;
            let dim = self.dims;
            return Ok((0..batch).map(|i| {
                let toks     = &data[i * seq_len * dim .. (i+1) * seq_len * dim];
                let mask_row = &attn_mask[i * seq_len .. (i+1) * seq_len];
                l2_normalize(last_token_pool(toks, mask_row, seq_len, dim))
            }).collect());
        }

        // ── Encoder models ──────────────────────────────────────────────────────
        let outputs = match (self.has_type_ids, self.has_task_id, self.has_position_ids) {
            (true,  true,  _    ) => self.session.run(ort::inputs![
                "input_ids"       => ids_t,
                "attention_mask"  => mask_t,
                "token_type_ids"  => types_t,
                "task_id"         => task_id_t
            ])?,
            (true,  false, _    ) => self.session.run(ort::inputs![
                "input_ids"       => ids_t,
                "attention_mask"  => mask_t,
                "token_type_ids"  => types_t
            ])?,
            (false, true,  _    ) => self.session.run(ort::inputs![
                "input_ids"       => ids_t,
                "attention_mask"  => mask_t,
                "task_id"         => task_id_t
            ])?,
            (false, false, true ) => self.session.run(ort::inputs![
                "input_ids"       => ids_t,
                "attention_mask"  => mask_t,
                "position_ids"    => pos_ids_t
            ])?,
            (false, false, false) => self.session.run(ort::inputs![
                "input_ids"       => ids_t,
                "attention_mask"  => mask_t
            ])?,
        };

        if self.pre_pooled {
            // Model outputs a pre-pooled sentence embedding [batch, dim].
            let dim = self.dims;
            let f32_data: Vec<f32> = if let Some((scale, zp)) = self.dequant {
                // uint8 output — dequantize: f32 = (u8 - zero_point) * scale
                let (_shape, data) = outputs[self.first_output.as_str()]
                    .try_extract_tensor::<u8>()
                    .context("pre-pooled uint8 extract")?;
                data.iter().map(|&v| (v as i32 - zp as i32) as f32 * scale).collect()
            } else {
                let (_shape, data) = outputs[self.first_output.as_str()]
                    .try_extract_tensor::<f32>()
                    .context("pre-pooled f32 extract")?;
                data.to_vec()
            };
            Ok((0..batch).map(|i| l2_normalize(f32_data[i*dim..(i+1)*dim].to_vec())).collect())
        } else {
            // last_hidden_state: [batch, seq, dim] — apply mean or last-token pooling.
            let (_shape, data) = outputs[self.first_output.as_str()]
                .try_extract_tensor::<f32>()
                .context("last_hidden_state extract")?;
            let dim = self.dims;
            Ok((0..batch).map(|i| {
                let token_embs = &data[i * seq_len * dim .. (i+1) * seq_len * dim];
                let mask       = &attn_mask[i * seq_len .. (i+1) * seq_len];
                if self.last_token_pool_mode {
                    l2_normalize(last_token_pool(token_embs, mask, seq_len, dim))
                } else {
                    l2_normalize(mean_pool(token_embs, mask, seq_len, dim))
                }
            }).collect())
        }
    }
}

/// Last-token pooling for decoder/causal models (e.g. Qwen3-Embedding).
/// Takes the embedding at the last non-padding position (EOS token).
fn last_token_pool(token_embs: &[f32], mask: &[i64], seq_len: usize, dim: usize) -> Vec<f32> {
    let last_pos = mask.iter().rposition(|&m| m != 0).unwrap_or(seq_len - 1);
    token_embs[last_pos * dim .. (last_pos + 1) * dim].to_vec()
}

fn mean_pool(token_embs: &[f32], mask: &[i64], seq_len: usize, dim: usize) -> Vec<f32> {
    let mut pooled = vec![0.0f32; dim];
    let mut count  = 0.0f32;
    for t in 0..seq_len {
        if mask[t] != 0 {
            for d in 0..dim {
                pooled[d] += token_embs[t * dim + d];
            }
            count += 1.0;
        }
    }
    if count > 0.0 { pooled.iter_mut().for_each(|v| *v /= count); }
    pooled
}

fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 { v.iter_mut().for_each(|x| *x /= norm); }
    v
}

// ── DenseBackend ───────────────────────────────────────────────────────────

enum DenseBackend {
    Fastembed(TextEmbedding),
    OrtPath(OrtPathEmbedder),
}

// ── Embedder ────────────────────────────────────────────────────────────────

pub struct Embedder {
    config: EmbedderConfig,
    dense:  DenseBackend,
    sparse: Option<SparseTextEmbedding>,
}

impl Embedder {
    pub async fn new(config: EmbedderConfig) -> Result<Self> {
        let eps = config.device.execution_providers();

        let dense = if config.model.is_native() {
            // ── fastembed built-in model ────────────────────────────────────
            let opts = TextInitOptions::new(config.model.to_fastembed_dense())
                .with_cache_dir(config.cache_dir.clone())
                .with_show_download_progress(true)
                .with_execution_providers(eps.clone());
            DenseBackend::Fastembed(TextEmbedding::try_new(opts)?)

        } else {
            let spec = config.model.to_model_spec()
                .ok_or_else(|| anyhow::anyhow!("No model spec for {:?}", config.model))?;

            if spec.needs_ort_path() {
                // ── OrtPath: needed for external-data models OR repos without config.json ──
                let paths = ensure_model_on_disk(&spec, &config.cache_dir).await?;
                let emb = OrtPathEmbedder::load(
                    &paths.onnx,
                    &paths.tokenizer,
                    config.model.max_tokens(),
                    config.model.dims(),
                    config.batch_size,
                    eps.clone(),
                    spec.kv_cache_kv_heads,
                    spec.kv_cache_head_dim,
                    spec.force_pre_pooled,
                    spec.dequant,
                    spec.last_token_pool,
                )?;
                DenseBackend::OrtPath(emb)
            } else {
                // ── fastembed UserDefined: self-contained ONNX with config.json ──
                let (onnx_bytes, tokenizer_files) =
                    fetch_model_bytes(&spec, &config.cache_dir).await?;
                let model = UserDefinedEmbeddingModel::new(onnx_bytes, tokenizer_files);
                let opts  = InitOptionsUserDefined::new()
                    .with_execution_providers(eps.clone());
                DenseBackend::Fastembed(TextEmbedding::try_new_from_user_defined(model, opts)?)
            }
        };

        // Sparse head — only BgeM3 supports one today.
        let sparse = match config.model.to_fastembed_sparse() {
            Some(sm) => {
                let opts = SparseInitOptions::new(sm)
                    .with_cache_dir(config.cache_dir.clone())
                    .with_show_download_progress(true)
                    .with_execution_providers(eps);
                match SparseTextEmbedding::try_new(opts) {
                    Ok(m)  => Some(m),
                    Err(e) => {
                        eprintln!("[embedder] sparse model failed, skipping: {e}");
                        None
                    }
                }
            }
            None => None,
        };

        Ok(Embedder { config, dense, sparse })
    }

    pub fn embed_dense(&mut self, texts: Vec<String>) -> Result<DenseEmbedding> {
        let vectors = match &mut self.dense {
            DenseBackend::Fastembed(fe) => fe.embed(texts, Some(self.config.batch_size))?,
            DenseBackend::OrtPath(op)  => op.embed(texts)?,
        };
        Ok(DenseEmbedding { vectors })
    }

    pub fn embed_sparse(&mut self, texts: Vec<String>) -> Result<Vec<Option<SparseVector>>> {
        let n = texts.len();
        let Some(ref mut sm) = self.sparse else { return Ok(vec![None; n]); };
        let results = sm.embed(texts, Some(self.config.batch_size))?;
        Ok(results.into_iter().map(|sv| Some(SparseVector {
            indices: sv.indices.into_iter().map(|i| i as u32).collect(),
            values:  sv.values,
        })).collect())
    }

    pub fn embed_full(
        &mut self,
        texts: Vec<String>,
    ) -> Result<(DenseEmbedding, Vec<Option<SparseVector>>)> {
        let t2    = texts.clone();
        let dense  = self.embed_dense(texts)?;
        let sparse = self.embed_sparse(t2)?;
        Ok((dense, sparse))
    }

    pub fn dims(&self) -> usize          { self.config.model.dims() }
    pub fn model(&self) -> EmbedderModel { self.config.model }
    pub fn has_sparse(&self) -> bool     { self.sparse.is_some() }
}

// ── Chunking ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TextChunk {
    pub text:        String,
    pub start_char:  usize,
    pub end_char:    usize,
    pub chunk_index: i32,
}

/// Split `text` into overlapping word-boundary chunks.
/// `max_tokens` ≈ word count per chunk; `stride` = overlap word count.
pub fn chunk_text(
    text: &str,
    max_tokens: usize,
    stride: usize,
    _heading_offsets: &[usize],
) -> Vec<TextChunk> {
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut pos = 0usize;
    for word in text.split_ascii_whitespace() {
        if let Some(rel) = text[pos..].find(word) {
            let start = pos + rel;
            let end   = start + word.len();
            words.push((start, end));
            pos = end;
        }
    }
    if words.is_empty() { return vec![]; }

    let step = max_tokens.saturating_sub(stride).max(1);
    let mut chunks   = Vec::new();
    let mut word_pos = 0usize;
    let mut idx      = 0i32;

    while word_pos < words.len() {
        let end_word   = (word_pos + max_tokens).min(words.len()) - 1;
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
        assert_eq!(EmbedderModel::PixieRuneV1.dims(),        1024);
        assert_eq!(EmbedderModel::SnowflakeArcticLv2.dims(), 1024);
        assert_eq!(EmbedderModel::MultilingualMiniLm.dims(),  384);
        assert_eq!(EmbedderModel::JinaV5Nano.dims(),          768);
        assert_eq!(EmbedderModel::JinaV3.dims(),             1024);
    }

    #[test]
    fn max_tokens_correct() {
        assert_eq!(EmbedderModel::BgeM3.max_tokens(),              8192);
        assert_eq!(EmbedderModel::MultilingualMiniLm.max_tokens(), 512);
        assert_eq!(EmbedderModel::JinaV5Small.max_tokens(),        32768);
    }

    #[test]
    fn ort_path_detection() {
        // OrtPath models: external data or force_ort_path or KV-cache or pre-pooled
        assert!(EmbedderModel::JinaV5Nano.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::JinaV5Small.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::PixieRuneV1.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::JinaV3.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::Qwen3Embedding.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::Qwen3EmbeddingInt8.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::Qwen3EmbeddingUint8.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::Octen06bFp32.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::Octen06bInt8Local.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::Octen06bInt4Local.to_model_spec().unwrap().needs_ort_path());
        assert!(EmbedderModel::Octen06bInt8FullLocal.to_model_spec().unwrap().needs_ort_path());

        // Fastembed UserDefined backend (self-contained ONNX + config.json present)
        assert!(!EmbedderModel::JinaV2Small.to_model_spec().unwrap().needs_ort_path());
        assert!(!EmbedderModel::JinaV2Base.to_model_spec().unwrap().needs_ort_path());
        assert!(!EmbedderModel::SnowflakeArcticLv2.to_model_spec().unwrap().needs_ort_path());
    }

    #[test]
    fn external_data_detection() {
        // Models with external data companion files
        assert!(EmbedderModel::PixieRuneV1.to_model_spec().unwrap().has_external_onnx_data());
        assert!(EmbedderModel::JinaV3.to_model_spec().unwrap().has_external_onnx_data());
        assert!(EmbedderModel::Qwen3Embedding.to_model_spec().unwrap().has_external_onnx_data());
        assert!(EmbedderModel::Octen06bFp32.to_model_spec().unwrap().has_external_onnx_data());
        assert!(EmbedderModel::Octen06bInt8Local.to_model_spec().unwrap().has_external_onnx_data());
        assert!(EmbedderModel::Octen06bInt4Local.to_model_spec().unwrap().has_external_onnx_data());
        assert!(EmbedderModel::Octen06bInt8FullLocal.to_model_spec().unwrap().has_external_onnx_data());
        // Self-contained ONNX (no external data file)
        assert!(!EmbedderModel::Qwen3EmbeddingInt8.to_model_spec().unwrap().has_external_onnx_data());
    }

    #[test]
    fn sparse_mapping_correct() {
        assert_eq!(EmbedderModel::BgeM3.to_fastembed_sparse(), Some(SparseModel::BGEM3));
        assert_eq!(EmbedderModel::PixieRuneV1.to_fastembed_sparse(), None);
    }

    #[test]
    fn dense_mapping_correct() {
        assert!(matches!(EmbedderModel::BgeM3.to_fastembed_dense(), EmbeddingModel::BGEM3));
        assert!(matches!(
            EmbedderModel::MultilingualMiniLm.to_fastembed_dense(),
            EmbeddingModel::ParaphraseMLMiniLML12V2
        ));
    }

    #[test]
    fn chunking_overlap() {
        let words: Vec<_> = (0..200).map(|i| format!("w{:04}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, 100, 20, &[]);
        assert!(chunks.len() >= 2);
        assert!(chunks[1].start_char > chunks[0].start_char);
        assert!(chunks[1].start_char < chunks[0].end_char, "chunks should overlap");
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

    #[test]
    fn mean_pool_single_token() {
        let embs = vec![1.0f32, 2.0, 3.0];
        let mask = vec![1i64];
        let out  = mean_pool(&embs, &mask, 1, 3);
        assert_eq!(out, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn l2_normalize_unit() {
        let v   = vec![3.0f32, 4.0];
        let out = l2_normalize(v);
        let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }
}
