//! Cross-encoder reranking via CrispEmbed.
//!
//! Sits alongside the dense `Embedder`: lazy-loaded on first query when
//! `IndexConfig.reranker_model` is `Some(_)`. Holds its own
//! `crispembed::CrispEmbed` instance — separate model file, separate memory.
//!
//! Without the `crispembed` cargo feature this module compiles to stubs
//! that error if the user toggles reranking on. The UI gates the
//! configuration on `feature = "crispembed"` via the existing
//! `GGUF_CAPABLE_MODELS` flow.
//!
//! Wired into the search pipeline:
//!   1. `SearchEngine::search_*` runs FTS + ANN + RRF as today.
//!   2. If a `Reranker` is present, take top-`rerank_top_n` candidates,
//!      score each via `Reranker::score(query, doc_text)`, sort by score
//!      desc, truncate to `limit`.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(feature = "crispembed")]
use anyhow::Context;
#[cfg(feature = "crispembed")]
use hf_hub::api::tokio::ApiBuilder;

/// Cross-encoder reranker shortlist. GGUF-only — these are CrispEmbed
/// models with `is_reranker() == true`. Mirrors a slice of
/// `examples/cli/model_mgr.cpp` in CrispEmbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RerankerModel {
    /// BAAI/bge-reranker-v2-m3 — multilingual, default. ~570 MB Q8_0.
    BgeRerankerV2M3,
    /// BAAI/bge-reranker-base — English baseline, smaller. ~280 MB Q8_0.
    BgeRerankerBase,
    /// jinaai/jina-reranker-v2-base-multilingual — multilingual alt.
    JinaRerankerV2BaseMultilingual,
}

impl RerankerModel {
    pub fn display_name(&self) -> &'static str {
        match self {
            RerankerModel::BgeRerankerV2M3 => "BGE-Reranker v2-m3 (multilingual, default)",
            RerankerModel::BgeRerankerBase => "BGE-Reranker base (English baseline)",
            RerankerModel::JinaRerankerV2BaseMultilingual => {
                "Jina-Reranker v2 base (multilingual alt)"
            }
        }
    }

    /// Short-name in the `cstr/<name>-GGUF` HuggingFace registry, mirroring
    /// `EmbedderModel::gguf_registry_name()` for the embedder side.
    pub fn gguf_registry_name(&self) -> &'static str {
        match self {
            RerankerModel::BgeRerankerV2M3 => "bge-reranker-v2-m3",
            RerankerModel::BgeRerankerBase => "bge-reranker-base",
            RerankerModel::JinaRerankerV2BaseMultilingual => {
                "jina-reranker-v2-base-multilingual"
            }
        }
    }

    /// HuggingFace repo id and filename in the `cstr/<name>-GGUF` registry.
    /// The base file uses `<name>.gguf` (no `-q8_0` suffix) — matches the
    /// CrispEmbed `model_mgr.cpp` registry line for each of these three
    /// reranker entries.
    pub fn gguf_spec(&self) -> RerankerGgufSpec {
        let name = self.gguf_registry_name();
        RerankerGgufSpec {
            repo: format!("cstr/{name}-GGUF"),
            file: format!("{name}.gguf"),
        }
    }
}

/// HF repo id + filename for a reranker GGUF.
#[derive(Debug, Clone)]
pub struct RerankerGgufSpec {
    pub repo: String,
    pub file: String,
}

// ── Reranker backend ────────────────────────────────────────────────────────

/// Cross-encoder reranker. Holds an opened CrispEmbed model instance.
///
/// Without the `crispembed` cargo feature this is a stub that errors on
/// any operation — keeps call sites unconditional and avoids feature-gate
/// proliferation in `SearchEngine`.
pub struct Reranker {
    #[cfg(feature = "crispembed")]
    model: crispembed::CrispEmbed,
    #[allow(dead_code)]
    spec: RerankerGgufSpec,
}

impl Reranker {
    /// Async constructor: ensures the GGUF is on disk (downloading via
    /// hf-hub if needed), then opens it through CrispEmbed and verifies
    /// `is_reranker() == true`.
    #[cfg(feature = "crispembed")]
    pub async fn load(model: RerankerModel, cache_dir: &Path) -> Result<Self> {
        let spec = model.gguf_spec();
        let path = ensure_reranker_on_disk(&spec, cache_dir).await?;
        let p = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 reranker GGUF path: {:?}", path))?;
        println!("[reranker] Loading GGUF: {}", p);
        let m = crispembed::CrispEmbed::new(p, 0)
            .map_err(|e| anyhow::anyhow!("crispembed reranker load failed: {e}"))?;
        if !m.is_reranker() {
            anyhow::bail!(
                "model at {} is not a cross-encoder reranker (CrispEmbed reports is_reranker=false)",
                p
            );
        }
        Ok(Self { model: m, spec })
    }

