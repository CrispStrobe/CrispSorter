//! Audio language identification (LID) + per-backend capability
//! table + routing policy.
//!
//! Three concerns in one module, in roughly increasing layer order:
//!
//! 1. **[`Language`]** — a tiny newtype around an ISO 639-1 code that
//!    normalises casing and rejects malformed input.  Cheap to copy,
//!    safe to `match` against literal `"en"` etc.
//!
//! 2. **[LID](detect_language_from_pcm) wrapper** — thin shim over
//!    `crispasr::detect_language_pcm` (and, for Ecapa/Firered, the
//!    `Session::detect_language` form).  Same shape as our other
//!    crispasr wrappers in [`super`]: error-with-message stub when
//!    the `crispasr` feature isn't on so callers get an actionable
//!    error instead of a silent compile-time disappear.
//!
//! 3. **[Capability table](backend_capabilities)** + **routing
//!    policy** ([`BackendFallback`]).  The README's feature matrix
//!    lists which languages each of the 24 ASR backends supports —
//!    [`RegistryEntry`](crispasr::RegistryEntry) doesn't expose that
//!    today (it's `filename` + `url` + `approx_size`), so we curate
//!    it here.  [`route`] takes a detected language + currently-
//!    configured backend + policy and returns a
//!    [`RoutingDecision`] (keep / switch / translate / reject).
//!    Phase 6 wires this into the actual transcribe pipeline; phases
//!    3–5 already let callers consume the table read-only.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::AsrConfig;

// ─── Language newtype ──────────────────────────────────────────────────

/// ISO 639-1 language code (two lowercase ASCII letters).  Built via
/// [`Language::parse`], which normalises casing and rejects anything
/// that isn't exactly two ASCII letters.  We don't validate against
/// the official ISO 639-1 registry — CrispASR's LID returns whatever
/// the underlying model trained on, and downstream code just needs to
/// pass it back through unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Language(String);

