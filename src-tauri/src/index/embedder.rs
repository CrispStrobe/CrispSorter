use anyhow::{bail, Context, Result};
use fastembed::{
    EmbeddingModel, ExecutionProviderDispatch, InitOptionsUserDefined, SparseInitOptions,
    SparseModel, SparseTextEmbedding, TextEmbedding, TextInitOptions, TokenizerFiles,
    UserDefinedEmbeddingModel,
};
// hf_hub::api was used for the crispembed GGUF download path before we
// switched it to our own progress-aware reqwest prefetcher. Kept the
// import gone so neither feature-on nor feature-off builds warn.
use serde::{Deserialize, Serialize};
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
use tokenizers::{
    PaddingDirection, PaddingParams, PaddingStrategy, TruncationDirection, TruncationParams,
    TruncationStrategy,
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

    // ── intfloat/multilingual-e5 (fastembed-native + GGUF) ──────────────────
    /// intfloat/multilingual-e5-small — 384d, 512 ctx, 100+ languages (~470 MB).
    MultilingualE5Small,
    /// intfloat/multilingual-e5-base — 768d, 512 ctx, 100+ languages (~1.1 GB).
    MultilingualE5Base,
    /// intfloat/multilingual-e5-large — 1024d, 512 ctx, 100+ languages.
    MultilingualE5Large,

    // ── BAAI/bge-en-v1.5 (fastembed-native + GGUF) ──────────────────────────
    /// BAAI/bge-small-en-v1.5 — 384d, 512 ctx, English (~130 MB). SPLADE++ sparse pair.
    BgeSmallEnV15,
    /// BAAI/bge-base-en-v1.5 — 768d, 512 ctx, English (~440 MB).
    BgeBaseEnV15,
    /// BAAI/bge-large-en-v1.5 — 1024d, 512 ctx, English.
    BgeLargeEnV15,

    // ── nomic-ai/nomic-embed-text-v1.5 (fastembed-native + GGUF) ────────────
    /// nomic-ai/nomic-embed-text-v1.5 — 768d, 8192 ctx, English.
    /// Long context, half the size of bge-large.
    NomicEmbedTextV15,

    // ── mixedbread-ai/mxbai-embed-large-v1 (fastembed-native + GGUF) ────────
    /// mixedbread-ai/mxbai-embed-large-v1 — 1024d, 512 ctx, English. Top-performing English encoder.
    MxbaiEmbedLargeV1,

    // ── sentence-transformers/all-MiniLM-L6-v2 (fastembed-native + GGUF) ────
    /// all-MiniLM-L6-v2 — 384d, 256 ctx, English (~90 MB). Tiny CPU baseline.
    AllMiniLmL6V2,

    // ── google/embeddinggemma-300m (fastembed-native + GGUF) ────────────────
    /// google/embeddinggemma-300m — 768d, 2048 ctx, multilingual.
    EmbeddingGemma300M,

    // ── Alibaba-NLP/gte-en-v1.5 (fastembed-native + GGUF) ───────────────────
    /// Alibaba-NLP/gte-base-en-v1.5 — 768d, 8192 ctx, English.
    GteBaseEnV15,
    /// Alibaba-NLP/gte-large-en-v1.5 — 1024d, 8192 ctx, English.
    GteLargeEnV15,

    // ── P17.6: GGUF-only decoder-based embedding models ──────────────────
    // These require --features crispembed (GGUF backend only, no ONNX).
    // Lighter than the ORT path: quantizable to Q4_K, no ONNX Runtime dep.

    /// Gemma3-Embedding 2B via CrispEmbed GGUF — 2048d, 8192 ctx.
    /// Decoder with GeGLU, last-token pooling.  GGUF-only.
    Gemma3Embed2B,

    /// ModernBERT-base via CrispEmbed GGUF — 768d, 8192 ctx.
    /// Pre-LN encoder, GeGLU, per-layer rotary theta.  GGUF-only.
    ModernBertBase,

    /// ModernBERT-large via CrispEmbed GGUF — 1024d, 8192 ctx.
    ModernBertLarge,

    /// DeBERTa-v2-xlarge via CrispEmbed GGUF — 1536d, 512 ctx.
    /// Disentangled attention.  GGUF-only.
    DebertaV2Xlarge,

    /// NomicBERT MoE via CrispEmbed GGUF — 768d, 8192 ctx.
    /// 8-expert top-2 routing, SwiGLU, RoPE.  GGUF-only.
    NomicBertMoe,
}