    #[cfg(not(feature = "crispembed"))]
    pub async fn load(_model: RerankerModel, _cache_dir: &Path) -> Result<Self> {
        anyhow::bail!(
            "reranking requires the `crispembed` cargo feature \
             (build with --features crispembed-metal / -cuda / -vulkan)"
        );
    }

    /// Score one (query, document) pair. Higher = more relevant.
    /// Returns `f32::NAN` if scoring fails — caller should fall back to
    /// the original RRF score in that case.
    #[cfg(feature = "crispembed")]
    pub fn score(&mut self, query: &str, document: &str) -> f32 {
        self.model.rerank(query, document)
    }

    #[cfg(not(feature = "crispembed"))]
    pub fn score(&mut self, _query: &str, _document: &str) -> f32 {
        f32::NAN
    }
}

// ── Lazy-load handle threaded through SearchEngine ──────────────────────────

/// Cheaply-clonable handle for the search pipeline. The first
/// `score_batch` call loads the GGUF (downloading if absent) and caches the
/// model behind the inner mutex; subsequent calls reuse it.
///
/// Failures during load are logged once per call site and produce
/// `f32::NAN` scores so the caller can fall back to the original RRF order
/// rather than hard-erroring the whole query.
#[derive(Clone)]
pub struct RerankerHandle {
    model: RerankerModel,
    cache_dir: PathBuf,
    slot: Arc<Mutex<Option<Reranker>>>,
}

impl RerankerHandle {
    pub fn new(model: RerankerModel, cache_dir: PathBuf) -> Self {
        Self {
            model,
            cache_dir,
            slot: Arc::new(Mutex::new(None)),
        }
    }

    pub fn model(&self) -> RerankerModel {
        self.model
    }

    /// Score `docs` against `query`. Returns one f32 per doc in input order.
    /// On load or scoring failure, returns NaN for the affected entries.
    pub async fn score_batch(&self, query: &str, docs: &[&str]) -> Vec<f32> {
        if docs.is_empty() {
            return vec![];
        }
        let mut guard = self.slot.lock().await;
        if guard.is_none() {
            match Reranker::load(self.model, &self.cache_dir).await {
                Ok(r) => {
                    *guard = Some(r);
                }
                Err(e) => {
                    eprintln!("[reranker] load failed, falling back: {e:#}");
                    return vec![f32::NAN; docs.len()];
                }
            }
        }
        // Safe: just populated above (or pre-populated).
        let r = guard.as_mut().unwrap();
        docs.iter().map(|d| r.score(query, d)).collect()
    }
}

#[cfg(feature = "crispembed")]
async fn ensure_reranker_on_disk(
    spec: &RerankerGgufSpec,
    cache_dir: &Path,
) -> Result<PathBuf> {
    let api = ApiBuilder::new()
        .with_cache_dir(cache_dir.to_path_buf())
        .build()
        .context("Failed to build hf-hub Api for reranker")?;
    let model_api = api.model(spec.repo.clone());
    println!("[reranker] Fetching GGUF: {}/{} …", spec.repo, spec.file);
    model_api
        .get(&spec.file)
        .await
        .with_context(|| format!("failed to get {}/{}", spec.repo, spec.file))
}

#[cfg(not(feature = "crispembed"))]
#[allow(dead_code)]
async fn ensure_reranker_on_disk(
    _spec: &RerankerGgufSpec,
    _cache_dir: &Path,
) -> Result<PathBuf> {
    anyhow::bail!("crispembed feature disabled — reranker GGUF cannot be downloaded");
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the serde kebab-case strings so the frontend mapper stays in lockstep.
    #[test]
    fn reranker_model_serde_strings() {
        let cases: &[(RerankerModel, &str)] = &[
            (RerankerModel::BgeRerankerV2M3, "bge-reranker-v2-m3"),
            (RerankerModel::BgeRerankerBase, "bge-reranker-base"),
            (
                RerankerModel::JinaRerankerV2BaseMultilingual,
                "jina-reranker-v2-base-multilingual",
            ),
        ];
        for (variant, expected) in cases {
            let s = serde_json::to_string(variant).unwrap();
            let inner = s.trim_matches('"');
            assert_eq!(inner, *expected, "serde for {:?}", variant);
        }
    }

    #[test]
    fn gguf_spec_matches_registry() {
        assert_eq!(
            RerankerModel::BgeRerankerV2M3.gguf_spec().file,
            "bge-reranker-v2-m3.gguf"
        );
        assert_eq!(
            RerankerModel::BgeRerankerV2M3.gguf_spec().repo,
            "cstr/bge-reranker-v2-m3-GGUF"
        );
        assert_eq!(
            RerankerModel::JinaRerankerV2BaseMultilingual.gguf_spec().repo,
            "cstr/jina-reranker-v2-base-multilingual-GGUF"
        );
    }
}