impl Language {
    /// Parse `s` as an ISO 639-1 code.  Trims whitespace, lowercases,
    /// then requires exactly two ASCII letters.  Errors with a clear
    /// message that includes the offending input (so logs and UI
    /// toasts can show what was passed).
    pub fn parse(s: &str) -> Result<Self> {
        let trimmed = s.trim();
        if trimmed.len() != 2 || !trimmed.chars().all(|c| c.is_ascii_alphabetic()) {
            anyhow::bail!(
                "expected ISO 639-1 language code (two ASCII letters), got {:?}",
                s
            );
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    /// The normalised two-letter code (`"en"`, `"de"`, …).
    pub fn code(&self) -> &str {
        &self.0
    }

    /// Same as [`Self::code`] — exposed for ergonomic `as_str()` use
    /// at call sites that already work in `&str` terms.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ─── LID wrapper ──────────────────────────────────────────────────────

/// Which LID model family to run.  Mirrors `crispasr::LidMethod` at
/// the module-level surface (Whisper + Silero) and additionally
/// exposes Firered and Ecapa — those are only reachable through
/// `crispasr::Session::detect_language` today, but we route through
/// the same wrapper here for caller simplicity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LidMethod {
    /// Whisper encoder + language head, 99 languages.  Reuses the
    /// regular `ggml-*.bin` whisper file the ASR side already
    /// downloads — cheapest extra setup when whisper is your ASR.
    Whisper,
    /// Silero classifier, 95 languages, ~16 MB GGUF.  Smallest LID
    /// model and the spec's preferred default for index-time use.
    Silero,
    /// ECAPA-TDNN, 107 (VoxLingua107) or 45 (CommonLanguage)
    /// languages depending on which variant the caller's model file
    /// is.  Recommended by the README for "tightest" detection
    /// thanks to the purpose-built architecture.
    Ecapa,
    /// FireRedLID conformer-encoder + transformer-decoder, 120
    /// languages including Chinese dialects.  Larger (~544 MB Q4_K)
    /// but the best Chinese-aware option.
    Firered,
}

impl LidMethod {
    /// Stable string for logs / config / CLI flag parsing.
    pub fn as_str(self) -> &'static str {
        match self {
            LidMethod::Whisper => "whisper",
            LidMethod::Silero => "silero",
            LidMethod::Ecapa => "ecapa",
            LidMethod::Firered => "firered",
        }
    }

    /// The integer code CrispASR's `Session::detect_language` accepts
    /// (`0` = whisper, `1` = silero, `2` = firered, `3` = ecapa).
    /// Pinned by the wire-format here so behaviour doesn't shift if
    /// upstream renames the enum.
    pub fn as_crispasr_code(self) -> i32 {
        match self {
            LidMethod::Whisper => 0,
            LidMethod::Silero => 1,
            LidMethod::Firered => 2,
            LidMethod::Ecapa => 3,
        }
    }
}

/// One detected language + the model's confidence (0.0–1.0).  Wraps
/// `crispasr::LidResult` with a typed [`Language`] instead of a raw
/// string, and forwards the upstream `-1.0`-on-failure sentinel as
/// `confidence = 0.0` with an empty-code error path — the wrapper
/// never returns a confident-looking result with an unparseable code.
#[derive(Debug, Clone, PartialEq)]
pub struct LidResult {
    pub language: Language,
    pub confidence: f32,
}

/// Run LID over a 16 kHz mono Float32 PCM buffer.  Auto-download /
/// model resolution is the caller's responsibility — pass a concrete
/// path to the model file matching `method`.
///
/// Routes through the right CrispASR surface depending on `method`:
/// Whisper + Silero go via the module-level `detect_language_pcm`
/// (works without a loaded ASR session), Ecapa + Firered go via
/// `Session::detect_language` (the C-ABI exposes these only on the
/// session form today).
///
/// Errors when:
/// - `pcm` is empty (LID needs at least a few hundred ms of audio);
/// - `model_path` doesn't exist;
/// - CrispASR returns an error code or an unparseable language code.
#[cfg(feature = "crispasr")]
pub fn detect_language_from_pcm(
    pcm: &[f32],
    method: LidMethod,
    model_path: &Path,
    n_threads: i32,
) -> Result<LidResult> {
    if pcm.is_empty() {
        anyhow::bail!("LID input PCM is empty");
    }
    if !model_path.exists() {
        anyhow::bail!("LID model not found at {}", model_path.display());
    }
    let model_path_str = model_path.to_string_lossy().into_owned();

    // Stage AC Phase 6 — IN PROGRESS.  The Rust dispatcher below would
    // route all four LID methods (Whisper, Silero, Firered, Ecapa)
    // through the same module-level `crispasr_detect_language_pcm`
    // C-ABI.  The C layer accepts methods 0-3 today, but the upstream
    // Rust binding (`crispasr::LidMethod`) only exposes Whisper +
    // Silero in v0.6.6 / v0.6.7.  Our `2036f0db` upstream patch that
    // adds `Firered = 2` and `Ecapa = 3` lives on `main` but is NOT
    // yet in a tagged release, and CRISPASR_REF in
    // `.github/workflows/release.yml` pins to v0.6.6.  Re-enable
    // the 4-arm dispatcher once CrispASR cuts v0.6.8+ and the
    // pin is bumped.  For now, keep the original split: Whisper +
    // Silero go through the module-level call; Ecapa + Firered bail
    // with an actionable error.
    match method {
        LidMethod::Whisper | LidMethod::Silero => {
            let upstream_method = match method {
                LidMethod::Whisper => crispasr::LidMethod::Whisper,
                LidMethod::Silero => crispasr::LidMethod::Silero,
                _ => unreachable!(),
            };
            let result = crispasr::detect_language_pcm(
                pcm,
                upstream_method,
                &model_path_str,
                n_threads,
                false, // use_gpu — LID is fast enough on CPU
                0,     // gpu_device — ignored when use_gpu = false
                false, // flash_attn — not all backends honour it
            )
            .map_err(|e| anyhow::anyhow!("crispasr detect_language_pcm: {e}"))?;
            return convert_lid(result);
        }
        LidMethod::Ecapa | LidMethod::Firered => {
            anyhow::bail!(
                "LID method {} not yet exposed via the upstream Rust binding \
                 (CrispASR v0.6.6 / v0.6.7 ship only Whisper + Silero variants).  \
                 The C-ABI supports all four; re-enable once CRISPASR_REF is bumped \
                 to v0.6.8+.  For now use whisper or silero.",
                method.as_str()
            )
        }
    }
}

/// Stub for builds without the `crispasr` feature.  Errors with a
/// clear, actionable message rather than silently no-op'ing.
#[cfg(not(feature = "crispasr"))]
pub fn detect_language_from_pcm(
    _pcm: &[f32],
    _method: LidMethod,
    _model_path: &Path,
    _n_threads: i32,
) -> Result<LidResult> {
    anyhow::bail!(
        "audio language ID requires the `crispasr` cargo feature \
         (build with --features crispasr-metal / -cuda / -vulkan)"
    )
}

/// Internal: convert the upstream `LidResult` into our typed form.
/// Treats the `-1.0`-confidence sentinel + empty code as a hard error
/// — callers asking for LID expect a real answer, not "unknown".
#[cfg(feature = "crispasr")]
fn convert_lid(result: crispasr::LidResult) -> Result<LidResult> {
    if result.lang_code.is_empty() || result.confidence < 0.0 {
        anyhow::bail!(
            "LID model returned no result (empty code / negative confidence)"
        );
    }
    let language = Language::parse(&result.lang_code)?;
    Ok(LidResult {
        language,
        confidence: result.confidence,
    })
}

// ─── Backend capability table ─────────────────────────────────────────

/// What languages a backend accepts, as exposed by the curated table
/// below.  Three shapes:
///
/// - [`Known`](Self::Known) — concrete list of ISO 639-1 codes.  The
///   only shape where [`supports_language`] can return a confident
///   `Some(false)`.
/// - [`Many`](Self::Many) — multilingual but not enumerable in this
///   table (e.g. whisper's 99 langs, omniasr's 1600+).  We optimistically
///   answer `Some(true)` for any input language; the [`BackendFallback::Strict`]
///   policy is the conservative one for callers that don't want this.
/// - [`PerModel`](Self::PerModel) — the supported set depends on
///   which model file the user opens (e.g. wav2vec2 has language-
///   specific checkpoints).  Returns `None` from
///   [`supports_language`] — caller must decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendLanguages {
    Known(&'static [&'static str]),
    Many,
    PerModel,
}

/// Translation pathway available to this backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationSupport {
    /// No in-pipeline translation — caller must shell to a separate
    /// text-translation pass after transcription.
    None,
    /// Whisper-style sticky flag: pass `translate = true` to the
    /// session, output is always English regardless of input lang.
    /// Wired via [`crispasr::Session::set_translate`].
    ToEnglish,
    /// Free-form target language via
    /// `crispasr::Session::set_target_language(lang)` — canary,
    /// cohere, voxtral{,4b}.  Caller picks the output language.
    AnyTarget,
}

