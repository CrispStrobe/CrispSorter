//! LID-driven backend routing applied to a transcribe call (P13.5
//! Phase 6).
//!
//! Sits one layer above [`super::AsrHandle::transcribe_with_language`]:
//! takes a `BackendFallback` policy and (optionally) an LID model,
//! decides which backend should actually handle the audio via
//! [`super::lang::route`], and runs the transcribe against that
//! backend.  Returns a [`TranscribeResult`] capturing the decision so
//! callers can show "decoded by `whisper` because parakeet doesn't
//! speak Japanese" in their UI.
//!
//! ## Fast path
//!
//! When `policy == BackendFallback::AsConfigured` (the default for the
//! existing single-backend surfaces), the orchestrator is a no-op
//! delegator — it skips LID entirely and forwards the PCM to
//! `primary.transcribe_with_language`.  Adding the orchestrator to
//! the chat-transcribe path therefore costs nothing for the existing
//! behaviour; opting into LID routing is the user's explicit
//! `--policy strict|auto|translate` choice.
//!
//! ## Tests
//!
//! Unit tests in this module cover the input-validation surface:
//!   * `policy != AsConfigured` with neither `--language ISO` nor an
//!     LID model errors with a clear "either supply X or Y" message.
//!   * Malformed language hints surface the [`Language::parse`] error
//!     verbatim.
//!
//! The routing decision itself (`Keep` / `Switch` / `Reject` /
//! `TranslateWith`) is exhaustively tested in
//! [`super::lang::tests`] — this module just dispatches against the
//! decision, which is mechanical.

use anyhow::Result;
use std::path::PathBuf;

use super::lang::{
    BackendFallback, Language, LidMethod, RoutingDecision,
};
#[cfg(feature = "crispasr")]
use super::lang::route;
use super::{AsrConfig, AsrHandle, AsrSegment};

#[cfg(feature = "crispasr")]
use super::lang::detect_language_from_pcm;

/// LID model bundle passed alongside a non-`AsConfigured` policy.
/// The caller is responsible for resolving + downloading the model
/// file — auto-resolution via CrispASR's registry is a Phase-6
/// follow-up (the current LID model registry entries aren't named
/// uniformly enough yet to auto-pick reliably).
#[derive(Debug, Clone)]
pub struct LidOptions {
    pub method: LidMethod,
    pub model_path: PathBuf,
    /// Threads for the LID inference. 1–4 is plenty; LID over a
    /// 10 s clip is dominated by I/O.
    pub n_threads: i32,
}

/// Outcome of [`transcribe_with_lid_routing`] — everything the caller
/// needs to render "did what, and why".  Cheap to clone (the
/// `RoutingDecision` carries small enums + a String reason, the
/// `AsrConfig` is just two strings).
#[derive(Debug, Clone)]
pub struct TranscribeResult {
    /// The transcribed text.  Empty for silent / VAD-rejected audio.
    pub text: String,
    /// Per-segment timing breakdown.  Empty when not requested (today
    /// the orchestrator always fetches them — CLI subtitle formatters
    /// need them, text-only callers discard cheaply).
    pub segments: Vec<AsrSegment>,
    /// Source language — set when LID ran successfully OR the caller
    /// passed an explicit `--language ISO`.  `None` only on the
    /// `AsConfigured` fast path with no caller hint.
    pub language: Option<Language>,
    /// LID model's posterior probability on the detected language.
    /// `None` whenever LID didn't run (AsConfigured + no hint, or
    /// caller-supplied hint).
    pub confidence: Option<f32>,
    /// Config of the backend that actually did the transcribe.  Equal
    /// to `primary.config()` on `Keep`, the routed config on
    /// `Switch` / `TranslateWith`, never reached on `Reject` (the
    /// function returns `Err`).
    pub used_config: AsrConfig,
    /// The decision the router took.  Surface this in UIs / logs to
    /// explain backend swaps to the user.
    pub decision: RoutingDecision,
}

/// LID window — 10 seconds at 16 kHz mono = 160 000 samples.
/// LID models train on 3–10 s clips; feeding more is wasted compute.
/// Caller's PCM may be shorter (we clamp to whatever's there).
pub const LID_SAMPLE_WINDOW: usize = 160_000;

