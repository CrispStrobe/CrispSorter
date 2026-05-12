//! On-device speech-to-text via CrispASR.
//!
//! Mirrors the CrispEmbed integration pattern: optional sibling path-dep
//! (`../../CrispASR/crispasr`) gated behind the `crispasr` cargo feature.
//! Without the feature this module compiles to stubs that error if the
//! frontend invokes `asr_transcribe`.
//!
//! ```ignore
//! // From the frontend:
//! const text = await invoke('asr_transcribe', { pcm: float32Array });
//! ```
//!
//! Auto-downloads the configured backend's canonical GGUF on first use
//! via `crispasr::cache_ensure_file`. Models are placed under the same
//! `model_cache_dir` the embedder uses, so a single configurable path
//! controls every weight on disk.
//!
//! ## Backend coverage
//!
//! All 24 ASR backends from the CrispASR registry are accessible — pick
//! any of `whisper`, `parakeet`, `canary`, `qwen3`, `distil-whisper`,
//! `cohere`, `granite{,-4.1,-4.1-plus,-4.1-nar}`, `fastconformer-ctc`,
//! `voxtral{,4b}`, `wav2vec2`, `glm-asr`, `kyutai-stt`, `firered-asr`,
//! `moonshine{,-streaming}`, `omniasr{,-llm}`, `vibevoice`, `gemma4-e2b`,
//! `mimo-asr` via [`AsrConfig::backend`]. Run `crispasr::list_known_models()`
//! for the live set.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(feature = "crispasr")]
use anyhow::Context;

/// ASR session configuration — backend name + optional explicit model
/// path.  All 24 backends from the CrispASR registry are supported;
/// see [the module docs](self) for the curated list.
///
/// **Speed-tier guidance for callers:**
/// - `whisper` — 99 languages, balanced default (`whisper-base` ≈ 244 MB)
/// - `parakeet` — 25 EU languages, FastConformer-TDT (much faster than whisper)
/// - `distil-whisper` — 6.3× faster than whisper, English-only
/// - `moonshine{-streaming}` — 34-245 M, designed for streaming
/// - `omniasr` — CTC, 1600+ languages, fastest at the cost of features
///
/// The indexer/batch pipeline should override [`Self::default()`] to a
/// faster backend; the GUI push-to-talk keeps whisper for back-compat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AsrConfig {
    /// Backend name from the crispasr registry.  Pass any of the
    /// strings returned by `crispasr::list_known_models()`.
    pub backend: String,
    /// Optional explicit GGUF path.  When `None`, `registry_lookup`
    /// + `cache_ensure_file` resolve a canonical model file for
    /// the backend and download on first use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_path: Option<String>,
}

impl AsrConfig {
    /// Construct a config with an auto-downloaded canonical model.
    pub fn new(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            model_path: None,
        }
    }

    /// Construct a config with an explicit model path (skips
    /// `registry_lookup` + `cache_ensure_file`).
    pub fn with_model_path(backend: impl Into<String>, model_path: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            model_path: Some(model_path.into()),
        }
    }

    /// Human-readable label for logs / UI.  Includes the model path
    /// when explicit, just the backend name otherwise.
    pub fn display_name(&self) -> String {
        match &self.model_path {
            Some(p) => format!("{} ({})", self.backend, p),
            None => self.backend.clone(),
        }
    }
}

impl Default for AsrConfig {
    fn default() -> Self {
        // whisper is the most general default (99 languages, shipped
        // in current releases — back-compat with the GUI push-to-talk
        // that landed pre-refactor).  Indexer/batch callers should
        // override to a faster backend per the speed-tier table in
        // PLAN.md / the module docs.
        Self {
            backend: "whisper".to_string(),
            model_path: None,
        }
    }
}

// ── Asr backend ──────────────────────────────────────────────────────────

/// Loaded ASR session. Wraps `crispasr::Session` under the feature gate;
/// stub form errors on every operation when the feature is off so the
/// caller gets a clear "build without `crispasr`" message instead of a
/// silent failure.
pub struct Asr {
    #[cfg(feature = "crispasr")]
    session: crispasr::Session,
    #[allow(dead_code)]
    config: AsrConfig,
}