/// Rough speed tier — used by the picker UI / the indexer default
/// to pick a faster backend than the GUI's `whisper` default.  Not a
/// promise of numeric throughput; consult the README feature matrix
/// for current measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedTier {
    /// Designed for live captioning (≤ 250 MB models, streaming-friendly).
    Realtime,
    /// Substantially faster-than-whisper batch — parakeet, distil-whisper.
    Fast,
    /// Whisper-baseline throughput — the default for GUI push-to-talk.
    Balanced,
    /// Quality-prioritised — large LLM-based backends (granite, gemma4-e2b,
    /// voxtral, omniasr-llm).  Higher latency, often better recall.
    Quality,
}

/// One row in the capability table.  Static — every field is
/// `'static` so the whole table can live in `.rodata` and lookups are
/// pointer comparisons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapabilities {
    /// Languages this backend accepts.  See [`BackendLanguages`].
    pub languages: BackendLanguages,
    /// `true` when the backend's own pipeline does language ID
    /// without an external LID step (whisper, parakeet, qwen3,
    /// glm-asr, gemma4-e2b per the README).
    pub native_lid: bool,
    /// Translation pathway.  See [`TranslationSupport`].
    pub translation: TranslationSupport,
    /// `Session::stream_open` / `feed` / `get_text` is implemented
    /// for this backend at the C-ABI level.  Currently whisper-only
    /// per the CrispASR docstring; this field is wired so the table
    /// can grow without callers having to special-case whisper.
    pub streaming: bool,
    /// Rough speed tier — see [`SpeedTier`].
    pub speed_tier: SpeedTier,
    /// Quoting-the-README human description for the picker UI
    /// ("99 languages, balanced default", "25 EU (auto-detect)").
    /// Kept verbatim so contributors editing the README can audit
    /// drift via grep.
    pub description: &'static str,
}

// ── Static language lists used by the table ───────────────────────────
//
// Curated from the README and the underlying model cards.  Where the
// README is vague ("13", "8") we err on the side of [`BackendLanguages::Many`]
// rather than guess — see the per-entry comments.

// Whisper officially supports 99 langs; rather than enumerate the
// full list we mark it [`BackendLanguages::Many`] (the routing path
// trusts whisper to handle any input — the README backs this up).

/// Parakeet TDT-0.6B-v3 — 25 European languages per the model card.
const PARAKEET_EU_25: &[&str] = &[
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it",
    "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "uk", "ru",
];

/// Granite 4.1 — README: "en fr de es pt ja".
const GRANITE_4_1: &[&str] = &["en", "fr", "de", "es", "pt", "ja"];

/// Granite 4.1-plus / -nar — README: "en fr de es pt" (no ja).
const GRANITE_4_1_REDUCED: &[&str] = &["en", "fr", "de", "es", "pt"];

/// Kyutai STT — README: "en, fr".
const KYUTAI: &[&str] = &["en", "fr"];

/// English-only backends: distil-whisper, fastconformer-ctc,
/// moonshine-streaming.
const ENGLISH_ONLY: &[&str] = &["en"];

/// Japanese-only: parakeet-ja.
const JAPANESE_ONLY: &[&str] = &["ja"];

/// Look up the curated capabilities row for `backend`.  Returns
/// `None` for backends not in the table — callers should treat that
/// as "unknown, pass through unchanged" (the routing layer does).
///
/// Backend names match `crispasr::list_known_models()` exactly,
/// case-sensitive.
pub fn backend_capabilities(backend: &str) -> Option<&'static BackendCapabilities> {
    // Tables.  Constructed once at first call via `OnceLock` would
    // also work, but plain `static`s are simpler and we don't need
    // any runtime initialisation.
    match backend {
        // ── Whisper family ──────────────────────────────────────────
        "whisper" => Some(&WHISPER),
        "distil-whisper" => Some(&DISTIL_WHISPER),

        // ── Parakeet family ─────────────────────────────────────────
        "parakeet" => Some(&PARAKEET),
        "parakeet-ja" => Some(&PARAKEET_JA),

        // ── FastConformer CTC ───────────────────────────────────────
        "fastconformer-ctc" => Some(&FASTCONFORMER_CTC),

        // ── Canary ──────────────────────────────────────────────────
        "canary" => Some(&CANARY),

        // ── Cohere (Aya / Cohere ASR) ───────────────────────────────
        "cohere" => Some(&COHERE),

        // ── Granite family ──────────────────────────────────────────
        "granite" => Some(&GRANITE),
        "granite-4.1" => Some(&GRANITE_41),
        "granite-4.1-plus" => Some(&GRANITE_41_PLUS),
        "granite-4.1-nar" => Some(&GRANITE_41_NAR),

        // ── Voxtral ─────────────────────────────────────────────────
        "voxtral" => Some(&VOXTRAL),
        "voxtral4b" => Some(&VOXTRAL4B),

        // ── Qwen3 ASR ───────────────────────────────────────────────
        "qwen3" => Some(&QWEN3),

        // ── Wav2Vec2 ────────────────────────────────────────────────
        "wav2vec2" => Some(&WAV2VEC2),

        // ── GLM ASR ─────────────────────────────────────────────────
        "glm-asr" => Some(&GLM_ASR),

        // ── Kyutai STT ──────────────────────────────────────────────
        "kyutai-stt" => Some(&KYUTAI_STT),

        // ── FireRed ASR ─────────────────────────────────────────────
        "firered-asr" => Some(&FIRERED_ASR),

        // ── Moonshine ───────────────────────────────────────────────
        "moonshine" => Some(&MOONSHINE),
        "moonshine-streaming" => Some(&MOONSHINE_STREAMING),

        // ── Gemma 4 e2b ─────────────────────────────────────────────
        "gemma4-e2b" => Some(&GEMMA4_E2B),

        // ── OmniASR family ──────────────────────────────────────────
        "omniasr" => Some(&OMNIASR),
        "omniasr-llm" => Some(&OMNIASR_LLM),
        "omniasr-llm-unlimited" => Some(&OMNIASR_LLM_UNLIMITED),

        // ── VibeVoice ───────────────────────────────────────────────
        "vibevoice" => Some(&VIBEVOICE),

        // ── MiMo ASR ────────────────────────────────────────────────
        "mimo-asr" => Some(&MIMO_ASR),

        _ => None,
    }
}

