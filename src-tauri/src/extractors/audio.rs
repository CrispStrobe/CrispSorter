//! Audio / video index-time extractor (P13.5 slice B).
//!
//! Wraps the shared [`crate::audio`] decode pipeline (symphonia tier-1
//! + ffmpeg fallback) and the [`crate::asr::AsrHandle`] transcription
//! surface to turn an audio file into an [`ExtractedDocument`]
//! suitable for [`crate::index::ingest::IngestPipeline`].
//!
//! ## Why a single process-level [`OnceLock<AsrHandle>`]?
//!
//! Loading a CrispASR session takes 0.5–5 s (model file open, GGML
//! init, GPU allocations).  Recreating that per-file during batch
//! ingest of a music / podcast folder would dominate the wall-clock
//! time.  An [`AsrHandle`] is already cheap-clonable + has its own
//! `Mutex<Option<Asr>>` that serialises concurrent transcribe calls,
//! so one handle for the lifetime of the process is exactly the
//! shape we want.
//!
//! Trade-off documented: the backend is fixed at the first call
//! (default `whisper`).  Phase 6 will swap to a per-document policy
//! routed by [`crate::asr::lang::route`] using the LID + capability
//! table, at which point this module gets a per-call factory instead
//! of a singleton.
//!
//! ## Feature gating
//!
//! Mirrors `extractors/ocr_paddle.rs`: the module always compiles,
//! [`is_audio_extraction_available`] is the cheap probe, and
//! [`extract`] returns a stub error without `--features crispasr-*`.
//! Pipeline code calls the probe first so the bg_ingest classifier
//! can downgrade to "L2 metadata only" gracefully.

use anyhow::Result;
#[cfg(feature = "crispasr")]
use anyhow::Context;
use std::path::Path;
#[cfg(feature = "crispasr")]
use std::sync::OnceLock;

use super::ExtractedDocument;

/// File extensions this extractor accepts — keep in sync with
/// [`crate::audio::supported_extension`].  The pure audio set,
/// the video-container set (we demux just the audio stream), and
/// the ffmpeg-only long-tail set are all included here; the
/// downstream decoder picks the right tier at runtime.
pub const AUDIO_EXTS: &[&str] = &[
    // Pure audio (symphonia tier-1)
    "wav", "mp3", "m4a", "flac", "ogg", "opus", "aac",
    "alac", "caf", "aiff",
    // Video containers — we demux audio stream only (symphonia tier-1)
    "mp4", "mov", "mkv", "webm", "m4v",
    // Long-tail (tier-2 ffmpeg shell-out)
    "avi", "wmv", "flv", "ts", "amr", "ra", "3gp", "asf",
];

/// `true` when the `crispasr` cargo feature is compiled in.
/// Pipeline code probes this BEFORE the dispatch arm so it can
/// fall through to L2-metadata ingest with a clear failure reason
/// rather than letting [`extract`] return its actionable-error
/// stub deep inside the ingest loop.
pub fn is_audio_extraction_available() -> bool {
    cfg!(feature = "crispasr")
}

/// Real implementation — only compiled with `--features crispasr*`.
#[cfg(feature = "crispasr")]
pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    // ── 0. L2 metadata probe (cheap, no decode) ────────────────────
    // Format-reader-only pass: duration / codec / sample rate /
    // channels / bitrate.  Best-effort: failure logs but doesn't
    // error the extraction, because the transcript is what callers
    // actually wait on.
    let audio_meta = crate::audio::probe::probe_metadata(path).ok();
    // ── 1. Decode to 16 kHz mono Float32 ───────────────────────────
    // The default `AllowFfmpeg` policy lets .avi / .wmv / .flv etc.
    // through; the bg_ingest classifier doesn't pre-check the
    // extension against AUDIO_EXTS so anything that lands here got
    // there through the extractor dispatcher's whitelist already.
    let decoded = crate::audio::decode_to_16khz_mono(
        path,
        crate::audio::FallbackPolicy::AllowFfmpeg,
    )
    .with_context(|| format!("audio decode for {}", path.display()))?;

    // ── 2. Transcribe via the shared process-level handle ──────────
    //
    // First call to `get_or_init` constructs the handle (cheap — no
    // model load yet); the actual session load happens inside
    // `AsrHandle::transcribe_with_language` the first time it sees a
    // non-empty PCM buffer.  Subsequent calls reuse the same loaded
    // session, paid for once across the whole batch.
    let handle = shared_asr_handle();

    // Clone the decoded PCM before transcription consumes it — bg_ingest
    // needs the raw samples for omni cross-modal embedding.
    let pcm_for_omni = decoded.pcm.clone();

    // Build a current-thread runtime to bridge the sync extractor
    // boundary into the async `AsrHandle::transcribe_with_language`.
    // bg_ingest already calls extractors via `tokio::task::spawn_blocking`
    // so we're on a dedicated blocking thread — building a nested
    // current-thread runtime here is the standard pattern (same
    // shape the CLI's `cmd_chat_transcribe` uses).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("constructing tokio runtime for audio extractor")?;
    let transcript = rt
        .block_on(handle.transcribe_with_language(decoded.pcm, None))
        .with_context(|| format!("ASR transcribe for {}", path.display()))?;

    // ── 3. Pack into ExtractedDocument ─────────────────────────────
    //
    // `ext` is left empty — the dispatcher fills it (line 188 in
    // `extractors/mod.rs` etc.).  No headings extraction: ASR
    // transcripts are typically a single stream of text; a future
    // pass could lift speaker labels or chapter markers (for
    // podcasts with embedded timestamps) into `headings`.
    Ok(ExtractedDocument {
        full_text: transcript,
        headings: Vec::new(),
        ext: String::new(),
        language: None,
        translated_text: None,
        translated_to_lang: None,
        audio: audio_meta,
        image_exif: None,
        source_url: None,
        tags: vec![],
        audio_pcm: Some(pcm_for_omni),
    })
}