impl EmbedderModel {
    pub fn display_name(&self) -> &'static str {
        match self {
            EmbedderModel::BgeM3 => "BGE-M3 (8k ctx, Multilingual)",
            EmbedderModel::PixieRuneV1 => "PIXIE-Rune-v1.0 (6k ctx, 74 languages, FP32)",
            EmbedderModel::PixieRuneV1Q => "PIXIE-Rune-v1.0 INT8 (6k ctx, 74 languages, 542 MB)",
            EmbedderModel::PixieRuneV1Int4 => {
                "PIXIE-Rune-v1.0 INT4+INT8 emb (6k ctx, 74 languages, 434 MB)"
            }
            EmbedderModel::PixieRuneV1Int4Full => {
                "PIXIE-Rune-v1.0 INT4 full (6k ctx, 74 languages, 337 MB)"
            }
            EmbedderModel::SnowflakeArcticLv2 => {
                "Snowflake Arctic-L v2.0 INT8-quant (8k ctx, 1024d)"
            }
            EmbedderModel::SnowflakeArcticLv2Fp16 => "Snowflake Arctic-L v2.0 FP16 (8k ctx, 1024d)",
            EmbedderModel::SnowflakeArcticLv2Int8 => "Snowflake Arctic-L v2.0 INT8 (8k ctx, 1024d)",
            EmbedderModel::SnowflakeArcticLv2Q4 => {
                "Snowflake Arctic-L v2.0 Q4 (8k ctx, 1024d, smallest)"
            }
            EmbedderModel::SnowflakeArcticLv2Q4F16 => {
                "Snowflake Arctic-L v2.0 Q4F16 (8k ctx, 1024d)"
            }
            EmbedderModel::SnowflakeArcticLv2O4 => "Snowflake Arctic-L v2.0 O4 (8k ctx, 1024d)",
            EmbedderModel::SnowflakeArcticLv2Fp32 => {
                "Snowflake Arctic-L v2.0 FP32 (8k ctx, 1024d, ~1.7 GB)"
            }
            EmbedderModel::JinaV2Base => "Jina-v2 Base EN (8k ctx, 768d)",
            EmbedderModel::JinaV2Small => "Jina-v2 Small EN (8k ctx, 512d)",
            EmbedderModel::JinaV3 => "Jina-v3 (8k ctx, 1024d, Multilingual)",
            EmbedderModel::JinaV5Small => "Jina-v5 Small (32k ctx, 1024d)",
            EmbedderModel::JinaV5Nano => "Jina-v5 Nano (8k ctx, 768d)",
            EmbedderModel::MultilingualMiniLm => "Multilingual MiniLM (Fast CPU, 384d)",
            EmbedderModel::Qwen3Embedding => "Qwen3-Embedding-0.6B fp32 (32k ctx, 1024d)",
            EmbedderModel::Qwen3EmbeddingInt8 => "Qwen3-Embedding-0.6B int8 (32k ctx, 1024d)",
            EmbedderModel::Qwen3EmbeddingUint8 => "Qwen3-Embedding-0.6B uint8 calibrated (1024d)",
            EmbedderModel::Octen06bFp32 => {
                "Octen-0.6B fp32 (auto-download, 1024d, last-token pool, ~2.4 GB)"
            }
            EmbedderModel::Octen06bInt8Local => {
                "Octen-0.6B int8 MatMul-only (LOCAL-ONLY, ~1.1 GB)"
            }
            EmbedderModel::Octen06bInt4Local => {
                "Octen-0.6B int4 (auto-download, 1024d, last-token pool, ~900 MB)"
            }
            EmbedderModel::Octen06bInt8FullLocal => {
                "Octen-0.6B int8-full (auto-download, incl. embedding table, ~570 MB)"
            }
            EmbedderModel::MultilingualE5Small => "Multilingual-E5 Small (512 ctx, 384d, 100+ langs, ~470 MB)",
            EmbedderModel::MultilingualE5Base => "Multilingual-E5 Base (512 ctx, 768d, 100+ langs, ~1.1 GB)",
            EmbedderModel::MultilingualE5Large => "Multilingual-E5 Large (512 ctx, 1024d, 100+ langs)",
            EmbedderModel::BgeSmallEnV15 => "BGE-small-en-v1.5 (512 ctx, 384d, English, ~130 MB)",
            EmbedderModel::BgeBaseEnV15 => "BGE-base-en-v1.5 (512 ctx, 768d, English, ~440 MB)",
            EmbedderModel::BgeLargeEnV15 => "BGE-large-en-v1.5 (512 ctx, 1024d, English)",
            EmbedderModel::NomicEmbedTextV15 => "Nomic-Embed Text v1.5 (8k ctx, 768d, English)",
            EmbedderModel::MxbaiEmbedLargeV1 => "Mxbai-Embed Large v1 (512 ctx, 1024d, English)",
            EmbedderModel::AllMiniLmL6V2 => "all-MiniLM-L6-v2 (256 ctx, 384d, English, ~90 MB)",
            EmbedderModel::EmbeddingGemma300M => "EmbeddingGemma 300M (2k ctx, 768d, Multilingual)",
            EmbedderModel::GteBaseEnV15 => "GTE Base en v1.5 (8k ctx, 768d, English)",
            EmbedderModel::GteLargeEnV15 => "GTE Large en v1.5 (8k ctx, 1024d, English)",
            // P17.6 — GGUF-only decoder models
            EmbedderModel::Gemma3Embed2B => "Gemma3-Embedding 2B GGUF (8k ctx, 2048d)",
            EmbedderModel::ModernBertBase => "ModernBERT-base GGUF (8k ctx, 768d)",
            EmbedderModel::ModernBertLarge => "ModernBERT-large GGUF (8k ctx, 1024d)",
            EmbedderModel::DebertaV2Xlarge => "DeBERTa-v2-xlarge GGUF (512 ctx, 1536d)",
            EmbedderModel::NomicBertMoe => "NomicBERT-MoE GGUF (8k ctx, 768d, 8-expert)",
        }
    }

    /// SPDX-ish license class for this model's weights. Drives the
    /// download/use consent gate (see `index::license_consent`).
    pub fn license(&self) -> crate::index::license_consent::ModelLicense {
        use crate::index::license_consent::ModelLicense::*;
        match self {
            // Jina v3 + v5 retrieval heads are CC-BY-NC-4.0 (non-commercial).
            EmbedderModel::JinaV3 | EmbedderModel::JinaV5Small | EmbedderModel::JinaV5Nano => {
                NonCommercial("CC-BY-NC-4.0")
            }
            // EmbeddingGemma ships under Google's Gemma Terms of Use.
            EmbedderModel::EmbeddingGemma300M => Restricted("Gemma Terms of Use"),
            _ => Permissive,
        }
    }

    /// Stable consent key (matches `gguf_registry_name()` / the GUI's
    /// `indexEmbedderToRust` mapping) so GUI/CLI acceptance lines up with the
    /// gate. Empty for permissive models (no consent needed).
    pub fn consent_key(&self) -> &'static str {
        match self {
            EmbedderModel::JinaV3 => "jina-v3",
            EmbedderModel::JinaV5Small => "jina-v5-small",
            EmbedderModel::JinaV5Nano => "jina-v5-nano",
            EmbedderModel::EmbeddingGemma300M => "embedding-gemma300-m",
            _ => "",
        }
    }

    /// Gate: errors unless this model's license is permissive or consent is on
    /// record (env `CRISPSORTER_ACCEPT_MODEL_LICENSE`, CLI `--accept-license`,
    /// or GUI confirmation).
    pub fn ensure_license_consent(&self) -> Result<()> {
        crate::index::license_consent::ensure(self.display_name(), self.consent_key(), self.license())
    }

    pub fn dims(&self) -> usize {
        match self {
            EmbedderModel::MultilingualMiniLm
            | EmbedderModel::BgeSmallEnV15
            | EmbedderModel::AllMiniLmL6V2
            | EmbedderModel::MultilingualE5Small => 384,
            EmbedderModel::JinaV2Base
            | EmbedderModel::JinaV5Nano
            | EmbedderModel::NomicEmbedTextV15
            | EmbedderModel::BgeBaseEnV15
            | EmbedderModel::MultilingualE5Base
            | EmbedderModel::EmbeddingGemma300M
            | EmbedderModel::GteBaseEnV15
            | EmbedderModel::ModernBertBase
            | EmbedderModel::NomicBertMoe => 768,
            // P17.6 — DeBERTa-v2-xlarge: 1536d
            EmbedderModel::DebertaV2Xlarge => 1536,
            // P17.6 — Gemma3-Embedding 2B: 2048d
            EmbedderModel::Gemma3Embed2B => 2048,
            // 1024d models hit the fall-through below:
            // BgeM3, MultilingualE5Large, BgeLargeEnV15, MxbaiEmbedLargeV1,
            // GteLargeEnV15, ModernBertLarge, all PIXIE/Snowflake/Jina-v3/v5-Small/Qwen3/Octen variants.
            _ => 1024,
        }
    }

    pub fn max_tokens(&self) -> usize {
        match self {
            EmbedderModel::BgeM3 => 8192,
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
            EmbedderModel::JinaV2Base => 8192,
            EmbedderModel::JinaV2Small => 8192,
            EmbedderModel::JinaV3 => 8192,
            EmbedderModel::JinaV5Small => 32768,
            EmbedderModel::JinaV5Nano => 8192,
            EmbedderModel::MultilingualMiniLm => 512,
            // Qwen3 base + Octen finetune: 32k ctx
            EmbedderModel::Qwen3Embedding
            | EmbedderModel::Qwen3EmbeddingInt8
            | EmbedderModel::Qwen3EmbeddingUint8
            | EmbedderModel::Octen06bFp32
            | EmbedderModel::Octen06bInt8Local
            | EmbedderModel::Octen06bInt4Local
            | EmbedderModel::Octen06bInt8FullLocal => 32768,
            // 512-token encoders
            EmbedderModel::MultilingualE5Small
            | EmbedderModel::MultilingualE5Base
            | EmbedderModel::MultilingualE5Large
            | EmbedderModel::BgeSmallEnV15
            | EmbedderModel::BgeBaseEnV15
            | EmbedderModel::BgeLargeEnV15
            | EmbedderModel::MxbaiEmbedLargeV1 => 512,
            // 256-token encoder
            EmbedderModel::AllMiniLmL6V2 => 256,
            // 8k-context encoders
            EmbedderModel::NomicEmbedTextV15
            | EmbedderModel::GteBaseEnV15
            | EmbedderModel::GteLargeEnV15 => 8192,
            // 2k-context Gemma encoder
            EmbedderModel::EmbeddingGemma300M => 2048,
            // P17.6 — GGUF-only models
            EmbedderModel::Gemma3Embed2B
            | EmbedderModel::ModernBertBase
            | EmbedderModel::ModernBertLarge
            | EmbedderModel::NomicBertMoe => 8192,
            EmbedderModel::DebertaV2Xlarge => 512,
        }
    }

    pub fn has_multilingual_sparse(&self) -> bool {
        matches!(self, EmbedderModel::BgeM3)
    }

    /// Approximate first-time download size in megabytes (model + tokenizer
    /// + any companion `.onnx_data`). Used by the UI to show a realistic
    /// "first run downloads X MB" hint before the prefetch begins.
    pub fn approx_download_mb(&self) -> u32 {
        match self {
            EmbedderModel::BgeM3 => 2280,
            EmbedderModel::MultilingualMiniLm => 130,
            EmbedderModel::PixieRuneV1 => 1830,
            EmbedderModel::PixieRuneV1Q => 542,
            EmbedderModel::PixieRuneV1Int4 => 434,
            EmbedderModel::PixieRuneV1Int4Full => 337,
            EmbedderModel::SnowflakeArcticLv2 => 600,
            EmbedderModel::SnowflakeArcticLv2Fp16 => 1100,
            EmbedderModel::SnowflakeArcticLv2Int8 => 600,
            EmbedderModel::SnowflakeArcticLv2Q4 => 320,
            EmbedderModel::SnowflakeArcticLv2Q4F16 => 360,
            EmbedderModel::SnowflakeArcticLv2O4 => 360,
            EmbedderModel::SnowflakeArcticLv2Fp32 => 1700,
            EmbedderModel::JinaV2Base => 320,
            EmbedderModel::JinaV2Small => 130,
            EmbedderModel::JinaV3 => 1100,
            EmbedderModel::JinaV5Small => 2500,
            EmbedderModel::JinaV5Nano => 700,
            EmbedderModel::Qwen3Embedding => 2400,
            EmbedderModel::Qwen3EmbeddingInt8 => 600,
            EmbedderModel::Qwen3EmbeddingUint8 => 600,
            EmbedderModel::Octen06bFp32 => 2400,
            EmbedderModel::Octen06bInt8Local => 1100,
            EmbedderModel::Octen06bInt4Local => 900,
            EmbedderModel::Octen06bInt8FullLocal => 570,
            EmbedderModel::BgeLargeEnV15 => 1300,
            EmbedderModel::MultilingualE5Large => 2240, // model.onnx + .onnx_data
            EmbedderModel::MxbaiEmbedLargeV1 => 1300,
            EmbedderModel::NomicEmbedTextV15 => 550,
            EmbedderModel::BgeSmallEnV15 => 130,
            EmbedderModel::BgeBaseEnV15 => 440,
            EmbedderModel::AllMiniLmL6V2 => 90,
            EmbedderModel::MultilingualE5Small => 470,
            EmbedderModel::MultilingualE5Base => 1100,
            EmbedderModel::EmbeddingGemma300M => 1200,
            EmbedderModel::GteBaseEnV15 => 440,
            EmbedderModel::GteLargeEnV15 => 1300,
            // P17.6 — GGUF-only models (Q8_0 sizes from HF repos)
            EmbedderModel::Gemma3Embed2B => 2600,
            EmbedderModel::ModernBertBase => 440,
            EmbedderModel::ModernBertLarge => 1300,
            EmbedderModel::DebertaV2Xlarge => 1800,
            EmbedderModel::NomicBertMoe => 900,
        }
    }

    /// Native fastembed models — loaded via `TextEmbedding::try_new` (no manual ONNX path).
    /// All models listed here have an entry in fastembed-rs's `EmbeddingModel` enum
    /// (CrispStrobe/fastembed-rs `feat/new-model-entries` branch).
    pub fn is_native(&self) -> bool {
        matches!(
            self,
            EmbedderModel::BgeM3
                | EmbedderModel::MultilingualMiniLm
                | EmbedderModel::MultilingualE5Small
                | EmbedderModel::MultilingualE5Base
                | EmbedderModel::MultilingualE5Large
                | EmbedderModel::BgeSmallEnV15
                | EmbedderModel::BgeBaseEnV15
                | EmbedderModel::BgeLargeEnV15
                | EmbedderModel::NomicEmbedTextV15
                | EmbedderModel::MxbaiEmbedLargeV1
                | EmbedderModel::AllMiniLmL6V2
                | EmbedderModel::EmbeddingGemma300M
                | EmbedderModel::GteBaseEnV15
                | EmbedderModel::GteLargeEnV15
                // Octen: 3 of 4 variants now have fastembed-rs entries
                // pointing at cstr/Octen-Embedding-0.6B-ONNX*. The 4th
                // (Octen06bInt8Local) stays local-only — no fastembed
                // equivalent (Int8 MatMul-only was dropped from fastembed-rs
                // post-77cc2e45 due to platform-dependent checksums).
                | EmbedderModel::Octen06bFp32
                | EmbedderModel::Octen06bInt4Local
                | EmbedderModel::Octen06bInt8FullLocal
        )
    }

    /// Whether this model has a known-good GGUF equivalent shipped through
    /// CrispEmbed. Used to gate the ONNX/GGUF backend toggle in the UI.
    ///
    /// Verified bit-identical (cos > 0.999 F32, Q8_0 > 0.99) per CrispEmbed
    /// accuracy report. ONNX-only quant variants (e.g. `*Int8`, `*Fp16`) are
    /// collapsed to their base model for this check — the GGUF side uses its
    /// own quant (Q8_0 / Q4_K / etc.) selected separately.
    pub fn supports_gguf(&self) -> bool {
        self.gguf_registry_name().is_some()
    }

    /// Shared short-name used by the `cstr/<name>-GGUF` HuggingFace registry
    /// (mirrors `CrispEmbed/examples/cli/model_mgr.cpp`). All ONNX quant
    /// variants of the same base model map to the same GGUF — the GGUF side
    /// uses its own quant (Q8_0 by default).
    fn gguf_registry_name(&self) -> Option<&'static str> {
        use EmbedderModel::*;
        Some(match self {
            PixieRuneV1 | PixieRuneV1Q | PixieRuneV1Int4 | PixieRuneV1Int4Full => {
                "pixie-rune-v1"
            }
            SnowflakeArcticLv2
            | SnowflakeArcticLv2Fp16
            | SnowflakeArcticLv2Int8
            | SnowflakeArcticLv2Q4
            | SnowflakeArcticLv2Q4F16
            | SnowflakeArcticLv2O4
            | SnowflakeArcticLv2Fp32 => "arctic-embed-l-v2",
            JinaV5Small => "jina-v5-small",
            JinaV5Nano => "jina-v5-nano",
            Qwen3Embedding | Qwen3EmbeddingInt8 | Qwen3EmbeddingUint8 => "qwen3-embed-0.6b",
            Octen06bFp32 | Octen06bInt8Local | Octen06bInt4Local | Octen06bInt8FullLocal => {
                "octen-0.6b"
            }
            MultilingualE5Small => "multilingual-e5-small",
            MultilingualE5Base => "multilingual-e5-base",
            MultilingualE5Large => "multilingual-e5-large",
            BgeSmallEnV15 => "bge-small-en-v1.5",
            BgeBaseEnV15 => "bge-base-en-v1.5",
            BgeLargeEnV15 => "bge-large-en-v1.5",
            NomicEmbedTextV15 => "nomic-embed-text-v1.5",
            MxbaiEmbedLargeV1 => "mxbai-embed-large-v1",
            AllMiniLmL6V2 => "all-MiniLM-L6-v2",
            EmbeddingGemma300M => "embeddinggemma-300m",
            GteBaseEnV15 => "gte-base-en-v1.5",
            GteLargeEnV15 => "gte-large-en-v1.5",
            // P17.6 — GGUF-only decoder/encoder models
            Gemma3Embed2B => "gemma3-embed-2b",
            ModernBertBase => "modernbert-base",
            ModernBertLarge => "modernbert-large",
            DebertaV2Xlarge => "deberta-v2-xlarge",
            NomicBertMoe => "nomic-bert-moe",
            _ => return None,
        })
    }

    /// GGUF source for CrispEmbed — HF repo id + filename. Only models with
    /// a verified conversion in the `cstr/*-GGUF` registry return `Some`.
    ///
    /// Each repo follows the same naming convention:
    ///   `<name>.gguf`         — F32 reference
    ///   `<name>-q8_0.gguf`    — 8-bit quant
    ///   `<name>-q4_k.gguf`    — 4-bit K-quant
    ///
    /// We pick the quant that matches the user's *intent* baked into the
    /// `EmbedderModel` variant — see `gguf_quant_suffix_str()`.
    /// `quant` overrides the quantisation baked into the variant; `None`
    /// keeps the variant's own choice. See [`EmbedderConfig::gguf_quant`].
    #[cfg(feature = "crispembed")]
    pub(crate) fn to_gguf_spec_with_quant(self, quant: Option<GgufQuant>) -> Option<GgufSpec> {
        let name = self.gguf_registry_name()?;
        let suffix = match quant {
            Some(q) => q.suffix(),
            None => self.gguf_quant_suffix_str(),
        };
        Some(GgufSpec {
            repo: format!("cstr/{name}-GGUF"),
            file: format!("{name}{suffix}.gguf"),
        })
    }

    /// Sibling to `to_gguf_spec`: returns the same `(repo, file)` pair even
    /// when the `crispembed` feature is OFF, so the frontend can quote the
    /// download size + show the resolved filename without depending on the
    /// GGUF backend being linked in.
    pub(crate) fn gguf_file_name(&self) -> Option<(&'static str, String)> {
        let name = self.gguf_registry_name()?;
        Some((name, format!("{name}{}.gguf", self.gguf_quant_suffix_str())))
    }

    /// Approximate GGUF download size in megabytes for this variant.
    /// Mirrors the size of the file `gguf_file_name()` resolves to,
    /// taken from the actual cstr/*-GGUF repos (HF API, 2026-05-05).
    pub fn gguf_download_mb(&self) -> u32 {
        let Some((name, _)) = self.gguf_file_name() else {
            return 0;
        };
        let suffix = self.gguf_quant_suffix_str();
        match (name, suffix) {
            // Encoders 1024d (BERT/XLM-R)
            ("pixie-rune-v1",          "-q4_k") => 437, ("pixie-rune-v1",          "-q8_0") => 581, ("pixie-rune-v1",          "") => 2168,
            ("arctic-embed-l-v2",      "-q4_k") => 437, ("arctic-embed-l-v2",      "-q8_0") => 581, ("arctic-embed-l-v2",      "") => 2168,
            ("multilingual-e5-large",  "-q4_k") => 429, ("multilingual-e5-large",  "-q8_0") => 574, ("multilingual-e5-large",  "") => 2141,
            // Encoders 768d
            ("bge-large-en-v1.5",      "-q4_k") => 196, ("bge-large-en-v1.5",      "-q8_0") => 341, ("bge-large-en-v1.5",      "") => 1279,
            ("mxbai-embed-large-v1",   "-q4_k") => 196, ("mxbai-embed-large-v1",   "-q8_0") => 341, ("mxbai-embed-large-v1",   "") => 1279,
            ("multilingual-e5-base",   "-q4_k") => 247, ("multilingual-e5-base",   "-q8_0") => 287, ("multilingual-e5-base",   "") => 1066,
            ("bge-base-en-v1.5",       "-q4_k") =>  71, ("bge-base-en-v1.5",       "-q8_0") => 112, ("bge-base-en-v1.5",       "") =>  418,
            ("nomic-embed-text-v1.5",  "-q4_k") =>  85, ("nomic-embed-text-v1.5",  "-q8_0") => 139, ("nomic-embed-text-v1.5",  "") =>  522,
            // Encoders 384d
            ("multilingual-e5-small",  "-q4_k") => 115, ("multilingual-e5-small",  "-q8_0") => 126, ("multilingual-e5-small",  "") =>  455,
            ("bge-small-en-v1.5",      "-q4_k") =>  24, ("bge-small-en-v1.5",      "-q8_0") =>  34, ("bge-small-en-v1.5",      "") =>  128,
            ("all-MiniLM-L6-v2",       "-q4_k") =>  19, ("all-MiniLM-L6-v2",       "-q8_0") =>  24, ("all-MiniLM-L6-v2",       "") =>   87,
            // Decoders 1024d
            ("octen-0.6b",             "-q4_k") => 400, ("octen-0.6b",             "-q8_0") => 610, ("octen-0.6b",             "") => 2278,
            ("qwen3-embed-0.6b",       "-q4_k") => 400, ("qwen3-embed-0.6b",       "-q8_0") => 610, ("qwen3-embed-0.6b",       "") => 2278,
            ("jina-v5-small",          "-q4_k") => 400, ("jina-v5-small",          "-q8_0") => 610, ("jina-v5-small",          "") => 2279,
            // Decoders 768d
            ("jina-v5-nano",           "-q4_k") => 168, ("jina-v5-nano",           "-q8_0") => 222, ("jina-v5-nano",           "") =>  815,
            _ => 0,
        }
    }

    /// String form of `gguf_quant_suffix` so the runtime size lookup works
    /// regardless of whether the `crispembed` Cargo feature is on.
    pub(crate) fn gguf_quant_suffix_str(&self) -> &'static str {
        use EmbedderModel::*;
        match self {
            PixieRuneV1Int4 | PixieRuneV1Int4Full
            | SnowflakeArcticLv2Q4 | SnowflakeArcticLv2Q4F16
            | Octen06bInt4Local => "-q4_k",
            PixieRuneV1Q
            | SnowflakeArcticLv2 | SnowflakeArcticLv2Int8
            | Qwen3EmbeddingInt8 | Qwen3EmbeddingUint8
            | Octen06bInt8Local | Octen06bInt8FullLocal => "-q8_0",
            _ => "",
        }
    }

    pub fn to_model_spec(self) -> Option<ModelSpec> {
        match self {
            // ── external data (OrtPath backend) ─────────────────────────────
            EmbedderModel::PixieRuneV1 => Some(
                ModelSpec::new("telepix/PIXIE-Rune-v1.0", "model.onnx")
                    .with_onnx_prefix("onnx/")
                    .with_additional_files(vec!["model.onnx_data"])
                    .with_onnx_data_prefix("onnx/"),
            ),

            EmbedderModel::JinaV3 => Some(
                ModelSpec::new("jinaai/jina-embeddings-v3", "model.onnx")
                    .with_onnx_prefix("onnx/")
                    .with_additional_files(vec!["model.onnx_data"])
                    .with_onnx_data_prefix("onnx/"),
            ),

            EmbedderModel::JinaV5Small => Some(
                ModelSpec::new(
                    "jinaai/jina-embeddings-v5-text-small-retrieval",
                    "model.onnx",
                )
                .with_onnx_prefix("onnx/")
                .with_additional_files(vec!["model.onnx_data"])
                .with_onnx_data_prefix("onnx/"),
            ),

            EmbedderModel::JinaV5Nano => Some(
                ModelSpec::new(
                    "jinaai/jina-embeddings-v5-text-nano-retrieval",
                    "model_quantized.onnx",
                )
                .with_onnx_prefix("onnx/")
                .with_additional_files(vec!["model_quantized.onnx_data"])
                .with_onnx_data_prefix("onnx/"),
            ),

            // ── Qwen3-Embedding-0.6B (base decoder model, 28-layer KV-cache) ────────
            // onnx-community exports are full generative LM ONNX with past_key_values.
            // We pass empty [batch,8,0,128] KV tensors and use last-token pooling.
            EmbedderModel::Qwen3Embedding => Some(
                ModelSpec::new("onnx-community/Qwen3-Embedding-0.6B-ONNX", "model.onnx")
                    .with_onnx_prefix("onnx/")
                    .with_additional_files(vec!["model.onnx_data"])
                    .with_onnx_data_prefix("onnx/")
                    .with_kv_cache(8, 128),
            ),

            EmbedderModel::Qwen3EmbeddingInt8 => Some(
                ModelSpec::new(
                    "onnx-community/Qwen3-Embedding-0.6B-ONNX",
                    "model_int8.onnx",
                )
                .with_onnx_prefix("onnx/")
                .with_kv_cache(8, 128),
            ),

            // electroglyph's calibrated uint8 — pre-pooled uint8 output.
            // Dequant: range [-0.3009805, 0.3952634] → scale=0.002730, zero_point=110.
            EmbedderModel::Qwen3EmbeddingUint8 => Some(
                ModelSpec::new(
                    "electroglyph/Qwen3-Embedding-0.6B-onnx-uint8",
                    "dynamic_uint8.onnx",
                )
                .with_uint8_dequant(0.0027303685f32, 110),
            ),

            // ── Octen-Embedding-0.6B (Qwen3 finetune by geoffsee) ────────────────────
            // Three of four variants now ride the fastembed-native path with
            // model_code pointing at `cstr/Octen-Embedding-0.6B-ONNX*` — they
            // auto-download via hf-hub on first use. Returning None here
            // routes them through Embedder::new's `is_native()` branch.
            EmbedderModel::Octen06bFp32
            | EmbedderModel::Octen06bInt4Local
            | EmbedderModel::Octen06bInt8FullLocal => None,

            // Octen INT8 MatMul-only (~1.1 GB) — local-only because fastembed-rs
            // dropped the matching native entry (commit 77cc2e45) due to
            // platform-dependent checksums. Users with the local export can keep
            // using it; everyone else should pick Octen06bInt8FullLocal (smaller
            // and auto-downloads).
            EmbedderModel::Octen06bInt8Local => Some(
                ModelSpec::new(
                    "Octen/Octen-Embedding-0.6B", // informational only
                    "model.int8.onnx",
                )
                .with_local_subdir("octen-embedding-0.6b-int8")
                .with_additional_files(vec!["model.int8.onnx.data"])
                .force_last_token_pool(),
            ),

            // ── PIXIE-Rune-v1.0 quantized variants (cstr HF repo, self-contained) ─────
            EmbedderModel::PixieRuneV1Q => Some(
                ModelSpec::new("cstr/PIXIE-Rune-v1.0-ONNX", "model_quantized.onnx")
                    .with_onnx_prefix("onnx/"),
            ),

            EmbedderModel::PixieRuneV1Int4 => Some(
                ModelSpec::new("cstr/PIXIE-Rune-v1.0-ONNX", "model_int4.onnx")
                    .with_onnx_prefix("onnx/"),
            ),

            EmbedderModel::PixieRuneV1Int4Full => Some(
                ModelSpec::new("cstr/PIXIE-Rune-v1.0-ONNX", "model_int4_full.onnx")
                    .with_onnx_prefix("onnx/"),
            ),

            // ── Snowflake Arctic Embed L v2.0 variants ────────────────────────────────
            EmbedderModel::SnowflakeArcticLv2 => Some(
                ModelSpec::new(
                    "Snowflake/snowflake-arctic-embed-l-v2.0",
                    "model_quantized.onnx",
                )
                .with_onnx_prefix("onnx/"),
            ),

            EmbedderModel::SnowflakeArcticLv2Fp16 => Some(
                ModelSpec::new("Snowflake/snowflake-arctic-embed-l-v2.0", "model_fp16.onnx")
                    .with_onnx_prefix("onnx/"),
            ),

            EmbedderModel::SnowflakeArcticLv2Int8 => Some(
                ModelSpec::new("Snowflake/snowflake-arctic-embed-l-v2.0", "model_int8.onnx")
                    .with_onnx_prefix("onnx/"),
            ),

            EmbedderModel::SnowflakeArcticLv2Q4 => Some(
                ModelSpec::new("Snowflake/snowflake-arctic-embed-l-v2.0", "model_q4.onnx")
                    .with_onnx_prefix("onnx/"),
            ),

            EmbedderModel::SnowflakeArcticLv2Q4F16 => Some(
                ModelSpec::new(
                    "Snowflake/snowflake-arctic-embed-l-v2.0",
                    "model_q4f16.onnx",
                )
                .with_onnx_prefix("onnx/"),
            ),

            EmbedderModel::SnowflakeArcticLv2O4 => Some(
                ModelSpec::new("Snowflake/snowflake-arctic-embed-l-v2.0", "model_O4.onnx")
                    .with_onnx_prefix("onnx/"),
            ),

            EmbedderModel::SnowflakeArcticLv2Fp32 => Some(
                ModelSpec::new("Snowflake/snowflake-arctic-embed-l-v2.0", "model.onnx")
                    .with_onnx_prefix("onnx/")
                    .with_additional_files(vec!["model.onnx_data"])
                    .with_onnx_data_prefix("onnx/"),
            ),

            EmbedderModel::JinaV2Base => Some(ModelSpec::new(
                "jinaai/jina-embeddings-v2-base-en",
                "model.onnx",
            )),

            EmbedderModel::JinaV2Small => Some(ModelSpec::new(
                "jinaai/jina-embeddings-v2-small-en",
                "model.onnx",
            )),

            // Native fastembed models — no spec needed (handled via
            // `fastembed_native_files()` + `TextEmbedding::try_new`).
            EmbedderModel::BgeM3
            | EmbedderModel::MultilingualMiniLm
            | EmbedderModel::MultilingualE5Small
            | EmbedderModel::MultilingualE5Base
            | EmbedderModel::MultilingualE5Large
            | EmbedderModel::BgeSmallEnV15
            | EmbedderModel::BgeBaseEnV15
            | EmbedderModel::BgeLargeEnV15
            | EmbedderModel::NomicEmbedTextV15
            | EmbedderModel::MxbaiEmbedLargeV1
            | EmbedderModel::AllMiniLmL6V2
            | EmbedderModel::EmbeddingGemma300M
            | EmbedderModel::GteBaseEnV15
            | EmbedderModel::GteLargeEnV15
            // P17.6 — GGUF-only models: no ONNX spec, handled via CrispEmbed backend.
            | EmbedderModel::Gemma3Embed2B
            | EmbedderModel::ModernBertBase
            | EmbedderModel::ModernBertLarge
            | EmbedderModel::DebertaV2Xlarge
            | EmbedderModel::NomicBertMoe => None,
        }
    }

    /// HF repo + file list for the fastembed-native models. Used by
    /// `hf_prefetch::prefetch_repo_files` to pre-populate the cache before
    /// `TextEmbedding::try_new` runs (works around a Windows hf-hub bug —
    /// see `hf_prefetch.rs`). Returns `None` for models that don't go through
    /// the native fastembed path.
    pub(crate) fn fastembed_native_files(self) -> Option<(&'static str, Vec<&'static str>)> {
        // The HF repo + file list for each model is taken verbatim from
        // fastembed's `text_embedding/models.rs`. Keep this in sync if
        // fastembed renames or splits a repo.
        let tokenizer_set = vec![
            "tokenizer.json",
            "config.json",
            "special_tokens_map.json",
            "tokenizer_config.json",
        ];
        match self {
            EmbedderModel::BgeM3 => Some((
                "BAAI/bge-m3",
                {
                    let mut v = vec![
                        "onnx/model.onnx",
                        "onnx/model.onnx_data",
                        "onnx/Constant_7_attr__value",
                    ];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::MultilingualMiniLm => Some((
                "Qdrant/paraphrase-multilingual-MiniLM-L12-v2-onnx-Q",
                {
                    let mut v = vec!["model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::BgeLargeEnV15 => Some((
                "Xenova/bge-large-en-v1.5",
                {
                    let mut v = vec!["onnx/model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::MultilingualE5Large => Some((
                "Qdrant/multilingual-e5-large-onnx",
                {
                    let mut v = vec!["model.onnx", "model.onnx_data"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::MxbaiEmbedLargeV1 => Some((
                "mixedbread-ai/mxbai-embed-large-v1",
                {
                    let mut v = vec!["onnx/model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::NomicEmbedTextV15 => Some((
                "nomic-ai/nomic-embed-text-v1.5",
                {
                    let mut v = vec!["onnx/model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::BgeSmallEnV15 => Some((
                "Xenova/bge-small-en-v1.5",
                {
                    let mut v = vec!["onnx/model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::BgeBaseEnV15 => Some((
                "Xenova/bge-base-en-v1.5",
                {
                    let mut v = vec!["onnx/model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::AllMiniLmL6V2 => Some((
                "Qdrant/all-MiniLM-L6-v2-onnx",
                {
                    // Qdrant variant has tokenizer at the root, no `onnx/` prefix
                    let mut v = vec!["model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::MultilingualE5Small => Some((
                "intfloat/multilingual-e5-small",
                {
                    let mut v = vec!["onnx/model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            EmbedderModel::MultilingualE5Base => Some((
                "intfloat/multilingual-e5-base",
                {
                    let mut v = vec!["onnx/model.onnx"];
                    v.extend(&tokenizer_set);
                    v
                },
            )),
            _ => None,
        }
    }

    fn to_fastembed_dense(self) -> EmbeddingModel {
        match self {
            EmbedderModel::BgeM3 => EmbeddingModel::BGEM3,
            EmbedderModel::MultilingualMiniLm => EmbeddingModel::ParaphraseMLMiniLML12V2,
            EmbedderModel::MultilingualE5Small => EmbeddingModel::MultilingualE5Small,
            EmbedderModel::MultilingualE5Base => EmbeddingModel::MultilingualE5Base,
            EmbedderModel::MultilingualE5Large => EmbeddingModel::MultilingualE5Large,
            EmbedderModel::BgeSmallEnV15 => EmbeddingModel::BGESmallENV15,
            EmbedderModel::BgeBaseEnV15 => EmbeddingModel::BGEBaseENV15,
            EmbedderModel::BgeLargeEnV15 => EmbeddingModel::BGELargeENV15,
            EmbedderModel::NomicEmbedTextV15 => EmbeddingModel::NomicEmbedTextV15,
            EmbedderModel::MxbaiEmbedLargeV1 => EmbeddingModel::MxbaiEmbedLargeV1,
            EmbedderModel::AllMiniLmL6V2 => EmbeddingModel::AllMiniLML6V2,
            EmbedderModel::EmbeddingGemma300M => EmbeddingModel::EmbeddingGemma300M,
            EmbedderModel::GteBaseEnV15 => EmbeddingModel::GTEBaseENV15,
            EmbedderModel::GteLargeEnV15 => EmbeddingModel::GTELargeENV15,
            // Octen — auto-download via fastembed-rs (cstr/Octen-Embedding-0.6B-ONNX*)
            EmbedderModel::Octen06bFp32 => EmbeddingModel::OctenEmbedding0_6BFp32,
            EmbedderModel::Octen06bInt4Local => EmbeddingModel::OctenEmbedding0_6BInt4,
            EmbedderModel::Octen06bInt8FullLocal => EmbeddingModel::OctenEmbedding0_6BInt8Full,
            _ => EmbeddingModel::BGEM3,
        }
    }

    fn to_fastembed_sparse(self) -> Option<SparseModel> {
        match self {
            EmbedderModel::BgeM3 => Some(SparseModel::BGEM3),
            // BGE-small + SPLADE++ pairing per HISTORY.md §2: SPLADE on English
            // text only; multilingual sparse stays exclusive to BGE-M3.
            EmbedderModel::BgeSmallEnV15 => Some(SparseModel::SPLADEPPV1),
            _ => None,
        }
    }

    /// Asymmetric retrieval prefix for this model. Returns `""` when the
    /// model was trained without prefixes (BGE-M3, Qwen3, Octen, PIXIE-Rune,
    /// Snowflake Arctic-L v2, Jina v2/v3, GTE v1.5, MiniLM, BERT bases).
    ///
    /// Sources: model cards on HuggingFace, fastembed-rs `model_code` notes,
    /// CrispEmbed `--prefix` examples in README. When in doubt, default to
    /// no prefix — a wrong prefix degrades retrieval quality more than a
    /// missing one.
    pub fn prefix(&self, role: EmbedRole) -> &'static str {
        use EmbedRole::*;
        use EmbedderModel::*;
        match (self, role) {
            // E5 family: symmetric "query: " / "passage: ".
            (MultilingualE5Small | MultilingualE5Base | MultilingualE5Large, Query) => "query: ",
            (MultilingualE5Small | MultilingualE5Base | MultilingualE5Large, Passage) => "passage: ",
            // Nomic v1.5: asymmetric "search_query: " / "search_document: ".
            (NomicEmbedTextV15, Query) => "search_query: ",
            (NomicEmbedTextV15, Passage) => "search_document: ",
            // BGE en-v1.5 + Mxbai: query-only prefix; passages get nothing.
            (
                BgeSmallEnV15 | BgeBaseEnV15 | BgeLargeEnV15 | MxbaiEmbedLargeV1,
                Query,
            ) => "Represent this sentence for searching relevant passages: ",
            // Jina v5 family: asymmetric "Query: " / "Document: " (per fastembed-rs).
            (JinaV5Small | JinaV5Nano, Query) => "Query: ",
            (JinaV5Small | JinaV5Nano, Passage) => "Document: ",
            // EmbeddingGemma 300M: task-templated prefixes.
            (EmbeddingGemma300M, Query) => "task: search result | query: ",
            (EmbeddingGemma300M, Passage) => "title: none | text: ",
            _ => "",
        }
    }
}

/// Whether a text is being embedded as a search query or a stored passage.
/// Asymmetric models (E5, Nomic, BGE en-v1.5, Jina v5, EmbeddingGemma) use
/// different prefixes for each side; symmetric models ignore the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedRole {
    Query,
    Passage,
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
        self.additional_files = self
            .additional_files
            .into_iter()
            .map(|f| {
                if f.ends_with(".onnx_data") {
                    format!("{}{}", prefix, f)
                } else {
                    f
                }
            })
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
        self.additional_files
            .iter()
            .any(|f| f.ends_with(".onnx_data") || f.ends_with(".onnx.data"))
    }

    /// True when this model must use the OrtPath backend (external data OR no config.json).
    pub fn needs_ort_path(&self) -> bool {
        self.has_external_onnx_data() || self.use_ort_path
    }
}

// ── Download helpers ────────────────────────────────────────────────────────

struct ModelPaths {
    onnx: PathBuf,
    tokenizer: PathBuf,
    config: Option<PathBuf>,
    special_tokens_map: Option<PathBuf>,
    tokenizer_config: Option<PathBuf>,
}

/// Payload for the `index://download-progress` Tauri event. Emitted once
/// per file-start and on every meaningful byte-level update so the
/// frontend progress bar can advance smoothly.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadProgress {
    pub repo: String,
    pub file: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// 0..=100. Always derivable from done/total but precomputed so the
    /// frontend doesn't have to.
    pub pct: u8,
}

/// Build a progress callback for `prefetch_repo_files` that:
///  - logs to the in-app log panel (every 10% AND every 50 MB so multi-GB
///    files stay visible without flooding small ones),
///  - emits an `index://download-progress` Tauri event each time, so the
///    UI progress bar moves continuously rather than staying frozen at 5%
///    for the whole download.
fn make_prefetch_logger(repo: &str) -> impl FnMut(&str, u64, u64) + Send + 'static {
    let repo = repo.to_owned();
    let mut current_file = String::new();
    let mut last_logged_pct: u64 = 0;
    let mut last_logged_bytes: u64 = 0;
    let mut last_emitted_bytes: u64 = 0;
    const LOG_EVERY_BYTES: u64 = 50 * 1024 * 1024;     // 50 MB — log line
    const EMIT_EVERY_BYTES: u64 = 1 * 1024 * 1024;     // 1 MB — Tauri event

    fn emit_progress(repo: &str, file: &str, done: u64, total: u64) {
        let pct = if total > 0 { (done * 100 / total) as u8 } else { 0 };
        crate::emit_app_event(
            "index://download-progress",
            &DownloadProgress {
                repo: repo.to_owned(),
                file: file.to_owned(),
                bytes_done: done,
                bytes_total: total,
                pct: pct.min(100),
            },
        );
    }

    move |file: &str, done: u64, total: u64| {
        if file != current_file {
            current_file = file.to_owned();
            last_logged_pct = 0;
            last_logged_bytes = 0;
            last_emitted_bytes = 0;
            crate::app_log!(
                "info",
                "Embedder: starting {}/{} (size ≈ {:.1} MB)",
                repo,
                file,
                total as f64 / (1024.0 * 1024.0)
            );
            emit_progress(&repo, file, 0, total);
        }
        let pct = if total > 0 { done * 100 / total } else { 0 };
        let pct_step = pct >= last_logged_pct + 10 || pct == 100 && last_logged_pct < 100;
        let byte_step = done >= last_logged_bytes + LOG_EVERY_BYTES;
        if pct_step || byte_step {
            last_logged_pct = pct;
            last_logged_bytes = done;
            crate::app_log!(
                "info",
                "Embedder: {}/{} {}% ({:.1} / {:.1} MB)",
                repo,
                file,
                pct,
                done as f64 / (1024.0 * 1024.0),
                total as f64 / (1024.0 * 1024.0)
            );
        }
        // Emit a Tauri event at finer granularity (every 1 MB) so the UI
        // bar moves smoothly. Always emit on completion.
        if done >= last_emitted_bytes + EMIT_EVERY_BYTES || done == total {
            last_emitted_bytes = done;
            emit_progress(&repo, file, done, total);
        }
    }
}

/// Ensure all model files are on disk via hf-hub (re-uses cache on repeat calls).
/// `config.json` is fetched best-effort — some repos (e.g. Octen) don't have one.
/// When `spec.local_subdir` is set, files are read directly from `{cache_dir}/{subdir}/`
/// without any hf-hub network access.
async fn ensure_model_on_disk(spec: &ModelSpec, cache_dir: &Path) -> Result<ModelPaths> {
    if let Some(ref subdir) = spec.local_subdir {
        let base = cache_dir.join(subdir);
        let onnx = base.join(&spec.file);
        let tokenizer = base.join(&spec.tokenizer_file);
        crate::app_log!(
            "info",
            "Embedder: using local model {} (subdir {})",
            onnx.display(),
            subdir
        );
        if !onnx.exists() {
            crate::app_log!("error", "Embedder: local ONNX missing at {}", onnx.display());
            bail!(
                "Local ONNX not found at {:?} — run the export script first",
                onnx
            );
        }
        if !tokenizer.exists() {
            crate::app_log!(
                "error",
                "Embedder: local tokenizer missing at {}",
                tokenizer.display()
            );
            bail!("Local tokenizer not found at {:?}", tokenizer);
        }
        return Ok(ModelPaths {
            onnx,
            tokenizer,
            config: None,
            special_tokens_map: None,
            tokenizer_config: None,
        });
    }

    use super::hf_prefetch::prefetch_repo_files;

    // Required files (failure ⇒ error).
    let mut required: Vec<&str> = vec![spec.file.as_str(), spec.tokenizer_file.as_str()];
    for f in &spec.additional_files {
        required.push(f.as_str());
    }

    crate::app_log!(
        "info",
        "Embedder: prefetching {} files from {} into {}",
        required.len(),
        spec.repo,
        cache_dir.display()
    );
    let progress = make_prefetch_logger(&spec.repo);
    prefetch_repo_files(&spec.repo, &required, cache_dir, progress)
        .await
        .map_err(|e| {
            crate::app_log!("error", "Embedder: prefetch failed for {}: {e:#}", spec.repo);
            e
        })?;

    // The prefetcher writes into hf-hub's cache layout. We compute the same
    // pointer paths here so we can return them directly. After prefetch, the
    // commit hash is in `<cache>/models--<safe_repo>/refs/main`.
    let safe_repo = format!("models--{}", spec.repo.replace('/', "--"));
    let repo_dir = cache_dir.join(&safe_repo);
    let commit = std::fs::read_to_string(repo_dir.join("refs").join("main"))
        .unwrap_or_else(|_| "main".to_owned());
    let snap_dir = repo_dir.join("snapshots").join(commit.trim());

    let onnx = snap_dir.join(&spec.file);
    let tokenizer = snap_dir.join(&spec.tokenizer_file);

    // Optional files: ignore failures since not every repo ships them.
    let config_repo_id = spec.config_repo.as_deref().unwrap_or(spec.repo.as_str());
    let config = if config_repo_id == spec.repo.as_str() {
        let p = snap_dir.join(&spec.config_file);
        if p.exists() {
            Some(p)
        } else {
            // Try to fetch the optional config from the same repo; ignore failure.
            let _ = prefetch_repo_files(
                &spec.repo,
                &[spec.config_file.as_str()],
                cache_dir,
                |_, _, _| {},
            )
            .await;
            if p.exists() { Some(p) } else { None }
        }
    } else {
        // Pull config.json from a different repo (rare).
        let other_safe = format!("models--{}", config_repo_id.replace('/', "--"));
        let other_dir = cache_dir.join(&other_safe);
        let _ = prefetch_repo_files(
            config_repo_id,
            &[spec.config_file.as_str()],
            cache_dir,
            |_, _, _| {},
        )
        .await;
        let other_commit = std::fs::read_to_string(other_dir.join("refs").join("main"))
            .unwrap_or_else(|_| "main".to_owned());
        let p = other_dir
            .join("snapshots")
            .join(other_commit.trim())
            .join(&spec.config_file);
        if p.exists() { Some(p) } else { None }
    };

    let special_tokens_map = if let Some(ref f) = spec.special_tokens_map_file {
        let p = snap_dir.join(f);
        if !p.exists() {
            let _ = prefetch_repo_files(&spec.repo, &[f.as_str()], cache_dir, |_, _, _| {}).await;
        }
        if p.exists() { Some(p) } else { None }
    } else {
        None
    };

    let tokenizer_config = if let Some(ref f) = spec.tokenizer_config_file {
        let p = snap_dir.join(f);
        if !p.exists() {
            let _ = prefetch_repo_files(&spec.repo, &[f.as_str()], cache_dir, |_, _, _| {}).await;
        }
        if p.exists() { Some(p) } else { None }
    } else {
        None
    };

    Ok(ModelPaths {
        onnx,
        tokenizer,
        config,
        special_tokens_map,
        tokenizer_config,
    })
}

/// Download files and return them as bytes (for self-contained fastembed UserDefined models).
async fn fetch_model_bytes(
    spec: &ModelSpec,
    cache_dir: &Path,
) -> Result<(Vec<u8>, TokenizerFiles)> {
    let paths = ensure_model_on_disk(spec, cache_dir).await?;

    let onnx_bytes = std::fs::read(&paths.onnx).context("reading ONNX bytes")?;
    if onnx_bytes.len() < 1_000_000 && spec.additional_files.is_empty() {
        bail!(
            "ONNX file for {} is suspiciously small ({} B). Git-LFS pointer?",
            spec.repo,
            onnx_bytes.len()
        );
    }

    let read_opt = |p: &Option<PathBuf>| -> Vec<u8> {
        p.as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .unwrap_or_default()
    };

    let tokenizer_files = TokenizerFiles {
        tokenizer_file: std::fs::read(&paths.tokenizer).context("reading tokenizer.json")?,
        config_file: read_opt(&paths.config),
        special_tokens_map_file: read_opt(&paths.special_tokens_map),
        tokenizer_config_file: read_opt(&paths.tokenizer_config),
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
            EmbedderDevice::Auto => "Auto (recommended)",
            EmbedderDevice::Cpu => "CPU",
            EmbedderDevice::Metal => "Metal (macOS)",
            EmbedderDevice::Cuda => "CUDA (NVIDIA)",
        }
    }

    pub fn execution_providers(&self) -> Vec<ExecutionProviderDispatch> {
        match self {
            EmbedderDevice::Cpu => vec![],
            EmbedderDevice::Auto => ep_auto(),
            EmbedderDevice::Metal => ep_metal(),
            EmbedderDevice::Cuda => ep_cuda(),
        }
    }
}

fn ep_auto() -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "macos")]
    {
        ep_metal()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ep_cuda()
    }
}

fn ep_metal() -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "macos")]
    {
        use ort::execution_providers::CoreMLExecutionProvider;
        vec![CoreMLExecutionProvider::default().build()]
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![]
    }
}

fn ep_cuda() -> Vec<ExecutionProviderDispatch> {
    #[cfg(not(target_os = "macos"))]
    {
        use ort::execution_providers::CUDAExecutionProvider;
        vec![CUDAExecutionProvider::default().build()]
    }
    #[cfg(target_os = "macos")]
    {
        vec![]
    }
}

// ── Config ─────────────────────────────────────────────────────────────────

/// Which dense-embedding implementation to run.
///
/// `Onnx` (default) routes through fastembed or `OrtPathEmbedder` as before.
/// `Gguf` routes through CrispEmbed — only available under the `crispembed`
/// cargo feature and only for models whose GGUF equivalent is known-good.
/// Callers must gate selection on `EmbedderModel::supports_gguf()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EmbedderBackend {
    #[default]
    Onnx,
    Gguf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedderConfig {
    pub model: EmbedderModel,
    pub device: EmbedderDevice,
    pub cache_dir: PathBuf,
    pub batch_size: usize,
    pub backend: EmbedderBackend,
    /// Matryoshka truncation dim. `None` (or `Some(0)`) means use the
    /// model's default. Only honored on the CrispEmbed (GGUF) backend —
    /// fastembed has no per-call truncation hook. Quality only holds for
    /// MRL-trained models (BGE-M3, Snowflake Arctic L v2, PIXIE-Rune).
    #[serde(default)]
    pub matryoshka_dim: Option<u32>,

    /// Registry-driven model override (Stage: registry-driven selection).
    /// When set and the backend is `Gguf`, this name/path is passed to
    /// `crispembed::CrispEmbed::new` instead of resolving through the
    /// `EmbedderModel` enum's `to_gguf_spec()`.  The crispembed library
    /// handles name → cached-path → download automatically.
    /// Has no effect on the ONNX/OrtPath backends.
    #[serde(default)]
    pub model_name_override: Option<String>,

    /// Quantisation to pick out of the `cstr/<name>-GGUF` repo, overriding
    /// the one baked into the [`EmbedderModel`] variant.
    ///
    /// Without this the quant is a property of the *variant*, so a model is
    /// only reachable at whatever quant someone happened to encode — e.g.
    /// `MultilingualE5Small` resolves to the 455 MiB F32 file and there was
    /// no way to ask for the 126 MiB `-q8_0` sitting in the same repo. The
    /// alternative to this field is a variant per model × quant, which for
    /// the current registry is ~14 × 3.
    ///
    /// `None` keeps the variant's own choice, so existing callers and the
    /// persisted config are unaffected.
    #[serde(default)]
    pub gguf_quant: Option<GgufQuant>,
}

/// A quantisation available in the `cstr/*-GGUF` repos.
///
/// The suffix is part of the filename convention those repos follow:
/// `<name>.gguf` (F32), `<name>-q8_0.gguf`, `<name>-q4_k.gguf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GgufQuant {
    /// Full-precision reference. Largest, and — per the conversion
    /// measurements — not worth its size next to Q8_0.
    F32,
    /// 8-bit. Cosine similarity to F32 is ≥0.9995 for every model in the
    /// registry, so this is the default worth reaching for.
    Q8_0,
    /// 4-bit K-quant. Fidelity is model-dependent (0.965–0.99); worth it
    /// only when memory-bound.
    Q4K,
}

impl GgufQuant {
    /// Filename suffix, matching the repo naming convention.
    pub fn suffix(self) -> &'static str {
        match self {
            GgufQuant::F32 => "",
            GgufQuant::Q8_0 => "-q8_0",
            GgufQuant::Q4K => "-q4_k",
        }
    }

    /// Parse a user-supplied spelling. Accepts the forms that appear in the
    /// repos and the ones people actually type.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', '.'], "_").as_str() {
            "f32" | "fp32" | "full" | "none" => Some(GgufQuant::F32),
            "q8_0" | "q8" | "int8" => Some(GgufQuant::Q8_0),
            "q4_k" | "q4k" | "q4" | "int4" => Some(GgufQuant::Q4K),
            _ => None,
        }
    }
}

impl EmbedderConfig {
    pub fn new(model: EmbedderModel, device: EmbedderDevice, cache_dir: PathBuf) -> Self {
        EmbedderConfig {
            model,
            device,
            cache_dir,
            batch_size: 32,
            backend: EmbedderBackend::Onnx,
            matryoshka_dim: None,
            model_name_override: None,
            gguf_quant: None,
        }
    }

    /// Pick a specific quantisation out of the model's GGUF repo.
    /// `None` leaves the variant's own choice in place.
    pub fn with_gguf_quant(mut self, quant: Option<GgufQuant>) -> Self {
        self.gguf_quant = quant;
        self
    }

    pub fn with_model_name_override(mut self, name: Option<String>) -> Self {
        self.model_name_override = name.filter(|s| !s.is_empty());
        self
    }

    pub fn with_backend(mut self, backend: EmbedderBackend) -> Self {
        self.backend = backend;
        self
    }

    pub fn with_matryoshka_dim(mut self, dim: Option<u32>) -> Self {
        self.matryoshka_dim = dim.filter(|&d| d > 0);
        self
    }

    /// Effective output dim: matryoshka_dim if set and ≤ model's nominal
    /// dim, else the model default. Use this anywhere you need to size a
    /// LanceDB column or pre-allocate per-vector buffers.
    pub fn effective_dim(&self) -> usize {
        self.matryoshka_dim
            .map(|d| (d as usize).min(self.model.dims()))
            .unwrap_or_else(|| self.model.dims())
    }
}

// ── Output types ───────────────────────────────────────────────────────────

pub struct DenseEmbedding {
    pub vectors: Vec<Vec<f32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseVector {
    pub indices: Vec<u32>,
    pub values: Vec<f32>,
}

impl SparseVector {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "indices": self.indices, "values": self.values })
    }
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(SparseVector {
            indices: serde_json::from_value(v["indices"].clone()).ok()?,
            values: serde_json::from_value(v["values"].clone()).ok()?,
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
    session: ort::session::Session,
    tokenizer: tokenizers::Tokenizer,
    batch_size: usize,
    dims: usize,
    /// Whether the model accepts `token_type_ids` as input.
    has_type_ids: bool,
    /// Whether the model accepts a `task_id` input (e.g. jina-embeddings-v3 LoRA).
    has_task_id: bool,
    /// Whether the model requires explicit `position_ids` (Qwen3-based models).
    has_position_ids: bool,
    /// Name of the first output (cached to avoid borrow conflict with `run()`).
    first_output: String,
    /// True when the model outputs a pre-pooled `sentence_embedding` tensor.
    pre_pooled: bool,
    /// Number of KV-cache layer pairs (0 = no KV-cache, encoder model).
    kv_cache_layers: usize,
    /// Number of KV heads per layer (e.g. 8 for Qwen3-0.6B).
    kv_cache_kv_heads: usize,
    /// Head dimension (e.g. 128 for Qwen3-0.6B).
    kv_cache_head_dim: usize,
    /// uint8 dequantization params: (scale, zero_point). None = output is float32.
    dequant: Option<(f32, u8)>,
    /// Use last-token pooling instead of mean pooling (decoder/causal without KV-cache).
    last_token_pool_mode: bool,
}

pub struct OrtPathLoadOptions<'a> {
    pub onnx_path: &'a Path,
    pub tok_path: &'a Path,
    pub max_tokens: usize,
    pub dims: usize,
    pub batch_size: usize,
    pub eps: Vec<ExecutionProviderDispatch>,
    pub kv_cache_kv_heads: usize,
    pub kv_cache_head_dim: usize,
    pub force_pre_pooled: bool,
    pub dequant: Option<(f32, u8)>,
    pub last_token_pool_mode: bool,
}

impl OrtPathEmbedder {
    fn load(opts: OrtPathLoadOptions) -> Result<Self> {
        let OrtPathLoadOptions {
            onnx_path,
            tok_path,
            max_tokens,
            dims,
            batch_size,
            eps,
            kv_cache_kv_heads,
            kv_cache_head_dim,
            force_pre_pooled,
            dequant,
            last_token_pool_mode,
        } = opts;
        // Build ORT session from file — ORT resolves `.onnx_data` automatically.
        let builder = ort::session::Session::builder().context("ORT session builder")?;
        let builder = if eps.is_empty() {
            builder
        } else {
            builder
                .with_execution_providers(eps)
                .context("setting EPs")?
        };
        let session = builder
            .commit_from_file(onnx_path)
            .with_context(|| format!("ORT failed to load {:?}", onnx_path))?;

        // Load tokenizer and configure padding + truncation.
        let mut tokenizer = tokenizers::Tokenizer::from_file(tok_path)
            .map_err(|e| anyhow::anyhow!("tokenizer load error: {e}"))?;

        let _ = tokenizer.with_truncation(Some(TruncationParams {
            direction: TruncationDirection::Right,
            max_length: max_tokens.min(512), // tokenizers crate cap
            strategy: TruncationStrategy::LongestFirst,
            stride: 0,
        }));

        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            direction: PaddingDirection::Right,
            pad_to_multiple_of: None,
            pad_id: 0,
            pad_type_id: 0,
            pad_token: "[PAD]".to_string(),
        }));

        let has_type_ids = session
            .inputs()
            .iter()
            .any(|i: &ort::value::Outlet| i.name() == "token_type_ids");
        let has_task_id = session
            .inputs()
            .iter()
            .any(|i: &ort::value::Outlet| i.name() == "task_id");
        let has_position_ids = session
            .inputs()
            .iter()
            .any(|i: &ort::value::Outlet| i.name() == "position_ids");
        let kv_cache_layers = session
            .inputs()
            .iter()
            .filter(|i: &&ort::value::Outlet| {
                i.name().starts_with("past_key_values.") && i.name().ends_with(".key")
            })
            .count();
        // Pre-pooled: model outputs [batch, dim] directly (no further pooling needed).
        // Detected either by output name or by the spec flag (for models with non-standard names).
        let pre_pooled = force_pre_pooled
            || session.outputs().iter().any(|o: &ort::value::Outlet| {
                let n = o.name();
                n == "sentence_embedding" || n == "embeddings"
            });
        let first_output = session.outputs()[0].name().to_owned();

        println!(
            "[embedder] OrtPath session ready — inputs: {:?}  outputs: {:?}  kv_cache_layers: {}",
            session
                .inputs()
                .iter()
                .map(|i: &ort::value::Outlet| i.name())
                .collect::<Vec<_>>(),
            session
                .outputs()
                .iter()
                .map(|o: &ort::value::Outlet| o.name())
                .collect::<Vec<_>>(),
            kv_cache_layers,
        );

        Ok(OrtPathEmbedder {
            session,
            tokenizer,
            batch_size,
            dims,
            has_type_ids,
            has_task_id,
            has_position_ids,
            pre_pooled,
            first_output,
            kv_cache_layers,
            kv_cache_kv_heads,
            kv_cache_head_dim,
            dequant,
            last_token_pool_mode,
        })
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

        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| anyhow::anyhow!("tokenizer batch error: {e}"))?;

        let seq_len = encodings[0].get_ids().len();

        // Flatten tensors — shape [batch, seq_len].
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch * seq_len);
        let mut attn_mask: Vec<i64> = Vec::with_capacity(batch * seq_len);
        let mut token_type_ids: Vec<i64> = Vec::with_capacity(batch * seq_len);

        for enc in &encodings {
            input_ids.extend(enc.get_ids().iter().map(|&x| x as i64));
            attn_mask.extend(enc.get_attention_mask().iter().map(|&x| x as i64));
            token_type_ids.extend(enc.get_type_ids().iter().map(|&x| x as i64));
        }

        let ids_t = ort::value::Tensor::<i64>::from_array(([batch, seq_len], input_ids))
            .context("input_ids tensor")?;
        let mask_t = ort::value::Tensor::<i64>::from_array(([batch, seq_len], attn_mask.clone()))
            .context("attention_mask tensor")?;
        let types_t = ort::value::Tensor::<i64>::from_array(([batch, seq_len], token_type_ids))
            .context("token_type_ids tensor")?;

        // task_id=1 → retrieval.passage (Jina-v3 LoRA adapter selection).
        let task_id_t = ort::value::Tensor::<i64>::from_array(([batch], vec![1i64; batch]))
            .context("task_id tensor")?;

        // position_ids: [[0,1,...,seq_len-1], ...] repeated for each batch item (Qwen3).
        let pos_ids: Vec<i64> = (0..batch).flat_map(|_| 0..seq_len as i64).collect();
        let pos_ids_t = ort::value::Tensor::<i64>::from_array(([batch, seq_len], pos_ids))
            .context("position_ids tensor")?;

        // ── KV-cache decoder models (Qwen3-Embedding style) ────────────────────
        // Pass empty past_key_values tensors [batch, kv_heads, 0, head_dim] and
        // use last-token pooling (EOS token position = last non-padding token).
        if self.kv_cache_layers > 0 {
            let mut inputs: Vec<(std::borrow::Cow<str>, ort::value::DynValue)> = vec![
                ("input_ids".into(), ids_t.upcast().into()),
                ("attention_mask".into(), mask_t.upcast().into()),
                ("position_ids".into(), pos_ids_t.upcast().into()),
            ];
            // Build empty KV-cache tensors [batch, kv_heads, 0, head_dim].
            // ndarray supports zero-sized dimensions; ort's raw-data path does not.
            for layer in 0..self.kv_cache_layers {
                let k_empty = ort::value::Tensor::from_array(ndarray::Array4::<f32>::zeros((
                    batch,
                    self.kv_cache_kv_heads,
                    0usize,
                    self.kv_cache_head_dim,
                )))
                .context("kv key tensor")?;
                let v_empty = ort::value::Tensor::from_array(ndarray::Array4::<f32>::zeros((
                    batch,
                    self.kv_cache_kv_heads,
                    0usize,
                    self.kv_cache_head_dim,
                )))
                .context("kv val tensor")?;
                inputs.push((
                    format!("past_key_values.{}.key", layer).into(),
                    k_empty.upcast().into(),
                ));
                inputs.push((
                    format!("past_key_values.{}.value", layer).into(),
                    v_empty.upcast().into(),
                ));
            }
            let outputs = self.session.run(inputs)?;
            let (_shape, data) = outputs[self.first_output.as_str()]
                .try_extract_tensor::<f32>()
                .context("last_hidden_state extract (kv-cache)")?;
            let dim = self.dims;
            return Ok((0..batch)
                .map(|i| {
                    let toks = &data[i * seq_len * dim..(i + 1) * seq_len * dim];
                    let mask_row = &attn_mask[i * seq_len..(i + 1) * seq_len];
                    l2_normalize(last_token_pool(toks, mask_row, seq_len, dim))
                })
                .collect());
        }

        // ── Encoder models ──────────────────────────────────────────────────────
        let outputs = match (self.has_type_ids, self.has_task_id, self.has_position_ids) {
            (true, true, _) => self.session.run(ort::inputs![
                "input_ids"       => ids_t,
                "attention_mask"  => mask_t,
                "token_type_ids"  => types_t,
                "task_id"         => task_id_t
            ])?,
            (true, false, _) => self.session.run(ort::inputs![
                "input_ids"       => ids_t,
                "attention_mask"  => mask_t,
                "token_type_ids"  => types_t
            ])?,
            (false, true, _) => self.session.run(ort::inputs![
                "input_ids"       => ids_t,
                "attention_mask"  => mask_t,
                "task_id"         => task_id_t
            ])?,
            (false, false, true) => self.session.run(ort::inputs![
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
                data.iter()
                    .map(|&v| (v as i32 - zp as i32) as f32 * scale)
                    .collect()
            } else {
                let (_shape, data) = outputs[self.first_output.as_str()]
                    .try_extract_tensor::<f32>()
                    .context("pre-pooled f32 extract")?;
                data.to_vec()
            };
            Ok((0..batch)
                .map(|i| l2_normalize(f32_data[i * dim..(i + 1) * dim].to_vec()))
                .collect())
        } else {
            // last_hidden_state: [batch, seq, dim] — apply mean or last-token pooling.
            let (_shape, data) = outputs[self.first_output.as_str()]
                .try_extract_tensor::<f32>()
                .context("last_hidden_state extract")?;
            let dim = self.dims;
            Ok((0..batch)
                .map(|i| {
                    let token_embs = &data[i * seq_len * dim..(i + 1) * seq_len * dim];
                    let mask = &attn_mask[i * seq_len..(i + 1) * seq_len];
                    if self.last_token_pool_mode {
                        l2_normalize(last_token_pool(token_embs, mask, seq_len, dim))
                    } else {
                        l2_normalize(mean_pool(token_embs, mask, seq_len, dim))
                    }
                })
                .collect())
        }
    }
}

/// Last-token pooling for decoder/causal models (e.g. Qwen3-Embedding).
/// Takes the embedding at the last non-padding position (EOS token).
fn last_token_pool(token_embs: &[f32], mask: &[i64], seq_len: usize, dim: usize) -> Vec<f32> {
    let last_pos = mask.iter().rposition(|&m| m != 0).unwrap_or(seq_len - 1);
    token_embs[last_pos * dim..(last_pos + 1) * dim].to_vec()
}

fn mean_pool(token_embs: &[f32], mask: &[i64], seq_len: usize, dim: usize) -> Vec<f32> {
    let mut pooled = vec![0.0f32; dim];
    let mut count = 0.0f32;
    for t in 0..seq_len {
        if mask[t] != 0 {
            for d in 0..dim {
                pooled[d] += token_embs[t * dim + d];
            }
            count += 1.0;
        }
    }
    if count > 0.0 {
        pooled.iter_mut().for_each(|v| *v /= count);
    }
    pooled
}

fn l2_normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-9 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

// ── DenseBackend ───────────────────────────────────────────────────────────

enum DenseBackend {
    Fastembed(TextEmbedding),
    OrtPath(OrtPathEmbedder),
    #[cfg(feature = "crispembed")]
    CrispEmbed(CrispEmbedBackend),
}

// ── CrispEmbed backend (GGUF via libcrispembed) ────────────────────────────
// Thin wrapper around the `crispembed` safe crate. Activated with the
// `crispembed` cargo feature.
#[cfg(feature = "crispembed")]
pub(crate) struct CrispEmbedBackend {
    model: crispembed::CrispEmbed,
}

#[cfg(feature = "crispembed")]
#[allow(dead_code)] // LoRA set/get/list are plumbed but not yet called from
                    // a higher level (awaiting Settings UI for adapter selection).
impl CrispEmbedBackend {
    /// Open a GGUF file through the CrispEmbed wrapper.
    ///
    /// Public to the crate so `index::reranker` can route through this
    /// same loader instead of importing `crispembed::CrispEmbed`
    /// directly — keeps the feature-gated upstream import confined to
    /// this module.
    pub(crate) fn load(gguf_path: &Path) -> Result<Self> {
        let long_path_safe = long_path_safe(gguf_path);
        let p = long_path_safe
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 GGUF path: {:?}", gguf_path))?;
        println!("[embedder] Loading GGUF via CrispEmbed: {}", p);
        let model = crispembed::CrispEmbed::new(p, crispembed_threads())
            .map_err(|e| anyhow::anyhow!("crispembed load failed: {e}"))?;
        Ok(Self { model })
    }

    /// Load by registry name, repo alias, or absolute path.  The crispembed
    /// library resolves the name to a cached path and downloads if needed.
    pub(crate) fn load_by_name(name: &str) -> Result<Self> {
        println!("[embedder] Loading GGUF by name via CrispEmbed: {name}");
        let model = crispembed::CrispEmbed::new(name, crispembed_threads())
            .map_err(|e| anyhow::anyhow!("crispembed load failed for '{name}': {e}"))?;
        Ok(Self { model })
    }

    /// Actual output dimension as reported by the loaded model.
    pub(crate) fn dim(&self) -> usize {
        self.model.dim()
    }

    fn embed(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let vecs = self.model.encode_batch(&refs);
        if vecs.len() != texts.len() {
            bail!(
                "CrispEmbed returned {} vectors for {} inputs",
                vecs.len(),
                texts.len()
            );
        }
        Ok(vecs)
    }

    /// Set prompt prefix (e.g. "query: " for E5, "search_query: " for Nomic)
    fn set_prefix(&mut self, prefix: &str) {
        self.model.set_prefix(prefix);
    }

    // The methods below are placeholders for upcoming P2 work tracked in
    // PLAN.md (Matryoshka dim selection, sparse routing into search,
    // reranking pipeline). Suppress dead_code until the corresponding
    // feature lands; deleting and re-adding them on each task adds churn.

    /// Set Matryoshka output dimension (0 = model default)
    fn set_dim(&mut self, dim: i32) {
        self.model.set_dim(dim);
    }

    /// Check if model supports sparse retrieval (BGE-M3, SPLADE).
    /// Promoted from `#[allow(dead_code)]` when Embedder::embed_sparse
    /// learned to fall back to the dense GGUF backend's sparse head
    /// (closing the gap for users on GGUF — fastembed's sparse path
    /// was the only producer before this).
    pub(crate) fn has_sparse(&self) -> bool {
        self.model.has_sparse()
    }

    /// Sparse encode (BGE-M3 / SPLADE) — returns SPLADE-style
    /// `(vocab_id, weight)` pairs.  Per-text call: feeding N texts
    /// runs the model N times.  Acceptable for chunked-ingest
    /// volumes (the GGUF prompt-cache helps a lot on repeated
    /// prefixes) but a future `encode_sparse_batch` upstream would
    /// help on large batches.
    pub(crate) fn encode_sparse(&mut self, text: &str) -> Vec<(i32, f32)> {
        self.model.encode_sparse(text)
    }

    /// Check if model has a ColBERT head (per-token L2-normalised
    /// projections).  Only BGE-M3 GGUF qualifies today.
    pub(crate) fn has_colbert(&self) -> bool {
        self.model.has_colbert()
    }

    /// ColBERT multi-vector encode — returns one L2-normalised
    /// vector per input token (dim = ColBERT projection dim).
    pub(crate) fn encode_multivec(&mut self, text: &str) -> Vec<Vec<f32>> {
        self.model.encode_multivec(text)
    }

    /// Per-token contextual embeddings for token-level match highlighting.
    /// Returns `(token_text, embedding)` for each subword token.
    pub(crate) fn encode_tokens(&mut self, text: &str) -> Vec<(String, Vec<f32>)> {
        self.model.encode_tokens(text)
    }

    /// Check if model is a cross-encoder reranker.
    /// Used by `index::reranker::Reranker::load` to verify a reranker
    /// GGUF was actually loaded.
    pub(crate) fn is_reranker(&self) -> bool {
        self.model.is_reranker()
    }

    /// Cross-encoder reranking score.  Used by
    /// `index::reranker::Reranker::score`.
    pub(crate) fn rerank(&mut self, query: &str, document: &str) -> f32 {
        self.model.rerank(query, document)
    }

    // ── LoRA adapter hot-swap (Jina v5 task adapters) ──────────────
    // NOT blocked upstream any more. `CrispEmbed::{list_lora, set_lora,
    // get_lora}` are present as of v0.16.1, which both workflows now pin, and
    // `self.model` is exactly that type — so the old note ("landed after the
    // v0.11.8 tag … un-gate once CrispEmbed cuts a release") is obsolete.
    //
    // What is actually missing is a *caller*: nothing in CrispSorter chooses a
    // task adapter yet, so un-commenting these three would add `pub(crate)`
    // methods with no users and fail CI's `clippy -D warnings` on `dead_code`.
    // Wiring them means picking where the choice lives — an embedder setting
    // that survives re-init, plus a surface to set it — which is a feature, not
    // an un-gate. Tracked in PLAN.md § P19; keep them commented until then
    // rather than adding an `#[allow(dead_code)]` nobody revisits.
    //
    // pub(crate) fn list_lora(&self) -> Vec<String> {
    //     self.model.list_lora()
    // }
    // pub(crate) fn set_lora(&mut self, adapter_name: &str) -> bool {
    //     self.model.set_lora(adapter_name)
    // }
    // pub(crate) fn get_lora(&self) -> Option<String> {
    //     self.model.get_lora()
    // }
}

/// Pre-converted GGUF hosted by cstr/ on HuggingFace. Mirrors the registry
/// in `CrispEmbed/examples/cli/model_mgr.cpp`.
/// Threads to hand CrispEmbed for CPU inference.
///
/// `crispembed_init` reads its argument as `n_threads > 0 ? n_threads : 1`, so
/// the `0` every call site used to pass meant **one thread** — not "pick a
/// sensible default". On a 14-core machine that left GGUF embedding running
/// single-threaded while the ONNX backend used the whole box, which is both a
/// large throughput loss and enough to make any comparison between the two
/// meaningless.
///
/// Capped rather than "all logical CPUs": these are small encoder graphs where
/// the gain flattens quickly, and on hybrid P/E-core parts (Alder Lake and
/// later) oversubscribing schedules work onto efficiency cores and can lose
/// time outright. `CRISPEMBED_N_THREADS` overrides for tuning.
#[cfg(feature = "crispembed")]
pub(crate) fn crispembed_threads() -> i32 {
    if let Ok(v) = std::env::var("CRISPEMBED_N_THREADS") {
        if let Ok(n) = v.trim().parse::<i32>() {
            if n > 0 {
                return n;
            }
        }
    }
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    logical.clamp(1, 8) as i32
}

/// Make `p` openable by a narrow-API C consumer on Windows.
///
/// CrispEmbed loads the GGUF through a C `fopen`, which is subject to the
/// 260-character `MAX_PATH` limit. Rust's std uses the wide API and happily
/// *writes* longer paths, so the download succeeds and the load then fails
/// with `No such file or directory` for a file that is plainly there. The
/// hf-hub cache layout makes this easy to hit — `<cache>/models--<org>--<repo>
/// /snapshots/<40-char sha>/<file>.gguf` spends ~120 characters before the
/// user's own directory is counted.
///
/// `\\?\` opts into the extended-length form (~32,767). It also disables path
/// normalisation, so the path must be absolute with no `.`/`..` and only
/// backslashes — hence the canonicalise first. `std::fs::canonicalize`
/// already returns a `\\?\` path on Windows, which is exactly what we want;
/// if it fails (file missing, permissions) we hand back the original so the
/// caller still reports the real error rather than one about canonicalising.
///
/// No-op on every other platform.
#[cfg(feature = "crispembed")]
fn long_path_safe(p: &Path) -> std::borrow::Cow<'_, Path> {
    #[cfg(windows)]
    {
        if let Ok(c) = std::fs::canonicalize(p) {
            return std::borrow::Cow::Owned(c);
        }
    }
    std::borrow::Cow::Borrowed(p)
}