// Static rows — one per known backend, ordered to match the README
// feature matrix.  Keep the comment block above each row so the
// README ↔ table coupling stays visible when someone edits either.

static WHISPER: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Many, // 99 langs, see model card
    native_lid: true,
    translation: TranslationSupport::ToEnglish,
    streaming: true,
    speed_tier: SpeedTier::Balanced,
    description: "99 languages, balanced default (whisper-base ≈ 244 MB)",
};
static DISTIL_WHISPER: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(ENGLISH_ONLY),
    native_lid: false, // distil-whisper is EN-only — no real LID need
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Fast,
    description: "6.3× faster than whisper, English only",
};
static PARAKEET: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(PARAKEET_EU_25),
    native_lid: true,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Fast,
    description: "25 EU languages, FastConformer-TDT (auto-detect)",
};
static PARAKEET_JA: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(JAPANESE_ONLY),
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Fast,
    description: "Japanese-only parakeet",
};
static FASTCONFORMER_CTC: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(ENGLISH_ONLY),
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Fast,
    description: "FastConformer-CTC, English only",
};
static CANARY: BackendCapabilities = BackendCapabilities {
    // README: "25 EU (explicit -sl/-tl)" — same language set as parakeet
    // but with explicit source/target flags (no native LID).
    languages: BackendLanguages::Known(PARAKEET_EU_25),
    native_lid: false,
    translation: TranslationSupport::AnyTarget,
    streaming: false,
    speed_tier: SpeedTier::Balanced,
    description: "25 EU languages with explicit source/target",
};
static COHERE: BackendCapabilities = BackendCapabilities {
    // README: "13" — exact list not enumerated in CrispASR docs.
    // Marked Many until we have a canonical list to cite.
    languages: BackendLanguages::Many,
    native_lid: false,
    translation: TranslationSupport::AnyTarget,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "Cohere ASR, 13 languages",
};
static GRANITE: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(GRANITE_4_1),
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "IBM Granite ASR, en/fr/de/es/pt/ja",
};
static GRANITE_41: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(GRANITE_4_1),
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "Granite 4.1, en/fr/de/es/pt/ja",
};
static GRANITE_41_PLUS: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(GRANITE_4_1_REDUCED),
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "Granite 4.1-plus, en/fr/de/es/pt",
};
static GRANITE_41_NAR: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(GRANITE_4_1_REDUCED),
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "Granite 4.1-nar, en/fr/de/es/pt",
};
static VOXTRAL: BackendCapabilities = BackendCapabilities {
    // README: "8" — exact set not enumerated.
    languages: BackendLanguages::Many,
    native_lid: false,
    translation: TranslationSupport::AnyTarget,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "Voxtral, 8 languages",
};
static VOXTRAL4B: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Many, // 13, realtime streaming
    native_lid: false,
    translation: TranslationSupport::AnyTarget,
    streaming: false, // README: "realtime streaming" — but C-ABI is whisper-only today
    speed_tier: SpeedTier::Realtime,
    description: "Voxtral 4B, 13 languages, designed for realtime",
};
static QWEN3: BackendCapabilities = BackendCapabilities {
    // 30 langs + 22 Chinese dialects — too many to enumerate
    // here, treated as Many.
    languages: BackendLanguages::Many,
    native_lid: true,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "Qwen3 ASR, 30 langs + 22 Chinese dialects",
};
static WAV2VEC2: BackendCapabilities = BackendCapabilities {
    // Each wav2vec2 checkpoint is trained for a specific language —
    // we can't know the supported set without the model file.
    languages: BackendLanguages::PerModel,
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Fast,
    description: "wav2vec2, per-model language",
};
static GLM_ASR: BackendCapabilities = BackendCapabilities {
    // README: "17 (Mandarin, English, Cantonese, ...)" — marked Many
    // until we have the canonical list.
    languages: BackendLanguages::Many,
    native_lid: true,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "GLM ASR, 17 languages (Mandarin/English/Cantonese/…)",
};
static KYUTAI_STT: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(KYUTAI),
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Fast,
    description: "Kyutai STT, en/fr",
};
static FIRERED_ASR: BackendCapabilities = BackendCapabilities {
    // Mandarin + English + 20+ Chinese dialects — best treated as Many
    // since it speaks pinyin/dialect codes that aren't all in ISO 639-1.
    languages: BackendLanguages::Many,
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "FireRed ASR, Mandarin + English + 20+ Chinese dialects",
};
static MOONSHINE: BackendCapabilities = BackendCapabilities {
    // README: "English + 6 langs" — set not enumerated.
    languages: BackendLanguages::Many,
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Realtime,
    description: "Moonshine, English + 6 languages",
};
static MOONSHINE_STREAMING: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Known(ENGLISH_ONLY),
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false, // own streaming runtime, not crispasr's C-ABI stream
    speed_tier: SpeedTier::Realtime,
    description: "Moonshine streaming, English-only",
};
static GEMMA4_E2B: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Many, // 140+ langs
    native_lid: true,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "Gemma 4 e2b, 140+ languages",
};
static OMNIASR: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Many, // 1600+ langs
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Fast,
    description: "OmniASR CTC, 1600+ languages",
};
static OMNIASR_LLM: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Many,
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "OmniASR-LLM, 1600+ languages",
};
static OMNIASR_LLM_UNLIMITED: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Many,
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "OmniASR-LLM (unlimited), 1600+ languages",
};
static VIBEVOICE: BackendCapabilities = BackendCapabilities {
    languages: BackendLanguages::Many, // 50+ langs
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "VibeVoice, 50+ languages",
};
static MIMO_ASR: BackendCapabilities = BackendCapabilities {
    // Mandarin + dialects + English — not enumerable as ISO 639-1.
    languages: BackendLanguages::Many,
    native_lid: false,
    translation: TranslationSupport::None,
    streaming: false,
    speed_tier: SpeedTier::Quality,
    description: "MiMo ASR, Mandarin + dialects + English",
};

