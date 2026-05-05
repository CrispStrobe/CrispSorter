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
//! Auto-downloads the Whisper-base GGUF on first use via
//! `crispasr::cache_ensure_file`. Models are placed under the same
//! `model_cache_dir` the embedder uses, so a single configurable path
//! controls every weight on disk.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(feature = "crispasr")]
use anyhow::Context;

/// Speech-recognition model picker. Currently only Whisper is exposed —
/// the CrispASR registry has more (parakeet, canary, voxtral, granite,
/// qwen3, cohere, wav2vec2) but Whisper is the right default for a
/// chat push-to-talk in 100+ languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AsrModel {
    /// Whisper-base — 244 MB, multilingual, 99 languages.
    #[default]
    Whisper,
}

impl AsrModel {
    /// Backend name in the CrispASR registry. Used by
    /// `registry_lookup(backend)` to resolve filename + download URL.
    pub fn registry_backend(&self) -> &'static str {
        match self {
            AsrModel::Whisper => "whisper",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            AsrModel::Whisper => "Whisper-base (multilingual, ~244 MB)",
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
    model: AsrModel,
}

impl Asr {
    /// Async constructor: looks up the model in CrispASR's registry,
    /// auto-downloads to `cache_dir` if absent, then opens the session.
    #[cfg(feature = "crispasr")]
    pub async fn load(model: AsrModel, cache_dir: PathBuf) -> Result<Self> {
        let backend = model.registry_backend();
        let entry = crispasr::registry_lookup(backend)
            .map_err(|e| anyhow::anyhow!("ASR registry lookup failed: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("ASR backend `{backend}` not in CrispASR registry"))?;

        let cache_dir_str = cache_dir.to_string_lossy();
        // CrispASR's cache helper is sync but cheap on a cache hit; for
        // first-run downloads it streams the file via libcrispasr — which
        // can take a few seconds on a fast link. Run on a blocking pool so
        // the Tauri runtime stays responsive.
        let path = tokio::task::spawn_blocking({
            let filename = entry.filename.clone();
            let url = entry.url.clone();
            let cache_dir_str = cache_dir_str.into_owned();
            move || {
                crispasr::cache_ensure_file(&filename, &url, false, Some(&cache_dir_str))
                    .map_err(|e| anyhow::anyhow!("ASR cache_ensure_file failed: {e}"))
            }
        })
        .await
        .context("spawn_blocking joined unexpectedly")??
        .ok_or_else(|| anyhow::anyhow!("ASR cache returned no path for {}", entry.filename))?;

        println!("[asr] Loading session: {path}");
        let session = tokio::task::spawn_blocking(move || {
            crispasr::Session::open(&path)
                .map_err(|e| anyhow::anyhow!("crispasr::Session::open failed: {e}"))
        })
        .await
        .context("spawn_blocking joined unexpectedly")??;

        Ok(Self { session, model })
    }

    #[cfg(not(feature = "crispasr"))]
    pub async fn load(_model: AsrModel, _cache_dir: PathBuf) -> Result<Self> {
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
        let segments = self
            .session
            .transcribe(pcm)
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
}

// ── Lazy-load handle ─────────────────────────────────────────────────────

/// Cheap-clonable handle held in `AppState`. First `transcribe` call
/// downloads + opens the session; subsequent calls reuse it. Mirrors
/// `RerankerHandle` from `index/reranker.rs`.
#[derive(Clone)]
pub struct AsrHandle {
    model: AsrModel,
    cache_dir: PathBuf,
    slot: Arc<Mutex<Option<Asr>>>,
}

impl AsrHandle {
    pub fn new(model: AsrModel, cache_dir: PathBuf) -> Self {
        Self {
            model,
            cache_dir,
            slot: Arc::new(Mutex::new(None)),
        }
    }

    pub fn model(&self) -> AsrModel {
        self.model
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
        let mut guard = self.slot.lock().await;
        if guard.is_none() {
            let asr = Asr::load(self.model, self.cache_dir.clone()).await?;
            *guard = Some(asr);
        }
        let asr = guard.as_ref().unwrap();
        asr.transcribe(&pcm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asr_model_serde() {
        let s = serde_json::to_string(&AsrModel::Whisper).unwrap();
        assert_eq!(s.trim_matches('"'), "whisper");
        let back: AsrModel = serde_json::from_str(&s).unwrap();
        assert_eq!(back, AsrModel::Whisper);
    }

    #[test]
    fn registry_backend_matches_crispasr_naming() {
        // CrispASR's list_known_models() includes "whisper" — the
        // backend string we ship must match exactly so registry_lookup
        // resolves a row.
        assert_eq!(AsrModel::Whisper.registry_backend(), "whisper");
    }
}