/// Feature-off stub.  Errors with a clear --features hint so the
/// bg_ingest classifier can surface "you need to rebuild with
/// crispasr enabled" rather than the generic "extraction failed".
#[cfg(not(feature = "crispasr"))]
pub fn extract(_path: &Path) -> Result<ExtractedDocument> {
    anyhow::bail!(
        "audio extraction requires the `crispasr` cargo feature \
         (build with --features crispasr-metal / -cuda / -vulkan)"
    )
}

/// Process-level shared `AsrHandle`.  Construction is cheap (no
/// model load); the actual session load is deferred to the first
/// `transcribe*` call.  Backend reads from
/// [`AUDIO_ASR_BACKEND_OVERRIDE`] on first init (set by `bg_ingest`
/// at startup from `IndexConfig.audio_asr_backend`); falls back to
/// `AsrConfig::default()` (whisper, 99 langs) when the override
/// hasn't been set yet (CLI / GUI push-to-talk paths that pre-date
/// the IndexConfig-driven configuration).
///
/// Restart-on-change semantics: once the OnceLock fires, swapping
/// IndexConfig.audio_asr_backend has no effect until the next
/// process start.  Same constraint the existing embedder has.
#[cfg(feature = "crispasr")]
fn shared_asr_handle() -> &'static crate::asr::AsrHandle {
    static HANDLE: OnceLock<crate::asr::AsrHandle> = OnceLock::new();
    HANDLE.get_or_init(|| {
        let backend = AUDIO_ASR_BACKEND_OVERRIDE
            .get()
            .cloned()
            .unwrap_or_else(|| "whisper".to_string());
        let config = crate::asr::AsrConfig::new(&backend);
        let cache_dir = default_asr_cache_dir();
        crate::asr::AsrHandle::new(config, cache_dir)
    })
}

/// Process-global ASR backend override, set at startup by bg_ingest
/// from `IndexConfig.audio_asr_backend`.  Read by
/// [`shared_asr_handle`] when constructing the lazy handle.  Mutex
/// + Option pattern rather than `OnceLock<String>` so the bg_ingest
/// startup can defensively set it before the OnceLock has fired —
/// the cleaner "write once and forget" semantics belong to the
/// HANDLE inside shared_asr_handle, which OnceLock locks for us.
#[cfg(feature = "crispasr")]
pub(crate) static AUDIO_ASR_BACKEND_OVERRIDE: OnceLock<String> = OnceLock::new();

/// Called from bg_ingest at startup to apply
/// `IndexConfig.audio_asr_backend`.  Set-once: subsequent calls are
/// ignored (matches the `OnceLock` guarantee).  Returns `true` when
/// the call actually set the value, `false` when the slot was
/// already filled.  bg_ingest doesn't currently surface that to the
/// user; the boolean is for tests that want to assert the slot got
/// touched.
#[cfg(feature = "crispasr")]
pub fn set_audio_asr_backend_override(backend: &str) -> bool {
    AUDIO_ASR_BACKEND_OVERRIDE.set(backend.to_string()).is_ok()
}

/// Feature-off stub.  No-op so call sites in bg_ingest compile
/// unconditionally.
#[cfg(not(feature = "crispasr"))]
pub fn set_audio_asr_backend_override(_backend: &str) -> bool {
    false
}