/// Does `backend` accept `lang`?
///
/// - `Some(true)` — the curated table says yes (or it's `Many`).
/// - `Some(false)` — the curated table says no (only possible for
///   `Known` rows).
/// - `None` — backend unknown, or `PerModel` shape where the answer
///   depends on the loaded checkpoint.  Caller decides what to do.
pub fn supports_language(backend: &str, lang: &Language) -> Option<bool> {
    let caps = backend_capabilities(backend)?;
    match &caps.languages {
        BackendLanguages::Known(list) => Some(list.contains(&lang.code())),
        // Optimistic — "this backend speaks many; assume yes".
        BackendLanguages::Many => Some(true),
        BackendLanguages::PerModel => None,
    }
}

// ─── Routing policy ───────────────────────────────────────────────────

/// Policy for handling a `(configured backend, detected language)`
/// mismatch at transcribe time.  PLAN.md / Phase 6 wires this into
/// the actual pipeline; Phase 2 just makes the decision pure and
/// testable.
#[derive(Debug, Clone, PartialEq)]
pub enum BackendFallback {
    /// Run the configured backend regardless of detected language.
    /// Status quo — matches today's behaviour, where the user's
    /// explicit `--backend X` choice always wins.
    AsConfigured,
    /// Refuse to transcribe if the configured backend doesn't list
    /// the detected language.  For indexing pipelines that prefer a
    /// hard error to silent garbage output.
    Strict,
    /// On mismatch, switch to `fallback` (typically whisper, the
    /// most general backend).  On a second mismatch — `fallback`
    /// also doesn't support the language — degrades to
    /// [`RoutingDecision::Reject`].
    Auto { fallback: AsrConfig },
    /// On mismatch, route through `fallback` (which must be a
    /// translation-capable backend) with `set_target_language(target)`
    /// (or whisper's `--translate` for English target).  Lets a
    /// fixed-language indexer ("English-only search corpus") consume
    /// foreign-language inputs.
    Translate { target: Language, fallback: AsrConfig },
}

/// What the routing layer decided to do for this transcription.
///
/// `current` is the original [`AsrConfig`], `language` is what LID
/// detected.  Variants:
///
/// - [`Keep`](Self::Keep) — proceed with the current config unchanged.
/// - [`Switch`](Self::Switch) — open a different backend for this call.
/// - [`TranslateWith`](Self::TranslateWith) — open the given backend in
///   translation mode targeting `target`.
/// - [`Reject`](Self::Reject) — refuse to transcribe; `reason` is a
///   user-visible explanation (the GUI / CLI can surface it directly).
#[derive(Debug, Clone, PartialEq)]
pub enum RoutingDecision {
    Keep,
    Switch(AsrConfig),
    TranslateWith {
        config: AsrConfig,
        target: Language,
    },
    Reject {
        reason: String,
    },
}