/// Run LID (if needed), route, transcribe.
///
/// Decision tree:
///
/// 1. `policy == AsConfigured` → skip LID, transcribe with `primary`.
///    Returns `decision = Keep`.
/// 2. `language_hint = Some(iso)` → trust the caller; route on `iso`.
/// 3. `language_hint = None && lid = Some(opts)` → run LID over the
///    first [`LID_SAMPLE_WINDOW`] samples; route on the detected lang.
/// 4. `language_hint = None && lid = None && policy != AsConfigured` →
///    error: routing needs a language, and the caller gave us neither.
///
/// On `Switch(cfg)` / `TranslateWith{cfg, ..}`, the orchestrator
/// constructs a fresh [`AsrHandle`] sharing `primary`'s `cache_dir`
/// so the new backend's model lands in the same place.  The new
/// handle's `Mutex<Option<Asr>>` is per-handle — each call's
/// `Switch` pays the model-load cost.  For batch ingest where the
/// same `Switch` triggers many times, the caller should cache the
/// fallback handle externally (Phase 4's `extractors/audio.rs`'s
/// `OnceLock<AsrHandle>` is the established pattern).
#[cfg(feature = "crispasr")]
pub async fn transcribe_with_lid_routing(
    pcm: Vec<f32>,
    primary: &AsrHandle,
    policy: BackendFallback,
    lid: Option<LidOptions>,
    language_hint: Option<String>,
) -> Result<TranscribeResult> {
    // ── Step 1: AsConfigured fast path ───────────────────────────────
    if matches!(policy, BackendFallback::AsConfigured) {
        let parsed_hint = language_hint
            .as_deref()
            .map(Language::parse)
            .transpose()
            .map_err(|e| anyhow::anyhow!("language hint: {e}"))?;
        let (segments, text) = primary
            .transcribe_full_with_language(pcm, language_hint)
            .await?;
        return Ok(TranscribeResult {
            text,
            segments,
            language: parsed_hint,
            confidence: None,
            used_config: primary.config().clone(),
            decision: RoutingDecision::Keep,
        });
    }

    // ── Step 2: resolve the language for routing ─────────────────────
    let (detected, confidence) = if let Some(hint) = &language_hint {
        // Explicit caller hint — no LID needed.
        let lang = Language::parse(hint)
            .map_err(|e| anyhow::anyhow!("language hint: {e}"))?;
        (lang, None)
    } else {
        // No hint — must run LID.
        let lid_opts = lid.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "policy {:?} needs a language: pass --language <ISO> OR \
                 supply an LID model via --lid-model <PATH>",
                policy_kind(&policy)
            )
        })?;
        let window_end = LID_SAMPLE_WINDOW.min(pcm.len());
        if window_end == 0 {
            anyhow::bail!("PCM is empty — LID needs at least one sample");
        }
        let result = detect_language_from_pcm(
            &pcm[..window_end],
            lid_opts.method,
            &lid_opts.model_path,
            lid_opts.n_threads,
        )?;
        (result.language, Some(result.confidence))
    };

    // ── Step 3: route ────────────────────────────────────────────────
    let decision = route(&policy, primary.config(), &detected);

    // ── Step 4: dispatch on the decision ─────────────────────────────
    match decision.clone() {
        RoutingDecision::Keep => {
            let (segments, text) = primary
                .transcribe_full_with_language(pcm, Some(detected.as_str().to_owned()))
                .await?;
            Ok(TranscribeResult {
                text,
                segments,
                language: Some(detected),
                confidence,
                used_config: primary.config().clone(),
                decision,
            })
        }
        RoutingDecision::Switch(switched_config) => {
            let switched = AsrHandle::new(
                switched_config.clone(),
                primary.cache_dir().to_path_buf(),
            );
            let (segments, text) = switched
                .transcribe_full_with_language(pcm, Some(detected.as_str().to_owned()))
                .await?;
            Ok(TranscribeResult {
                text,
                segments,
                language: Some(detected),
                confidence,
                used_config: switched_config,
                decision,
            })
        }
        RoutingDecision::TranslateWith { config, target } => {
            // Phase 6 transcribes with the routed-to backend; Phase 5
            // will wrap this with a translation post-processing pass
            // that consumes `target` and the produced text.  Today we
            // return the source-language transcript with the decision
            // intact so the caller can see what Phase 5 needs to do.
            let translate_handle = AsrHandle::new(
                config.clone(),
                primary.cache_dir().to_path_buf(),
            );
            let (segments, text) = translate_handle
                .transcribe_full_with_language(pcm, Some(detected.as_str().to_owned()))
                .await?;
            Ok(TranscribeResult {
                text,
                segments,
                language: Some(detected),
                confidence,
                used_config: config,
                decision: RoutingDecision::TranslateWith {
                    config: translate_handle.config().clone(),
                    target,
                },
            })
        }
        RoutingDecision::Reject { reason } => {
            anyhow::bail!("LID routing rejected: {reason}")
        }
    }
}

/// Always-compile stub for builds without the `crispasr` feature.
/// Mirrors the [`super::Asr::transcribe`] stub's pattern.
#[cfg(not(feature = "crispasr"))]
pub async fn transcribe_with_lid_routing(
    _pcm: Vec<f32>,
    _primary: &AsrHandle,
    _policy: BackendFallback,
    _lid: Option<LidOptions>,
    _language_hint: Option<String>,
) -> Result<TranscribeResult> {
    anyhow::bail!(
        "transcribe orchestration requires the `crispasr` cargo feature \
         (build with --features crispasr-metal / -cuda / -vulkan)"
    )
}