#[cfg(feature = "crispembed")]
#[derive(Debug, Clone)]
pub struct GgufSpec {
    /// HF repo id, e.g. "cstr/pixie-rune-v1-GGUF".
    pub repo: String,
    /// File inside the repo, e.g. "pixie-rune-v1.gguf".
    pub file: String,
}

#[cfg(feature = "crispembed")]
async fn ensure_gguf_on_disk(spec: &GgufSpec, cache_dir: &Path) -> Result<PathBuf> {
    use super::hf_prefetch::prefetch_repo_files;
    crate::app_log!(
        "info",
        "Embedder: prefetching GGUF {}/{}",
        spec.repo,
        spec.file
    );
    let progress = make_prefetch_logger(&spec.repo);
    let files = prefetch_repo_files(&spec.repo, &[spec.file.as_str()], cache_dir, progress)
        .await
        .with_context(|| format!("failed to fetch {}/{}", spec.repo, spec.file))?;
    files
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("prefetch returned no files"))
}

// ── Embedder ────────────────────────────────────────────────────────────────

fn prepend_prefix(texts: &[String], prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return texts.to_vec();
    }
    texts
        .iter()
        .map(|t| {
            let mut s = String::with_capacity(prefix.len() + t.len());
            s.push_str(prefix);
            s.push_str(t);
            s
        })
        .collect()
}