/// Decide what to do given a policy, the user's configured backend
/// and the detected language.  Pure function — no I/O, no LID call,
/// no model load.  Phase 6 calls this once per transcription with
/// the live LID result.
///
/// Decision matrix:
///
/// | policy             | current supports lang? | decision                    |
/// |--------------------|------------------------|-----------------------------|
/// | `AsConfigured`     | (ignored)              | `Keep`                      |
/// | `Strict`           | yes / unknown          | `Keep`                      |
/// | `Strict`           | no                     | `Reject`                    |
/// | `Auto{fallback}`   | yes / unknown          | `Keep`                      |
/// | `Auto{fallback}`   | no, fallback ok        | `Switch(fallback)`          |
/// | `Auto{fallback}`   | no, fallback also no   | `Reject`                    |
/// | `Translate{t,fb}`  | (always)               | `TranslateWith{fb, t}`      |
///
/// "Unknown" maps to a `Keep` rather than a `Reject` — we don't want
/// the Strict policy to punish backends we haven't catalogued yet
/// (an unknown user-added backend, or a [`BackendLanguages::PerModel`]
/// case where we can't tell without the checkpoint).
pub fn route(
    policy: &BackendFallback,
    current: &AsrConfig,
    detected: &Language,
) -> RoutingDecision {
    match policy {
        BackendFallback::AsConfigured => RoutingDecision::Keep,
        BackendFallback::Strict => match supports_language(&current.backend, detected) {
            // Known yes, or unknown (PerModel / unknown backend) — trust the user.
            Some(true) | None => RoutingDecision::Keep,
            Some(false) => RoutingDecision::Reject {
                reason: format!(
                    "Strict policy: backend `{}` does not support detected \
                     language `{}` ({})",
                    current.backend,
                    detected,
                    describe_languages(&current.backend),
                ),
            },
        },
        BackendFallback::Auto { fallback } => {
            // Order: current → fallback → reject.
            match supports_language(&current.backend, detected) {
                Some(true) | None => RoutingDecision::Keep,
                Some(false) => match supports_language(&fallback.backend, detected) {
                    Some(true) | None => RoutingDecision::Switch(fallback.clone()),
                    Some(false) => RoutingDecision::Reject {
                        reason: format!(
                            "Auto policy: neither `{}` nor fallback `{}` \
                             supports detected language `{}`",
                            current.backend, fallback.backend, detected
                        ),
                    },
                },
            }
        }
        BackendFallback::Translate { target, fallback } => {
            // For Translate we always route through the fallback in
            // translation mode targeting `target` — even when the
            // current backend supports the source language, because
            // the caller's intent is "give me text in `target`".
            // Sanity: the fallback must support the target output
            // language; reject early if it doesn't.
            match supports_language(&fallback.backend, target) {
                Some(true) | None => RoutingDecision::TranslateWith {
                    config: fallback.clone(),
                    target: target.clone(),
                },
                Some(false) => RoutingDecision::Reject {
                    reason: format!(
                        "Translate policy: fallback `{}` does not support \
                         target language `{}`",
                        fallback.backend, target
                    ),
                },
            }
        }
    }
}