impl Asr {
    /// Async constructor: looks up the model in CrispASR's registry,
    /// auto-downloads to `cache_dir` if absent, then opens the session.
    /// When [`AsrConfig::model_path`] is set the registry lookup is
    /// skipped and the path is opened directly.
    #[cfg(feature = "crispasr")]
    pub async fn load(config: AsrConfig, cache_dir: PathBuf) -> Result<Self> {
        let backend = config.backend.clone();
        let cache_dir_str = cache_dir.to_string_lossy().into_owned();

        // Resolve the model path: explicit > registry auto-download.
        let model_path = if let Some(p) = config.model_path.clone() {
            p
        } else {
            let backend_for_lookup = backend.clone();
            let cache_for_lookup = cache_dir_str.clone();
            tokio::task::spawn_blocking(move || -> Result<String> {
                let entry = crispasr::registry_lookup(&backend_for_lookup)
                    .map_err(|e| anyhow::anyhow!("ASR registry lookup failed: {e}"))?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "ASR backend `{backend_for_lookup}` not in CrispASR registry"
                        )
                    })?;
                let path = crispasr::cache_ensure_file(
                    &entry.filename,
                    &entry.url,
                    false,
                    Some(&cache_for_lookup),
                )
                .map_err(|e| anyhow::anyhow!("ASR cache_ensure_file failed: {e}"))?
                .ok_or_else(|| {
                    anyhow::anyhow!("ASR cache returned no path for {}", entry.filename)
                })?;
                Ok(path)
            })
            .await
            .context("spawn_blocking joined unexpectedly")??
        };

        println!("[asr] Loading session: backend={} path={}", backend, model_path);
        let session_path = model_path.clone();
        let session_backend = backend.clone();
        let session = tokio::task::spawn_blocking(move || {
            // Use open_with_backend so the C++ side skips its auto-detect
            // and goes straight to the requested family — matches the
            // CLI shape `crispasr --backend X -m ...`.
            crispasr::Session::open_with_backend(&session_path, &session_backend, 4)
                .map_err(|e| anyhow::anyhow!("crispasr::Session::open failed: {e}"))
        })
        .await
        .context("spawn_blocking joined unexpectedly")??;

        Ok(Self { session, config })
    }

    #[cfg(not(feature = "crispasr"))]
    pub async fn load(_config: AsrConfig, _cache_dir: PathBuf) -> Result<Self> {
        anyhow::bail!(
            "speech-to-text requires the `crispasr` cargo feature \
             (build with --features crispasr-metal / -cuda / -vulkan)"
        );
    }

    /// Transcribe 16 kHz mono Float32 PCM. Concatenates all segments into
    /// a single string. Returns an empty string for silence / VAD-rejected
    /// audio rather than erroring.
    #[cfg(feature = "crispasr")]
    pub fn transcribe(&self, pcm: &[f32]) -> Result<String> {
        self.transcribe_with_language(pcm, None)
    }

    /// Language-aware transcribe — passes the ISO 639-1 code to the
    /// backend (`"en"`, `"de"`, `"ja"`, …).  Backends that accept a
    /// source-language hint honour it; others ignore silently.  Pass
    /// `None` for backend-default behaviour (matches [`Self::transcribe`]).
    #[cfg(feature = "crispasr")]
    pub fn transcribe_with_language(
        &self,
        pcm: &[f32],
        language: Option<&str>,
    ) -> Result<String> {
        let segments = self
            .session
            .transcribe_with_language(pcm, language)
            .map_err(|e| anyhow::anyhow!("ASR transcribe failed: {e}"))?;
        let mut out = String::new();
        for seg in segments {
            if !out.is_empty() && !seg.text.is_empty() {
                out.push(' ');
            }
            out.push_str(seg.text.trim());
        }
        Ok(out)
    }

    #[cfg(not(feature = "crispasr"))]
    pub fn transcribe(&self, _pcm: &[f32]) -> Result<String> {
        anyhow::bail!("crispasr feature disabled")
    }

    #[cfg(not(feature = "crispasr"))]
    pub fn transcribe_with_language(&self, _pcm: &[f32], _language: Option<&str>) -> Result<String> {
        anyhow::bail!("crispasr feature disabled")
    }
}

// ── Lazy-load handle ─────────────────────────────────────────────────────