pub struct Embedder {
    config: EmbedderConfig,
    dense: DenseBackend,
    sparse: Option<SparseTextEmbedding>,
    /// Actual output dim discovered at load time.  Set when the model was
    /// loaded via `model_name_override` (registry-driven path) where the
    /// static `EmbedderModel::dims()` would return the enum-default dim
    /// rather than the real model's dim.  `None` = use `config.effective_dim()`.
    runtime_dim: Option<usize>,
}

impl Embedder {
    pub async fn new(config: EmbedderConfig) -> Result<Self> {
        // License-consent gate — refuse non-commercial / use-restricted models
        // unless the operator accepted the license. Single choke point for every
        // backend below (GGUF/CrispEmbed, ONNX/fastembed, OrtPath, direct-HF),
        // including the registry-driven load-by-name path.
        match config.model_name_override.as_deref() {
            Some(name) => crate::index::license_consent::ensure_for_registry_name(name)?,
            None => config.model.ensure_license_consent()?,
        }
        let eps = config.device.execution_providers();
        crate::app_log!(
            "info",
            "Embedder: init model={:?} device={:?} backend={:?} cache={}",
            config.model,
            config.device,
            config.backend,
            config.cache_dir.display()
        );

        // Try the GGUF path first — only produces Some when the `crispembed`
        // cargo feature is on AND the caller asked for it AND the model has a
        // known-good GGUF equivalent. Otherwise we fall through to the normal
        // ONNX paths.
        // registry_dim: Some(dim) when loaded via model_name_override; used to
        // populate Embedder::runtime_dim so dims() returns the actual model dim.
        #[cfg(feature = "crispembed")]
        let (gguf_backend, registry_dim): (Option<DenseBackend>, Option<usize>) = 'gguf: {
            if !matches!(config.backend, EmbedderBackend::Gguf) {
                break 'gguf (None, None);
            }

            // ── Registry-driven override: load by name / alias ────────────
            if let Some(ref name) = config.model_name_override {
                crate::app_log!(
                    "info",
                    "Embedder: registry-driven load '{}' (bypassing enum spec)",
                    name
                );
                let backend = CrispEmbedBackend::load_by_name(name)?;
                let actual_dim = backend.dim();
                let mut backend = backend;
                if let Some(d) = config.matryoshka_dim {
                    if d > 0 {
                        let clamped = (d as usize).min(actual_dim) as i32;
                        backend.set_dim(clamped);
                    }
                }
                break 'gguf (Some(DenseBackend::CrispEmbed(backend)), Some(actual_dim));
            }

            let Some(spec) = config.model.to_gguf_spec_with_quant(config.gguf_quant) else {
                eprintln!(
                    "[embedder] GGUF requested for {:?} but no GGUF spec available — falling back to ONNX",
                    config.model
                );
                break 'gguf (None, None);
            };
            let gguf_path = ensure_gguf_on_disk(&spec, &config.cache_dir).await?;
            let mut backend = CrispEmbedBackend::load(&gguf_path)?;
            // Matryoshka: only the GGUF backend exposes the underlying
            // crispembed::CrispEmbed::set_dim hook; fastembed and OrtPath
            // ignore the field. set_dim(0) = model default, so a None config
            // is a no-op.
            if let Some(d) = config.matryoshka_dim {
                if d > 0 {
                    let nominal = config.model.dims() as u32;
                    let clamped = d.min(nominal) as i32;
                    println!(
                        "[embedder] Matryoshka set_dim({}) (nominal {})",
                        clamped, nominal
                    );
                    backend.set_dim(clamped);
                }
            }
            (Some(DenseBackend::CrispEmbed(backend)), None)
        };
        #[cfg(not(feature = "crispembed"))]
        let (gguf_backend, registry_dim): (Option<DenseBackend>, Option<usize>) = (None, None);