/// Human-readable language summary for use in error / log messages —
/// mirrors `BackendCapabilities.description` when we have a row,
/// `(unknown backend)` otherwise.
fn describe_languages(backend: &str) -> String {
    match backend_capabilities(backend) {
        Some(caps) => caps.description.to_string(),
        None => "(unknown backend)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Language parsing ──────────────────────────────────────────────

    #[test]
    fn language_parse_accepts_lowercase() {
        // The canonical form — exactly what CrispASR's LID returns.
        // Bit-exact `code()` round-trip protects callers that key
        // hashmaps / config files on the string.
        let lang = Language::parse("en").unwrap();
        assert_eq!(lang.code(), "en");
        assert_eq!(lang.as_str(), "en");
    }

    #[test]
    fn language_parse_normalises_case_and_whitespace() {
        // Case-fold + trim so the picker UI can pass "EN " / "De"
        // / "JA" without callers having to lowercase first.
        assert_eq!(Language::parse("EN").unwrap().code(), "en");
        assert_eq!(Language::parse(" De ").unwrap().code(), "de");
        assert_eq!(Language::parse("JA").unwrap().code(), "ja");
    }

    #[test]
    fn language_parse_rejects_malformed() {
        // ISO 639-1 is strict two-letter — anything else is a bug
        // upstream (LID returning garbage / user typing freeform).
        for bad in &["e", "eng", "", "1n", "e!", "  "] {
            let err = Language::parse(bad).expect_err(&format!("must reject {bad:?}"));
            assert!(
                err.to_string().contains("ISO 639-1"),
                "error must explain the format: {err}"
            );
        }
    }

    #[test]
    fn language_display_is_code() {
        // Display impl is just the code — consistent with what
        // logs / panic-messages will see when interpolated into "{}".
        assert_eq!(format!("{}", Language::parse("fr").unwrap()), "fr");
    }

    #[test]
    fn language_serde_round_trip() {
        // Transparent newtype: serialises as a bare string ("en")
        // rather than `{"0":"en"}` — matches CrispASR's wire format.
        let lang = Language::parse("ja").unwrap();
        let s = serde_json::to_string(&lang).unwrap();
        assert_eq!(s, "\"ja\"");
        let back: Language = serde_json::from_str(&s).unwrap();
        assert_eq!(back, lang);
    }

    // ── LidMethod codes ───────────────────────────────────────────────

    #[test]
    fn lid_method_crispasr_codes_match_session_api() {
        // These ints are the ABI contract with `Session::detect_language`.
        // Drift here would silently change which model gets loaded.
        // Pinned by test so a future enum rename has to update both
        // sides.
        assert_eq!(LidMethod::Whisper.as_crispasr_code(), 0);
        assert_eq!(LidMethod::Silero.as_crispasr_code(), 1);
        assert_eq!(LidMethod::Firered.as_crispasr_code(), 2);
        assert_eq!(LidMethod::Ecapa.as_crispasr_code(), 3);
    }

    #[test]
    fn lid_method_stable_strings() {
        // Logs / config files / future CLI flags rely on these
        // strings — bake them in.
        assert_eq!(LidMethod::Whisper.as_str(), "whisper");
        assert_eq!(LidMethod::Silero.as_str(), "silero");
        assert_eq!(LidMethod::Ecapa.as_str(), "ecapa");
        assert_eq!(LidMethod::Firered.as_str(), "firered");
    }

    // ── Capability table coverage ─────────────────────────────────────

    #[test]
    fn capabilities_present_for_every_documented_backend() {
        // Every backend in the README's feature matrix must have a
        // table row — otherwise the routing layer can't reason about
        // it.  This list mirrors README.md lines 59–90; keep them in
        // sync.
        let backends = [
            "whisper", "distil-whisper", "parakeet", "parakeet-ja",
            "fastconformer-ctc", "canary", "cohere",
            "granite", "granite-4.1", "granite-4.1-plus", "granite-4.1-nar",
            "voxtral", "voxtral4b",
            "qwen3", "wav2vec2", "glm-asr", "kyutai-stt", "firered-asr",
            "moonshine", "moonshine-streaming",
            "gemma4-e2b", "omniasr", "omniasr-llm", "omniasr-llm-unlimited",
            "vibevoice", "mimo-asr",
        ];
        for b in backends {
            assert!(
                backend_capabilities(b).is_some(),
                "missing capabilities row for `{b}` — add it to lang.rs",
            );
        }
    }

    #[test]
    fn capabilities_none_for_unknown_backend() {
        // Routing relies on `None` to mean "unknown, pass through".
        assert!(backend_capabilities("frobnicator").is_none());
        assert!(backend_capabilities("").is_none());
    }

    // ── supports_language ─────────────────────────────────────────────

    #[test]
    fn whisper_supports_any_language() {
        // Many → optimistic Some(true).  Whisper's 99-lang coverage
        // is the closest we get to a universal backend.
        for code in ["en", "de", "fr", "ja", "zh", "ar"] {
            let l = Language::parse(code).unwrap();
            assert_eq!(supports_language("whisper", &l), Some(true), "{code}");
        }
    }

    #[test]
    fn distil_whisper_is_english_only() {
        // Known → strict membership.  This is the test that catches
        // the most common routing failure: someone picks distil-
        // whisper for speed without realising it can't speak their
        // language.
        let en = Language::parse("en").unwrap();
        let de = Language::parse("de").unwrap();
        assert_eq!(supports_language("distil-whisper", &en), Some(true));
        assert_eq!(supports_language("distil-whisper", &de), Some(false));
    }

    #[test]
    fn parakeet_eu_25_includes_expected_codes() {
        // Spot-check the EU 25 list — the routing layer's correctness
        // depends on it being faithful to the model card.
        for code in ["en", "de", "fr", "it", "es", "uk"] {
            let l = Language::parse(code).unwrap();
            assert_eq!(supports_language("parakeet", &l), Some(true), "{code}");
        }
        // Japanese isn't in the EU 25 — parakeet-ja is the right pick.
        let ja = Language::parse("ja").unwrap();
        assert_eq!(supports_language("parakeet", &ja), Some(false));
        assert_eq!(supports_language("parakeet-ja", &ja), Some(true));
        assert_eq!(supports_language("parakeet-ja", &Language::parse("en").unwrap()), Some(false));
    }

    #[test]
    fn wav2vec2_returns_unknown() {
        // PerModel → None — wav2vec2 needs the loaded checkpoint to
        // answer, so we punt to the caller.
        let en = Language::parse("en").unwrap();
        assert_eq!(supports_language("wav2vec2", &en), None);
    }

    #[test]
    fn unknown_backend_returns_unknown() {
        // None (backend missing from table) — the routing layer
        // treats this the same as `PerModel`: trust the caller, don't
        // reject.
        let en = Language::parse("en").unwrap();
        assert_eq!(supports_language("frobnicator", &en), None);
    }

    #[test]
    fn native_lid_flags_match_readme() {
        // README "Native LID" row: whisper, parakeet, qwen3, glm-asr,
        // gemma4-e2b.  This test pins those backends as native_lid =
        // true and everything else as false — drift in either
        // direction is a bug worth catching.
        let native = ["whisper", "parakeet", "qwen3", "glm-asr", "gemma4-e2b"];
        for b in native {
            let caps = backend_capabilities(b).unwrap();
            assert!(caps.native_lid, "expected native LID for {b}");
        }
        let external = [
            "distil-whisper", "canary", "cohere", "voxtral",
            "fastconformer-ctc", "wav2vec2", "kyutai-stt", "firered-asr",
            "moonshine", "moonshine-streaming", "omniasr", "omniasr-llm",
            "vibevoice", "mimo-asr",
        ];
        for b in external {
            let caps = backend_capabilities(b).unwrap();
            assert!(!caps.native_lid, "expected external LID for {b}");
        }
    }

    #[test]
    fn translation_pathways_match_readme() {
        // Whisper: sticky translate-to-English.
        let w = backend_capabilities("whisper").unwrap();
        assert_eq!(w.translation, TranslationSupport::ToEnglish);
        // Canary + cohere + voxtral: free-form target language.
        for b in ["canary", "cohere", "voxtral", "voxtral4b"] {
            let caps = backend_capabilities(b).unwrap();
            assert_eq!(caps.translation, TranslationSupport::AnyTarget, "{b}");
        }
        // Distil-whisper / parakeet-ja: no translation pathway.
        for b in ["distil-whisper", "parakeet-ja", "fastconformer-ctc"] {
            let caps = backend_capabilities(b).unwrap();
            assert_eq!(caps.translation, TranslationSupport::None, "{b}");
        }
    }

    // ── Routing decisions ─────────────────────────────────────────────

    fn whisper_cfg() -> AsrConfig {
        AsrConfig::new("whisper")
    }
    fn parakeet_cfg() -> AsrConfig {
        AsrConfig::new("parakeet")
    }
    fn distil_cfg() -> AsrConfig {
        AsrConfig::new("distil-whisper")
    }

    #[test]
    fn routing_as_configured_always_keeps() {
        // AsConfigured ignores LID entirely — explicit user choice wins.
        let de = Language::parse("de").unwrap();
        let policy = BackendFallback::AsConfigured;

        // Even on a clear mismatch (distil-whisper + German), Keep.
        assert_eq!(route(&policy, &distil_cfg(), &de), RoutingDecision::Keep);
    }

    #[test]
    fn routing_strict_keeps_when_supported() {
        // Whisper handles German → Keep.
        let de = Language::parse("de").unwrap();
        assert_eq!(
            route(&BackendFallback::Strict, &whisper_cfg(), &de),
            RoutingDecision::Keep
        );
    }

    #[test]
    fn routing_strict_rejects_when_unsupported() {
        // distil-whisper is EN-only — Strict + German → Reject.
        let de = Language::parse("de").unwrap();
        let decision = route(&BackendFallback::Strict, &distil_cfg(), &de);
        match decision {
            RoutingDecision::Reject { reason } => {
                // The reason must name both the backend and the language
                // so the user can act on it (pick a different backend,
                // override the detected lang, etc.).
                assert!(reason.contains("distil-whisper"), "reason: {reason}");
                assert!(reason.contains("de"), "reason: {reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn routing_strict_keeps_for_unknown_backend() {
        // Unknown backend (None from supports_language) → Keep, not
        // Reject.  Documented in the route() doc-comment.
        let de = Language::parse("de").unwrap();
        let cfg = AsrConfig::new("frobnicator");
        assert_eq!(
            route(&BackendFallback::Strict, &cfg, &de),
            RoutingDecision::Keep
        );
    }

    #[test]
    fn routing_auto_keeps_when_current_supports() {
        // Auto with whisper fallback, current = parakeet, lang = de.
        // Parakeet supports de → Keep (no need to invoke fallback).
        let de = Language::parse("de").unwrap();
        let policy = BackendFallback::Auto { fallback: whisper_cfg() };
        assert_eq!(route(&policy, &parakeet_cfg(), &de), RoutingDecision::Keep);
    }

    #[test]
    fn routing_auto_switches_on_mismatch_with_capable_fallback() {
        // Auto, current = parakeet (no Japanese), fallback = whisper
        // (universal), lang = ja → Switch(whisper).
        let ja = Language::parse("ja").unwrap();
        let policy = BackendFallback::Auto { fallback: whisper_cfg() };
        assert_eq!(
            route(&policy, &parakeet_cfg(), &ja),
            RoutingDecision::Switch(whisper_cfg())
        );
    }

    #[test]
    fn routing_auto_rejects_when_both_unsupported() {
        // Auto, current = parakeet (no ja), fallback = distil-whisper
        // (en-only) — neither speaks Japanese → Reject.
        let ja = Language::parse("ja").unwrap();
        let policy = BackendFallback::Auto { fallback: distil_cfg() };
        let decision = route(&policy, &parakeet_cfg(), &ja);
        match decision {
            RoutingDecision::Reject { reason } => {
                assert!(reason.contains("parakeet"), "reason: {reason}");
                assert!(reason.contains("distil-whisper"), "reason: {reason}");
                assert!(reason.contains("ja"), "reason: {reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn routing_translate_routes_through_fallback() {
        // Translate{target=en, fallback=whisper}, current=parakeet,
        // detected=ja.  Even though parakeet (or whisper) could
        // transcribe the source, Translate routes through the
        // fallback in translation mode targeting English.
        let en = Language::parse("en").unwrap();
        let ja = Language::parse("ja").unwrap();
        let policy = BackendFallback::Translate {
            target: en.clone(),
            fallback: whisper_cfg(),
        };
        assert_eq!(
            route(&policy, &parakeet_cfg(), &ja),
            RoutingDecision::TranslateWith {
                config: whisper_cfg(),
                target: en,
            }
        );
    }

    #[test]
    fn routing_translate_rejects_when_fallback_lacks_target() {
        // Translate{target=ja, fallback=distil-whisper} — distil
        // is EN-only and can't produce Japanese output → Reject.
        let ja = Language::parse("ja").unwrap();
        let policy = BackendFallback::Translate {
            target: ja.clone(),
            fallback: distil_cfg(),
        };
        let decision = route(&policy, &whisper_cfg(), &Language::parse("de").unwrap());
        match decision {
            RoutingDecision::Reject { reason } => {
                assert!(reason.contains("distil-whisper"), "reason: {reason}");
                assert!(reason.contains("ja"), "reason: {reason}");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    // ── Feature-gated stub ────────────────────────────────────────────

    #[test]
    #[cfg(not(feature = "crispasr"))]
    fn lid_stub_errors_without_feature() {
        // Same shape as the audio module's stub test: action error,
        // mentions the feature flag the user needs.
        let err = detect_language_from_pcm(
            &[0.0; 1600],
            LidMethod::Silero,
            Path::new("/nowhere"),
            1,
        )
        .expect_err("stub must error");
        let msg = err.to_string();
        assert!(msg.contains("crispasr"), "{msg}");
        assert!(msg.contains("--features"), "{msg}");
    }
}