/// Short, log-friendly name for the policy variant — `Debug`'s output
/// for `Auto { fallback: AsrConfig { ... } }` is too verbose for an
/// error message.  Used in the "policy X needs a language" hint.
#[cfg(any(feature = "crispasr", test))]
fn policy_kind(p: &BackendFallback) -> &'static str {
    match p {
        BackendFallback::AsConfigured => "as-configured",
        BackendFallback::Strict => "strict",
        BackendFallback::Auto { .. } => "auto",
        BackendFallback::Translate { .. } => "translate",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The orchestrator's transcribe path needs a real CrispASR model,
    // so the tests in this module focus on input validation — the
    // pre-transcribe error paths that fire before any I/O touches
    // a model file.  The routing decision itself (Keep / Switch /
    // Reject / TranslateWith) is covered in detail by
    // `super::lang::tests` and isn't re-tested here.

    fn whisper_handle() -> AsrHandle {
        AsrHandle::new(AsrConfig::default(), std::env::temp_dir())
    }

    // The three input-validation tests below exercise code paths
    // that only exist in the `crispasr` feature build (no-feature
    // builds short-circuit to the actionable --features stub error
    // before any of this validation runs).  Gating them here keeps
    // the always-compile no-feature test surface honest — see the
    // `stub_errors_without_feature` test at the bottom for the
    // no-feature contract.
    #[cfg(feature = "crispasr")]
    #[tokio::test]
    async fn rejects_strict_policy_with_neither_hint_nor_lid_model() {
        // The Strict-policy fast-fail path: caller asked for routing
        // but gave us no way to know what language the audio is in.
        // Error must name BOTH ways out (--language or --lid-model)
        // so the user can pick whichever fits their pipeline.
        let h = whisper_handle();
        let result = transcribe_with_lid_routing(
            vec![0.0; 100],
            &h,
            BackendFallback::Strict,
            None,
            None,
        )
        .await;
        let err = result.expect_err("must error — no hint, no LID model");
        let msg = err.to_string();
        assert!(msg.contains("--language"), "error must mention --language: {msg}");
        assert!(msg.contains("--lid-model"), "error must mention --lid-model: {msg}");
    }

    #[cfg(feature = "crispasr")]
    #[tokio::test]
    async fn rejects_auto_policy_with_neither_hint_nor_lid_model() {
        // Same shape as Strict — Auto also needs a language to decide
        // whether to invoke the fallback.
        let h = whisper_handle();
        let fallback = AsrConfig::new("parakeet");
        let result = transcribe_with_lid_routing(
            vec![0.0; 100],
            &h,
            BackendFallback::Auto { fallback },
            None,
            None,
        )
        .await;
        let err = result.expect_err("must error — no hint, no LID model");
        assert!(err.to_string().contains("auto"), "error must name the policy: {err}");
    }

    #[cfg(feature = "crispasr")]
    #[tokio::test]
    async fn rejects_malformed_language_hint() {
        // ISO 639-1 is strictly two letters; "english" / "" / "1n"
        // must all bounce before we touch the ASR.  Same error
        // surface as `Language::parse` so the user sees the actual
        // offending input.
        let h = whisper_handle();
        for bad in &["english", "", "1n", "EN!"] {
            let result = transcribe_with_lid_routing(
                vec![0.0; 100],
                &h,
                BackendFallback::Strict,
                None,
                Some(bad.to_string()),
            )
            .await;
            let err = result.expect_err(&format!("must reject {bad:?}"));
            assert!(
                err.to_string().contains("language hint")
                    || err.to_string().contains("ISO 639-1"),
                "error must surface the language-parse failure for {bad:?}: {err}"
            );
        }
    }

    #[test]
    fn policy_kind_covers_every_variant() {
        // Drift guard: if `BackendFallback` grows a variant,
        // `policy_kind` must learn it or the error message will read
        // "policy `unknown` needs a language" instead of "policy
        // `<new-name>` needs a language".  Compile-time enforcement
        // would be nicer but `match` over the variants is the next
        // best thing — this test pins the existing four.
        assert_eq!(policy_kind(&BackendFallback::AsConfigured), "as-configured");
        assert_eq!(policy_kind(&BackendFallback::Strict), "strict");
        assert_eq!(
            policy_kind(&BackendFallback::Auto {
                fallback: AsrConfig::default(),
            }),
            "auto"
        );
        assert_eq!(
            policy_kind(&BackendFallback::Translate {
                target: Language::parse("en").unwrap(),
                fallback: AsrConfig::default(),
            }),
            "translate"
        );
    }

    #[tokio::test]
    #[cfg(not(feature = "crispasr"))]
    async fn stub_errors_without_feature() {
        // No-feature build: every call path errors with the actionable
        // --features message, regardless of whether the caller would
        // hit the input-validation branch first.
        let h = whisper_handle();
        let err = transcribe_with_lid_routing(
            vec![0.0; 100],
            &h,
            BackendFallback::AsConfigured,
            None,
            None,
        )
        .await
        .expect_err("stub must error");
        let msg = err.to_string();
        assert!(msg.contains("crispasr"), "must name the feature: {msg}");
        assert!(msg.contains("--features"), "must suggest the flag: {msg}");
    }
}