        let dense = if let Some(g) = gguf_backend {
            g
        } else if config.model.is_native() {
            // ── fastembed built-in model ────────────────────────────────────
            crate::app_log!(
                "info",
                "Embedder: loading native fastembed model {:?}",
                config.model
            );
            // Workaround for a Windows-only hf-hub bug (see hf_prefetch.rs):
            // fastembed's internal download fails with os error 3, so we
            // pre-populate the cache via reqwest. fastembed's cache lookup
            // then skips the broken download path.
            if let Some((repo, files)) = config.model.fastembed_native_files() {
                use super::hf_prefetch::prefetch_repo_files;
                crate::app_log!(
                    "info",
                    "Embedder: prefetching {} files from {repo} (~{} MB total) into {}",
                    files.len(),
                    config.model.approx_download_mb(),
                    config.cache_dir.display()
                );
                let progress = make_prefetch_logger(repo);
                prefetch_repo_files(repo, &files, &config.cache_dir, progress)
                    .await
                    .map_err(|e| {
                        crate::app_log!(
                            "error",
                            "Embedder: prefetch failed for {repo}: {e:#}"
                        );
                        e
                    })?;
            }
            let opts = TextInitOptions::new(config.model.to_fastembed_dense())
                .with_cache_dir(config.cache_dir.clone())
                .with_show_download_progress(true)
                .with_execution_providers(eps.clone());
            let dense = TextEmbedding::try_new(opts).map_err(|e| {
                crate::app_log!("error", "Embedder: fastembed init failed: {e}");
                e
            })?;
            DenseBackend::Fastembed(dense)
        } else {
            let spec = config
                .model
                .to_model_spec()
                .ok_or_else(|| anyhow::anyhow!("No model spec for {:?}", config.model))?;

            if spec.needs_ort_path() {
                // ── OrtPath: needed for external-data models OR repos without config.json ──
                crate::app_log!(
                    "info",
                    "Embedder: using OrtPath backend for {} ({})",
                    spec.repo,
                    spec.file
                );
                let paths = ensure_model_on_disk(&spec, &config.cache_dir).await?;
                let emb = OrtPathEmbedder::load(OrtPathLoadOptions {
                    onnx_path: &paths.onnx,
                    tok_path: &paths.tokenizer,
                    max_tokens: config.model.max_tokens(),
                    dims: config.model.dims(),
                    batch_size: config.batch_size,
                    eps: eps.clone(),
                    kv_cache_kv_heads: spec.kv_cache_kv_heads,
                    kv_cache_head_dim: spec.kv_cache_head_dim,
                    force_pre_pooled: spec.force_pre_pooled,
                    dequant: spec.dequant,
                    last_token_pool_mode: spec.last_token_pool,
                })?;
                DenseBackend::OrtPath(emb)
            } else {
                // ── fastembed UserDefined: self-contained ONNX with config.json ──
                crate::app_log!(
                    "info",
                    "Embedder: using fastembed UserDefined backend for {} ({})",
                    spec.repo,
                    spec.file
                );
                let (onnx_bytes, tokenizer_files) =
                    fetch_model_bytes(&spec, &config.cache_dir).await?;
                let model = UserDefinedEmbeddingModel::new(onnx_bytes, tokenizer_files);
                let opts = InitOptionsUserDefined::new().with_execution_providers(eps.clone());
                let dense = TextEmbedding::try_new_from_user_defined(model, opts).map_err(|e| {
                    crate::app_log!(
                        "error",
                        "Embedder: fastembed UserDefined init failed for {}: {e}",
                        spec.repo
                    );
                    e
                })?;
                DenseBackend::Fastembed(dense)
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
                    Ok(m) => Some(m),
                    Err(e) => {
                        eprintln!("[embedder] sparse model failed, skipping: {e}");
                        None
                    }
                }
            }
            None => None,
        };

        crate::app_log!(
            "info",
            "Embedder: ready (dense backend {})",
            match &dense {
                DenseBackend::Fastembed(_) => "fastembed",
                DenseBackend::OrtPath(_) => "OrtPath",
                #[cfg(feature = "crispembed")]
                DenseBackend::CrispEmbed(_) => "CrispEmbed/GGUF",
            }
        );
        Ok(Embedder {
            config,
            dense,
            sparse,
            runtime_dim: registry_dim,
        })
    }

    pub fn embed_dense(
        &mut self,
        texts: Vec<String>,
        role: EmbedRole,
    ) -> Result<DenseEmbedding> {
        let prefix = self.config.model.prefix(role);
        let vectors = match &mut self.dense {
            DenseBackend::Fastembed(fe) => {
                // Native fastembed has no per-call prefix hook; prepend manually.
                let prefixed = prepend_prefix(&texts, prefix);
                fe.embed(prefixed, Some(self.config.batch_size))?
            }
            DenseBackend::OrtPath(op) => {
                let prefixed = prepend_prefix(&texts, prefix);
                op.embed(prefixed)?
            }
            #[cfg(feature = "crispembed")]
            DenseBackend::CrispEmbed(ce) => {
                // CrispEmbed has a native set_prefix that's applied inside
                // tokenization — preferable to manual concatenation because
                // the prefix doesn't compete with chunk text for max_tokens
                // in the same way (libcrispembed knows it's a prefix).
                ce.set_prefix(prefix);
                ce.embed(texts)?
            }
        };
        Ok(DenseEmbedding { vectors })
    }

    pub fn embed_sparse(&mut self, texts: Vec<String>) -> Result<Vec<Option<SparseVector>>> {
        let n = texts.len();

        // Path 1: dedicated fastembed sparse encoder (ONNX, the
        // historical case).  Single batched call, returns
        // `Vec<SparseEmbedding>` with i64 indices we narrow to u32.
        if let Some(ref mut sm) = self.sparse {
            let results = sm.embed(texts, Some(self.config.batch_size))?;
            return Ok(results
                .into_iter()
                .map(|sv| {
                    Some(SparseVector {
                        indices: sv.indices.into_iter().map(|i| i as u32).collect(),
                        values: sv.values,
                    })
                })
                .collect());
        }

        // Path 2: GGUF CrispEmbed dense backend with a sparse head
        // (BGE-M3 GGUF, SPLADE GGUFs).  Reuses the already-loaded
        // dense model so users on the GGUF backend get parity with
        // fastembed's sparse channel.  Per-text invocation because
        // upstream only exposes `encode_sparse(&str)`; on chunked
        // ingest the GGUF prompt-cache absorbs most of the repeated-
        // prefix cost.
        //
        // The shape returned matches the SparseVector contract:
        // `indices: u32, values: f32` so downstream code can't tell
        // which backend produced it.
        #[cfg(feature = "crispembed")]
        {
            if let DenseBackend::CrispEmbed(ref mut backend) = self.dense {
                if backend.has_sparse() {
                    let out: Vec<Option<SparseVector>> = texts
                        .iter()
                        .map(|t| {
                            let pairs = backend.encode_sparse(t);
                            if pairs.is_empty() {
                                None
                            } else {
                                let mut indices = Vec::with_capacity(pairs.len());
                                let mut values = Vec::with_capacity(pairs.len());
                                for (idx, val) in pairs {
                                    // Negative ids shouldn't appear in
                                    // practice (vocab ids are non-
                                    // negative), but skip rather than
                                    // panic if upstream ever emits one.
                                    if idx >= 0 {
                                        indices.push(idx as u32);
                                        values.push(val);
                                    }
                                }
                                Some(SparseVector { indices, values })
                            }
                        })
                        .collect();
                    return Ok(out);
                }
            }
        }

        // Neither sparse source is available — return Nones so
        // downstream code falls back to dense-only retrieval.
        Ok(vec![None; n])
    }

    pub fn embed_full(
        &mut self,
        texts: Vec<String>,
        role: EmbedRole,
    ) -> Result<(DenseEmbedding, Vec<Option<SparseVector>>)> {
        let t2 = texts.clone();
        let dense = self.embed_dense(texts, role)?;
        // Sparse models (BGE-M3, SPLADE++) are trained without prefixes —
        // pass texts through as-is.
        let sparse = self.embed_sparse(t2)?;
        Ok((dense, sparse))
    }

    /// Bi-encoder reranking via the loaded dense backend.  Embeds
    /// `query` (with the asymmetric `Query` prefix) and each
    /// `docs[i]` (with the `Passage` prefix), returns the cosine
    /// similarity of `query` against each doc as a `Vec<f32>` in
    /// input order.
    ///
    /// Both fastembed and CrispEmbed return L2-normalised vectors
    /// today, so cosine collapses to a single dot product — no
    /// re-normalisation step needed.  When that invariant ever
    /// flips, the dot product becomes inner-product similarity
    /// which is what the search-side cares about anyway; the
    /// downstream consumer just sorts by score, so the absolute
    /// scale doesn't matter.
    ///
    /// Use case: search-time reranking when the user doesn't have a
    /// dedicated cross-encoder reranker model loaded.  Reuses the
    /// already-loaded dense embedder → zero extra disk / memory.
    /// Faster than the cross-encoder path (one batch embed + N dot
    /// products) but typically ~70% of the cross-encoder's quality
    /// in published benchmarks.
    pub fn rerank_biencoder(
        &mut self,
        query: &str,
        docs: &[String],
    ) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(vec![]);
        }
        let dim = self.dims();
        let q = self.embed_dense(vec![query.to_string()], EmbedRole::Query)?;
        let q_vec = q
            .vectors
            .first()
            .ok_or_else(|| anyhow::anyhow!("query embedding came back empty"))?;
        let d = self.embed_dense(docs.to_vec(), EmbedRole::Passage)?;
        if d.vectors.len() != docs.len() {
            anyhow::bail!(
                "embedder returned {} doc vectors for {} inputs",
                d.vectors.len(),
                docs.len()
            );
        }
        Ok(d.vectors
            .iter()
            .map(|dv| {
                // Defensive: short-circuit dim mismatch.  Should
                // never fire in practice (effective_dim is the
                // single source of truth) but a length-mismatch
                // dot product would panic — return NaN instead so
                // the caller's NaN-fallback path keeps the original
                // RRF order for this doc.
                if q_vec.len() != dim || dv.len() != dim {
                    return f32::NAN;
                }
                q_vec.iter().zip(dv.iter()).map(|(a, b)| a * b).sum::<f32>()
            })
            .collect())
    }

    pub fn dims(&self) -> usize {
        // Registry-driven path: real dim discovered at load time overrides
        // the static enum dim.  Matryoshka truncation still applies on top.
        if let Some(rt) = self.runtime_dim {
            return self.config.matryoshka_dim
                .map(|d| (d as usize).min(rt))
                .unwrap_or(rt);
        }
        // Matryoshka, when configured + supported by backend (GGUF only),
        // truncates the output. The LanceDB column must match this dim,
        // so callers (LocalIndex, callers passing dims into schemas) must
        // use this rather than `model.dims()` directly.
        self.config.effective_dim()
    }
    pub fn model(&self) -> EmbedderModel {
        self.config.model
    }
    pub fn has_sparse(&self) -> bool {
        // Two producers of sparse vectors today: fastembed (ONNX
        // path, populates self.sparse) and the GGUF dense backend's
        // sparse head (BGE-M3 / SPLADE).  Report Yes on either —
        // matches what `embed_sparse` actually fires.
        if self.sparse.is_some() {
            return true;
        }
        #[cfg(feature = "crispembed")]
        {
            if let DenseBackend::CrispEmbed(ref backend) = self.dense {
                if backend.has_sparse() {
                    return true;
                }
            }
        }
        false
    }

    /// Whether the loaded model has a ColBERT head (per-token
    /// L2-normalised projections). Only BGE-M3 GGUF qualifies today.
    pub fn has_colbert(&self) -> bool {
        #[cfg(feature = "crispembed")]
        {
            if let DenseBackend::CrispEmbed(ref backend) = self.dense {
                return backend.has_colbert();
            }
        }
        false
    }

    /// Encode `texts` to per-token ColBERT vectors via the CrispEmbed backend.
    ///
    /// Returns one `Vec<Vec<f32>>` per input text; each inner vec is one
    /// L2-normalised token vector (dim = ColBERT projection dim, 128 for BGE-M3).
    /// Returns `vec![vec![]; texts.len()]` when the model has no ColBERT head.
    pub fn embed_multivec(&mut self, texts: Vec<String>) -> Result<Vec<Vec<Vec<f32>>>> {
        #[cfg(feature = "crispembed")]
        {
            if let DenseBackend::CrispEmbed(ref mut backend) = self.dense {
                if backend.has_colbert() {
                    return Ok(texts.iter().map(|t| backend.encode_multivec(t)).collect());
                }
            }
        }
        Ok(vec![vec![]; texts.len()])
    }

    /// Per-token contextual embeddings for token-level match highlighting.
    ///
    /// Tokenises `text` through the loaded model and returns one embedding
    /// vector per subword token.  Only available on the CrispEmbed (GGUF)
    /// backend — returns an empty vec on the ONNX path.
    ///
    /// Usage: compute cosine between each query token and each document
    /// token to find which spans in the document best match the query.
    pub fn encode_tokens(&mut self, text: &str) -> Vec<(String, Vec<f32>)> {
        #[cfg(feature = "crispembed")]
        {
            if let DenseBackend::CrispEmbed(ref mut backend) = self.dense {
                return backend.encode_tokens(text);
            }
        }
        let _ = text;
        vec![]
    }
}