/// Mirror the GUI / CLI app-data-dir resolution so the same
/// downloaded model files are reused across surfaces.  We can't
/// reach the Tauri `app_data_dir()` from here (this code path runs
/// inside `tokio::task::spawn_blocking` without the `AppHandle`),
/// so we replicate the per-OS path manually — same algorithm
/// `cli::resolve_data_dir` uses.
#[cfg(feature = "crispasr")]
fn default_asr_cache_dir() -> std::path::PathBuf {
    use std::path::PathBuf;

    let base = {
        #[cfg(target_os = "macos")]
        {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join("Library/Application Support/com.crispstrobe.crispsorter"))
                .unwrap_or_else(|| PathBuf::from("/tmp/crispsorter"))
        }
        #[cfg(target_os = "windows")]
        {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .map(|a| a.join("com.crispstrobe.crispsorter"))
                .unwrap_or_else(|| PathBuf::from("C:\\Temp\\crispsorter"))
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .map(|h| h.join(".local/share"))
                })
                .map(|d| d.join("com.crispstrobe.crispsorter"))
                .unwrap_or_else(|| PathBuf::from("/tmp/crispsorter"))
        }
    };
    let dir = base.join("models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_exts_covers_canonical_audio_set() {
        // Tier-1 audio formats — these are the ones every user has;
        // missing one would silently route to the "no extractor"
        // dispatch arm and end up as L2-metadata-only.
        for ext in ["wav", "mp3", "m4a", "flac", "ogg", "opus", "aac"] {
            assert!(
                AUDIO_EXTS.contains(&ext),
                "AUDIO_EXTS missing canonical audio format: {ext}",
            );
        }
    }

    #[test]
    fn audio_exts_covers_video_containers() {
        // We demux the audio stream from these — no video decode.
        // PLAN.md scope axis 2 lists these explicitly as in-scope.
        for ext in ["mp4", "mov", "mkv", "webm", "m4v"] {
            assert!(
                AUDIO_EXTS.contains(&ext),
                "AUDIO_EXTS missing video container: {ext}",
            );
        }
    }

    #[test]
    fn audio_exts_covers_long_tail_for_ffmpeg_fallback() {
        // .avi / .wmv etc. need the tier-2 ffmpeg shell-out — but
        // they're still extensions WE accept; we just route through
        // a different decoder tier.  Anything missing here gets
        // rejected at the dispatcher level before audio.rs even sees it.
        for ext in ["avi", "wmv", "flv", "ts", "amr"] {
            assert!(
                AUDIO_EXTS.contains(&ext),
                "AUDIO_EXTS missing tier-2 format: {ext}",
            );
        }
    }

    #[test]
    fn audio_exts_matches_audio_module_supported_extensions() {
        // The audio module's `supported_extension` router is the
        // source of truth for "can the decoder actually handle this?".
        // AUDIO_EXTS must be a subset (every extension we accept
        // here, the decoder also accepts).  Drift would mean the
        // dispatcher dispatches to us for a file we can't decode.
        use crate::audio::{supported_extension, ExtensionSupport};
        for ext in AUDIO_EXTS {
            assert!(
                matches!(
                    supported_extension(ext),
                    ExtensionSupport::Symphonia | ExtensionSupport::Ffmpeg
                ),
                "AUDIO_EXTS contains {ext:?} which the audio module \
                 reports as Unknown — drift between extractors/audio.rs \
                 and audio/mod.rs"
            );
        }
    }

    #[test]
    fn availability_probe_matches_feature_flag() {
        // The probe is what bg_ingest calls before dispatching, so
        // its boolean MUST track the actual `extract` behaviour.
        // If `extract` returns a feature-off error string, the probe
        // must return false (and vice versa).  This pins the contract.
        let probe = is_audio_extraction_available();
        let actual_runs = cfg!(feature = "crispasr");
        assert_eq!(
            probe, actual_runs,
            "probe disagrees with cfg!(feature = \"crispasr\")"
        );
    }

    #[test]
    fn audio_pcm_field_surfaces_on_extracted_document() {
        // Verify that the audio_pcm field exists on ExtractedDocument
        // and round-trips through construction.
        let doc = ExtractedDocument {
            audio_pcm: Some(vec![0.1, -0.2, 0.3]),
            ..Default::default()
        };
        let pcm = doc.audio_pcm.as_ref().unwrap();
        assert_eq!(pcm.len(), 3);
        assert!((pcm[0] - 0.1).abs() < f32::EPSILON);

        // Non-audio docs have None.
        let doc2 = ExtractedDocument::default();
        assert!(doc2.audio_pcm.is_none());
    }

    #[test]
    #[cfg(not(feature = "crispasr"))]
    fn extract_without_feature_errors_with_actionable_message() {
        // Same shape as the audio module's stub test — name the
        // feature flag the user needs.
        let err = extract(Path::new("/tmp/whatever.wav"))
            .expect_err("stub must error");
        let msg = err.to_string();
        assert!(msg.contains("crispasr"), "must name the feature: {msg}");
        assert!(msg.contains("--features"), "must suggest the build flag: {msg}");
    }
}