/// Cheap-clonable handle held in `AppState`. First `transcribe` call
/// downloads + opens the session; subsequent calls reuse it. Mirrors
/// `RerankerHandle` from `index/reranker.rs`.
#[derive(Clone)]
pub struct AsrHandle {
    config: AsrConfig,
    cache_dir: PathBuf,
    slot: Arc<Mutex<Option<Asr>>>,
}

impl AsrHandle {
    pub fn new(config: AsrConfig, cache_dir: PathBuf) -> Self {
        Self {
            config,
            cache_dir,
            slot: Arc::new(Mutex::new(None)),
        }
    }

    pub fn config(&self) -> &AsrConfig {
        &self.config
    }

    /// Transcribe `pcm` (Float32, 16 kHz, mono). On load failure returns
    /// the underlying error so the frontend can show a meaningful toast.
    ///
    /// The Mutex serializes calls, which matches `crispasr::Session`'s
    /// !Sync requirement — only one transcription at a time per handle.
    /// CrispASR's transcribe is synchronous and CPU-heavy; we hold the
    /// guard across the call. For chat push-to-talk (single user, short
    /// utterances) this blocks one runtime worker thread for ~1–3s,
    /// which is acceptable. Long-form transcription should use a
    /// dedicated worker outside this module.
    pub async fn transcribe(&self, pcm: Vec<f32>) -> Result<String> {
        self.transcribe_with_language(pcm, None).await
    }

    /// Language-aware variant of [`Self::transcribe`].  See
    /// [`Asr::transcribe_with_language`] for the language-hint semantics.
    pub async fn transcribe_with_language(
        &self,
        pcm: Vec<f32>,
        language: Option<String>,
    ) -> Result<String> {
        let mut guard = self.slot.lock().await;
        if guard.is_none() {
            let asr = Asr::load(self.config.clone(), self.cache_dir.clone()).await?;
            *guard = Some(asr);
        }
        let asr = guard.as_ref().unwrap();
        asr.transcribe_with_language(&pcm, language.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asr_config_serde_round_trip() {
        // Default config: whisper backend, no explicit path.  Must
        // serialise without the `model_path` key (skip_serializing_if)
        // and round-trip back identical.
        let cfg = AsrConfig::default();
        let s = serde_json::to_string(&cfg).unwrap();
        assert_eq!(s, r#"{"backend":"whisper"}"#);
        let back: AsrConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn asr_config_serde_with_explicit_model_path() {
        // When the caller passes an explicit model path, it round-trips
        // intact — backend + path both present.
        let cfg = AsrConfig::with_model_path("parakeet", "/tmp/parakeet-tdt-0.6b.gguf");
        let s = serde_json::to_string(&cfg).unwrap();
        let back: AsrConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.backend, "parakeet");
        assert_eq!(back.model_path.as_deref(), Some("/tmp/parakeet-tdt-0.6b.gguf"));
    }

    #[test]
    fn asr_config_constructors() {
        // new() defaults model_path to None — registry auto-download path.
        let cfg = AsrConfig::new("canary");
        assert_eq!(cfg.backend, "canary");
        assert!(cfg.model_path.is_none());

        // with_model_path() takes both backend + path.
        let cfg = AsrConfig::with_model_path("qwen3", "/var/models/qwen3-asr.gguf");
        assert_eq!(cfg.backend, "qwen3");
        assert_eq!(cfg.model_path.as_deref(), Some("/var/models/qwen3-asr.gguf"));
    }

    #[test]
    fn asr_config_display_name() {
        // Plain backend name when no explicit path.
        let cfg = AsrConfig::new("whisper");
        assert_eq!(cfg.display_name(), "whisper");

        // Backend + path in parens when explicit.
        let cfg = AsrConfig::with_model_path("parakeet", "/m/p.gguf");
        assert_eq!(cfg.display_name(), "parakeet (/m/p.gguf)");
    }

    #[test]
    fn asr_config_default_is_whisper() {
        // The default backend is `whisper` for back-compat with the
        // shipped GUI push-to-talk — if this ever changes, audit the
        // existing AppState init at src-tauri/src/lib.rs:603 first.
        assert_eq!(AsrConfig::default().backend, "whisper");
        assert!(AsrConfig::default().model_path.is_none());
    }
}