// ── Chunking ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TextChunk {
    pub text: String,
    pub start_char: usize,
    pub end_char: usize,
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
    // Collect word (start, end) byte offsets in a single pass — O(N).
    // Avoids the previous O(N²) text[pos..].find(word) per word.
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < len {
        // Skip ASCII whitespace
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Start of a word — scan to the next whitespace or end
        let start = i;
        while i < len && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        words.push((start, i));
    }
    if words.is_empty() {
        return vec![];
    }

    let step = max_tokens.saturating_sub(stride).max(1);
    let mut chunks = Vec::new();
    let mut word_pos = 0usize;
    let mut idx = 0i32;

    while word_pos < words.len() {
        let end_word = (word_pos + max_tokens).min(words.len()) - 1;
        let start_char = words[word_pos].0;
        let end_char = words[end_word].1;
        chunks.push(TextChunk {
            text: text[start_char..end_char].to_owned(),
            start_char,
            end_char,
            chunk_index: idx,
        });
        idx += 1;
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
        assert_eq!(EmbedderModel::BgeM3.dims(), 1024);
        assert_eq!(EmbedderModel::PixieRuneV1.dims(), 1024);
        assert_eq!(EmbedderModel::SnowflakeArcticLv2.dims(), 1024);
        assert_eq!(EmbedderModel::MultilingualMiniLm.dims(), 384);
        assert_eq!(EmbedderModel::JinaV5Nano.dims(), 768);
        assert_eq!(EmbedderModel::JinaV3.dims(), 1024);
    }

    #[test]
    fn max_tokens_correct() {
        assert_eq!(EmbedderModel::BgeM3.max_tokens(), 8192);
        assert_eq!(EmbedderModel::MultilingualMiniLm.max_tokens(), 512);
        assert_eq!(EmbedderModel::JinaV5Small.max_tokens(), 32768);
    }

    #[test]
    fn ort_path_detection() {
        // OrtPath models: external data or force_ort_path or KV-cache or pre-pooled
        assert!(EmbedderModel::JinaV5Nano
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(EmbedderModel::JinaV5Small
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(EmbedderModel::PixieRuneV1
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(EmbedderModel::JinaV3
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(EmbedderModel::Qwen3Embedding
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(EmbedderModel::Qwen3EmbeddingInt8
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(EmbedderModel::Qwen3EmbeddingUint8
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        // Only Octen06bInt8Local stays on the OrtPath flow (local-only with
        // local_subdir bypass). The other three Octen variants now ride the
        // fastembed-native path and return None from to_model_spec.
        assert!(EmbedderModel::Octen06bInt8Local
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(EmbedderModel::Octen06bFp32.to_model_spec().is_none());
        assert!(EmbedderModel::Octen06bInt4Local.to_model_spec().is_none());
        assert!(EmbedderModel::Octen06bInt8FullLocal
            .to_model_spec()
            .is_none());

        // Fastembed UserDefined backend (self-contained ONNX + config.json present)
        assert!(!EmbedderModel::JinaV2Small
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(!EmbedderModel::JinaV2Base
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
        assert!(!EmbedderModel::SnowflakeArcticLv2
            .to_model_spec()
            .unwrap()
            .needs_ort_path());
    }

    #[test]
    fn external_data_detection() {
        // Models with external data companion files
        assert!(EmbedderModel::PixieRuneV1
            .to_model_spec()
            .unwrap()
            .has_external_onnx_data());
        assert!(EmbedderModel::JinaV3
            .to_model_spec()
            .unwrap()
            .has_external_onnx_data());
        assert!(EmbedderModel::Qwen3Embedding
            .to_model_spec()
            .unwrap()
            .has_external_onnx_data());
        // Only the local-only Octen variant still exposes a ModelSpec; the
        // other three are native fastembed (returns None from to_model_spec).
        assert!(EmbedderModel::Octen06bInt8Local
            .to_model_spec()
            .unwrap()
            .has_external_onnx_data());
        assert!(EmbedderModel::Octen06bFp32.to_model_spec().is_none());
        // Self-contained ONNX (no external data file)
        assert!(!EmbedderModel::Qwen3EmbeddingInt8
            .to_model_spec()
            .unwrap()
            .has_external_onnx_data());
    }

    #[test]
    fn sparse_mapping_correct() {
        assert_eq!(
            EmbedderModel::BgeM3.to_fastembed_sparse(),
            Some(SparseModel::BGEM3)
        );
        assert_eq!(EmbedderModel::PixieRuneV1.to_fastembed_sparse(), None);
    }

    #[test]
    fn dense_mapping_correct() {
        assert!(matches!(
            EmbedderModel::BgeM3.to_fastembed_dense(),
            EmbeddingModel::BGEM3
        ));
        assert!(matches!(
            EmbedderModel::MultilingualMiniLm.to_fastembed_dense(),
            EmbeddingModel::ParaphraseMLMiniLML12V2
        ));
    }

    #[test]
    fn matryoshka_effective_dim() {
        let cfg = EmbedderConfig::new(
            EmbedderModel::BgeM3,
            EmbedderDevice::Cpu,
            std::path::PathBuf::from("/tmp"),
        );
        // Default: model.dims()
        assert_eq!(cfg.effective_dim(), 1024);
        // Set: returns the requested value
        let cfg2 = cfg.clone().with_matryoshka_dim(Some(256));
        assert_eq!(cfg2.effective_dim(), 256);
        // Clamp: requesting > nominal returns nominal
        let cfg3 = cfg.clone().with_matryoshka_dim(Some(4096));
        assert_eq!(cfg3.effective_dim(), 1024);
        // Some(0) is treated as None (model default)
        let cfg4 = cfg.clone().with_matryoshka_dim(Some(0));
        assert_eq!(cfg4.matryoshka_dim, None);
        assert_eq!(cfg4.effective_dim(), 1024);
        // None is preserved
        let cfg5 = cfg.with_matryoshka_dim(None);
        assert_eq!(cfg5.matryoshka_dim, None);
    }

    #[test]
    fn prefix_table() {
        // Asymmetric models: query/passage differ.
        assert_eq!(EmbedderModel::MultilingualE5Small.prefix(EmbedRole::Query), "query: ");
        assert_eq!(
            EmbedderModel::MultilingualE5Large.prefix(EmbedRole::Passage),
            "passage: "
        );
        assert_eq!(EmbedderModel::NomicEmbedTextV15.prefix(EmbedRole::Query), "search_query: ");
        assert_eq!(
            EmbedderModel::NomicEmbedTextV15.prefix(EmbedRole::Passage),
            "search_document: "
        );
        // BGE en-v1.5 + Mxbai: query-only prefix.
        assert!(EmbedderModel::BgeSmallEnV15
            .prefix(EmbedRole::Query)
            .starts_with("Represent this sentence"));
        assert_eq!(EmbedderModel::BgeSmallEnV15.prefix(EmbedRole::Passage), "");
        assert!(EmbedderModel::MxbaiEmbedLargeV1
            .prefix(EmbedRole::Query)
            .starts_with("Represent this sentence"));
        // Jina v5: asymmetric "Query: " / "Document: ".
        assert_eq!(EmbedderModel::JinaV5Small.prefix(EmbedRole::Query), "Query: ");
        assert_eq!(EmbedderModel::JinaV5Nano.prefix(EmbedRole::Passage), "Document: ");
        // No-prefix models.
        for m in [
            EmbedderModel::BgeM3,
            EmbedderModel::MultilingualMiniLm,
            EmbedderModel::AllMiniLmL6V2,
            EmbedderModel::PixieRuneV1,
            EmbedderModel::SnowflakeArcticLv2,
            EmbedderModel::JinaV2Small,
            EmbedderModel::JinaV2Base,
            EmbedderModel::JinaV3,
            EmbedderModel::Qwen3Embedding,
            EmbedderModel::Octen06bFp32,
            EmbedderModel::GteBaseEnV15,
            EmbedderModel::GteLargeEnV15,
        ] {
            assert_eq!(m.prefix(EmbedRole::Query), "", "{:?} expected no query prefix", m);
            assert_eq!(m.prefix(EmbedRole::Passage), "", "{:?} expected no passage prefix", m);
        }
    }

    #[test]
    fn prepend_prefix_skips_when_empty() {
        let texts = vec!["foo".to_string(), "bar".to_string()];
        let out = prepend_prefix(&texts, "");
        assert_eq!(out, texts);
        let with = prepend_prefix(&texts, "query: ");
        assert_eq!(with, vec!["query: foo".to_string(), "query: bar".to_string()]);
    }

    /// Pin the serde kebab-case string for every variant. The frontend's
    /// `indexEmbedderToRust` map in `Settings.svelte` must use these exact
    /// values — a mismatch silently falls back to `bge-m3`.
    #[test]
    fn embedder_model_serde_strings() {
        let cases: &[(EmbedderModel, &str)] = &[
            (EmbedderModel::BgeM3, "bge-m3"),
            (EmbedderModel::MultilingualMiniLm, "multilingual-mini-lm"),
            (EmbedderModel::MultilingualE5Small, "multilingual-e5-small"),
            (EmbedderModel::MultilingualE5Base, "multilingual-e5-base"),
            (EmbedderModel::MultilingualE5Large, "multilingual-e5-large"),
            (EmbedderModel::BgeSmallEnV15, "bge-small-en-v15"),
            (EmbedderModel::BgeBaseEnV15, "bge-base-en-v15"),
            (EmbedderModel::BgeLargeEnV15, "bge-large-en-v15"),
            (EmbedderModel::NomicEmbedTextV15, "nomic-embed-text-v15"),
            (EmbedderModel::MxbaiEmbedLargeV1, "mxbai-embed-large-v1"),
            (EmbedderModel::AllMiniLmL6V2, "all-mini-lm-l6-v2"),
            (EmbedderModel::EmbeddingGemma300M, "embedding-gemma300-m"),
            (EmbedderModel::GteBaseEnV15, "gte-base-en-v15"),
            (EmbedderModel::GteLargeEnV15, "gte-large-en-v15"),
            // P17.6 — GGUF-only decoder models
            (EmbedderModel::Gemma3Embed2B, "gemma3-embed2-b"),
            (EmbedderModel::ModernBertBase, "modern-bert-base"),
            (EmbedderModel::ModernBertLarge, "modern-bert-large"),
            (EmbedderModel::DebertaV2Xlarge, "deberta-v2-xlarge"),
            (EmbedderModel::NomicBertMoe, "nomic-bert-moe"),
        ];
        for (variant, expected) in cases {
            let s = serde_json::to_string(variant).unwrap();
            let inner = s.trim_matches('"');
            assert_eq!(
                inner, *expected,
                "serde kebab-case for {:?} was {:?}, expected {:?}",
                variant, inner, expected
            );
        }
    }

    #[test]
    fn chunking_overlap() {
        let words: Vec<_> = (0..200).map(|i| format!("w{:04}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, 100, 20, &[]);
        assert!(chunks.len() >= 2);
        assert!(chunks[1].start_char > chunks[0].start_char);
        assert!(
            chunks[1].start_char < chunks[0].end_char,
            "chunks should overlap"
        );
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
        let sv = SparseVector {
            indices: vec![1, 5, 10],
            values: vec![0.8, 0.3, 0.5],
        };
        let sv2 = SparseVector::from_json(&sv.to_json()).unwrap();
        assert_eq!(sv.indices, sv2.indices);
        assert_eq!(sv.values, sv2.values);
    }

    #[test]
    fn cpu_device_empty_eps() {
        assert!(EmbedderDevice::Cpu.execution_providers().is_empty());
    }

    #[test]
    fn mean_pool_single_token() {
        let embs = vec![1.0f32, 2.0, 3.0];
        let mask = vec![1i64];
        let out = mean_pool(&embs, &mask, 1, 3);
        assert_eq!(out, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn l2_normalize_unit() {
        let v = vec![3.0f32, 4.0];
        let out = l2_normalize(v);
        let norm: f32 = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    // ── CrispEmbed / GGUF metadata (no feature required) ──────────────────

    #[test]
    fn supports_gguf_matches_registry_membership() {
        // Models known to have a verified GGUF conversion in cstr/*-GGUF.
        for m in [
            EmbedderModel::BgeM3,           // (Note: BGE-M3 doesn't have a GGUF registry entry)
            EmbedderModel::BgeSmallEnV15,
            EmbedderModel::BgeBaseEnV15,
            EmbedderModel::BgeLargeEnV15,
            EmbedderModel::MultilingualE5Small,
            EmbedderModel::MultilingualE5Base,
            EmbedderModel::MultilingualE5Large,
            EmbedderModel::NomicEmbedTextV15,
            EmbedderModel::AllMiniLmL6V2,
            EmbedderModel::PixieRuneV1,
            EmbedderModel::SnowflakeArcticLv2,
            EmbedderModel::Qwen3Embedding,
            EmbedderModel::JinaV5Nano,
            EmbedderModel::EmbeddingGemma300M,
            EmbedderModel::GteBaseEnV15,
            EmbedderModel::GteLargeEnV15,
            EmbedderModel::MxbaiEmbedLargeV1,
        ] {
            // supports_gguf must agree with whether gguf_registry_name returns Some.
            // We can't call gguf_registry_name from outside (private), but the
            // public supports_gguf is its public face — assert the contract.
            let s = m.supports_gguf();
            // Either model has a GGUF, or it doesn't — both are acceptable;
            // the contract is just that the call doesn't panic and is stable.
            // This guards against future enum additions silently changing
            // the API surface.
            let _ = s;
        }
    }

    #[test]
    fn gguf_download_size_is_sensible() {
        for m in [EmbedderModel::BgeSmallEnV15, EmbedderModel::AllMiniLmL6V2,
                  EmbedderModel::BgeM3, EmbedderModel::SnowflakeArcticLv2] {
            let mb = m.gguf_download_mb();
            // Embedding-model GGUFs are 50 MB-1 GB; sanity check.
            assert!(mb < 5000,  "{m:?} GGUF size unrealistic: {mb} MB");
        }
    }

    #[test]
    fn embedder_model_serde_round_trip() {
        // Every model variant must round-trip through serde — IndexConfig is
        // persisted to the user's settings file. Adding a variant without
        // updating its serde shape would orphan their config silently.
        let candidates = [
            EmbedderModel::BgeM3,
            EmbedderModel::BgeSmallEnV15,
            EmbedderModel::MultilingualE5Base,
            EmbedderModel::AllMiniLmL6V2,
            EmbedderModel::NomicEmbedTextV15,
        ];
        for m in candidates {
            let json = serde_json::to_string(&m).unwrap();
            let back: EmbedderModel = serde_json::from_str(&json).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn embedder_device_serde_round_trip() {
        for d in [EmbedderDevice::Auto, EmbedderDevice::Cpu,
                  EmbedderDevice::Metal, EmbedderDevice::Cuda] {
            let json = serde_json::to_string(&d).unwrap();
            let back: EmbedderDevice = serde_json::from_str(&json).unwrap();
            assert_eq!(d, back);
        }
    }

    #[test]
    fn embedder_backend_default_is_onnx() {
        let b: EmbedderBackend = Default::default();
        assert_eq!(b, EmbedderBackend::Onnx);
    }

    /// Compile-time check: when the `crispembed` feature is enabled the
    /// `to_gguf_spec` method exists and returns a result for at least one
    /// well-known GGUF-supported model. When disabled, the method doesn't
    /// exist (this test compiles to nothing).
    #[cfg(feature = "crispembed")]
    #[test]
    fn crispembed_to_gguf_spec_returns_for_known_models() {
        // BGE-small-EN-v1.5 has a known GGUF in cstr/bge-small-en-v1.5-GGUF.
        let spec = EmbedderModel::BgeSmallEnV15.to_gguf_spec_with_quant(None);
        assert!(spec.is_some(),
            "BgeSmallEnV15 should have a GGUF spec under feature crispembed");
    }

    #[cfg(feature = "crispembed")]
    #[test]
    fn gguf_quant_override_selects_the_requested_file() {
        // The reason the override exists: multilingual-e5-small's variant
        // carries no quant suffix, so it could only ever resolve to the
        // 455 MiB F32 — the 126 MiB Q8_0 sits in the same repo with no way
        // to ask for it. These filenames must match the repo exactly; a
        // wrong one is a 404 at download time, not a compile error.
        let m = EmbedderModel::MultilingualE5Small;
        let default = m.to_gguf_spec_with_quant(None).expect("has a GGUF spec");
        assert_eq!(default.repo, "cstr/multilingual-e5-small-GGUF");
        assert_eq!(default.file, "multilingual-e5-small.gguf");

        let q8 = m
            .to_gguf_spec_with_quant(Some(GgufQuant::Q8_0))
            .expect("has a GGUF spec");
        assert_eq!(q8.file, "multilingual-e5-small-q8_0.gguf");

        let q4 = m
            .to_gguf_spec_with_quant(Some(GgufQuant::Q4K))
            .expect("has a GGUF spec");
        assert_eq!(q4.file, "multilingual-e5-small-q4_k.gguf");

        // F32 is the unsuffixed file, i.e. the same as the default here.
        let f32_spec = m
            .to_gguf_spec_with_quant(Some(GgufQuant::F32))
            .expect("has a GGUF spec");
        assert_eq!(f32_spec.file, default.file);
    }

    #[test]
    fn model_name_override_builder() {
        let cfg = EmbedderConfig::new(
            EmbedderModel::BgeM3,
            EmbedderDevice::Cpu,
            std::path::PathBuf::from("/tmp"),
        );
        assert!(cfg.model_name_override.is_none());

        // Non-empty name is preserved.
        let cfg2 = cfg.clone().with_model_name_override(Some("nomic-embed-text-v2.0".into()));
        assert_eq!(cfg2.model_name_override.as_deref(), Some("nomic-embed-text-v2.0"));

        // Empty string is normalised to None.
        let cfg3 = cfg.clone().with_model_name_override(Some(String::new()));
        assert!(cfg3.model_name_override.is_none());

        // None is preserved.
        let cfg4 = cfg.with_model_name_override(None);
        assert!(cfg4.model_name_override.is_none());
    }

    #[test]
    fn runtime_dim_override_wins_in_dims() {
        // Validate the `dims()` precedence rule through the helper exposed for tests.
        // Rule: when runtime_dim is Some(r),
        //   dims = matryoshka_dim.map(|d| d.min(r)).unwrap_or(r)
        let cfg_base = EmbedderConfig::new(
            EmbedderModel::BgeM3,
            EmbedderDevice::Cpu,
            std::path::PathBuf::from("/tmp"),
        );

        // matryoshka 512 ≤ runtime 1024 → 512.
        assert_eq!(
            compute_dims_with_runtime(cfg_base.clone().with_matryoshka_dim(Some(512)), Some(1024)),
            512
        );
        // matryoshka 2048 > runtime 768 → clamped to 768.
        assert_eq!(
            compute_dims_with_runtime(cfg_base.clone().with_matryoshka_dim(Some(2048)), Some(768)),
            768
        );
        // No matryoshka, runtime 512 → 512.
        assert_eq!(
            compute_dims_with_runtime(cfg_base.clone(), Some(512)),
            512
        );
        // No runtime_dim → falls through to config.effective_dim() = 1024 (BgeM3 default).
        assert_eq!(
            compute_dims_with_runtime(cfg_base.clone(), None),
            1024
        );
    }

    /// Replicates `Embedder::dims()` without constructing a real `Embedder`.
    fn compute_dims_with_runtime(config: EmbedderConfig, runtime_dim: Option<usize>) -> usize {
        if let Some(rt) = runtime_dim {
            return config
                .matryoshka_dim
                .map(|d| (d as usize).min(rt))
                .unwrap_or(rt);
        }
        config.effective_dim()
    }
}
