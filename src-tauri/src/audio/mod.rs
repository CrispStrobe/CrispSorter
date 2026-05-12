//! Cross-platform audio + video decoding to 16 kHz mono Float32 PCM —
//! the canonical input format for CrispASR.
//!
//! ## Two decode tiers
//!
//! 1. **Pure-Rust via [symphonia](https://github.com/pdeljanov/Symphonia)
//!    + [rubato](https://github.com/HEnquist/rubato):**
//!    - audio: WAV / MP3 / M4A / FLAC / OGG / OPUS / AAC
//!    - video: MP4 / MOV / MKV / WebM / M4V (audio stream demuxed,
//!      video frames skipped)
//!    No system deps — ships everywhere CrispSorter does.
//!
//! 2. **ffmpeg shell-out fallback** (`ffmpeg_fallback`) for the long
//!    tail of containers symphonia doesn't read: AVI DivX, WMV, FLV,
//!    TS, AMR, RA.  Only fires when symphonia rejects the file AND
//!    `ffmpeg` is on PATH; otherwise emits a clear "install ffmpeg
//!    for .<ext>" message rather than silent failure.  Cross-platform:
//!    `which ffmpeg` works on macOS / Linux, `where.exe ffmpeg` on
//!    Windows.
//!
//! ## Shape of returned data
//!
//! Always 16 kHz, mono, Float32 PCM in `[-1.0, 1.0]`.  Suitable as
//! direct input to `crispasr::Session::transcribe_with_language` or
//! `crispasr::Session::stream_open` + `feed`.

use anyhow::Result;
use std::path::Path;

#[cfg(feature = "crispasr")]
pub mod decoder;
#[cfg(feature = "crispasr")]
pub mod ffmpeg_fallback;
#[cfg(feature = "crispasr")]
pub mod resampler;
// `writer` is always-compile: the WAV write path is useful outside
// of the symphonia decode pipeline (e.g. `chat tts` synthesises via
// CrispASR which lives behind the feature, but the resulting Vec<f32>
// → WAV write doesn't depend on crispasr at all).  hound is non-
// optional in Cargo.toml to keep this honest.
pub mod writer;

/// Canonical input sample rate for CrispASR (and effectively every
/// modern ASR model — whisper, parakeet, canary, qwen3, granite, …).
pub const ASR_SAMPLE_RATE: u32 = 16_000;

/// Canonical input channel count for CrispASR (mono).
pub const ASR_CHANNELS: u16 = 1;

/// Result of a successful decode pass.  `pcm` is ready to feed
/// directly into `crispasr::Session::transcribe*` — already 16 kHz,
/// mono, Float32.
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    /// 16 kHz mono Float32 PCM in `[-1.0, 1.0]`.
    pub pcm: Vec<f32>,
    /// Source sample rate before resampling (informational — useful
    /// for logging "transcoded 48000 Hz → 16000 Hz").
    pub source_sample_rate: u32,
    /// Source channel count before downmix (1 = already mono, 2 =
    /// stereo, 6 = 5.1, etc.).  All non-mono inputs are summed +
    /// scaled to mono.
    pub source_channels: u16,
    /// Duration in seconds.  Equivalent to `pcm.len() as f64 /
    /// ASR_SAMPLE_RATE as f64`, exposed here for convenience.
    pub duration_seconds: f64,
    /// Which tier handled the decode — `"symphonia"` or `"ffmpeg"`.
    /// Lets callers tag log lines / emit warnings about shell-out
    /// usage on shipping installs.
    pub tier: DecodeTier,
}

/// Which decode tier produced [`DecodedAudio`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeTier {
    /// Pure-Rust path (symphonia + rubato).
    Symphonia,
    /// ffmpeg shell-out fallback.
    Ffmpeg,
}

impl DecodeTier {
    pub fn as_str(self) -> &'static str {
        match self {
            DecodeTier::Symphonia => "symphonia",
            DecodeTier::Ffmpeg => "ffmpeg",
        }
    }
}

/// Whether to allow the ffmpeg shell-out fallback when symphonia
/// can't read a file.  Default: [`FallbackPolicy::AllowFfmpeg`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FallbackPolicy {
    /// Try symphonia first; on failure, try `ffmpeg` if it's on PATH.
    /// Errors with a clear "install ffmpeg for .<ext>" message when
    /// symphonia fails AND ffmpeg isn't found.
    #[default]
    AllowFfmpeg,
    /// Only use symphonia.  Errors if the file isn't decodable
    /// purely in-process.  Useful for environments where shell-out
    /// is forbidden (sandboxes, embedded use).
    PureRust,
}

/// Decode `path` to 16 kHz mono Float32 PCM.  Tries symphonia first,
/// then optionally falls back to ffmpeg per `policy`.
///
/// **Errors** when:
/// - the file doesn't exist or isn't readable;
/// - symphonia rejects the container AND `policy ==
///   PureRust`;
/// - symphonia rejects the container AND `ffmpeg` isn't on PATH
///   (under `AllowFfmpeg`);
/// - the audio stream is silent (zero samples).
#[cfg(feature = "crispasr")]
pub fn decode_to_16khz_mono(path: &Path, policy: FallbackPolicy) -> Result<DecodedAudio> {
    // Tier 1: symphonia.
    match decoder::decode_with_symphonia(path) {
        Ok(d) => Ok(d),
        Err(symphonia_err) => match policy {
            FallbackPolicy::PureRust => Err(anyhow::anyhow!(
                "symphonia decode failed for {} (PureRust policy — \
                 ffmpeg fallback not allowed): {symphonia_err}",
                path.display()
            )),
            FallbackPolicy::AllowFfmpeg => {
                // Tier 2: ffmpeg.  Propagates "ffmpeg not found"
                // with the original symphonia error attached so the
                // caller can see both failure modes.
                ffmpeg_fallback::decode_with_ffmpeg(path).map_err(|ffmpeg_err| {
                    anyhow::anyhow!(
                        "audio decode failed for {}\n  \
                         tier-1 symphonia: {symphonia_err}\n  \
                         tier-2 ffmpeg: {ffmpeg_err}",
                        path.display()
                    )
                })
            }
        },
    }
}

/// Stub for builds without the `crispasr` feature.  Errors with a
/// clear "build with --features crispasr-*" message.
#[cfg(not(feature = "crispasr"))]
pub fn decode_to_16khz_mono(_path: &Path, _policy: FallbackPolicy) -> Result<DecodedAudio> {
    anyhow::bail!(
        "audio decode requires the `crispasr` cargo feature \
         (build with --features crispasr-metal / -cuda / -vulkan)"
    );
}

/// Best-effort container family lookup for a path.  Used by `doctor`
/// to report what's supported on this build without trying a decode.
/// Always-compile — independent of the `crispasr` feature.
pub fn supported_extension(ext: &str) -> ExtensionSupport {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    match ext.as_str() {
        // Tier 1 — symphonia handles natively, audio side.
        "wav" | "mp3" | "m4a" | "flac" | "ogg" | "opus" | "aac" | "alac" | "caf" | "aiff" => {
            ExtensionSupport::Symphonia
        }
        // Tier 1 — symphonia handles natively, video side (audio
        // stream demux only — video frames skipped).
        "mp4" | "mov" | "mkv" | "webm" | "m4v" => ExtensionSupport::Symphonia,
        // Tier 2 — ffmpeg fallback.
        "avi" | "wmv" | "flv" | "ts" | "amr" | "ra" | "3gp" | "asf" => ExtensionSupport::Ffmpeg,
        // Unknown — neither tier will likely succeed.
        _ => ExtensionSupport::Unknown,
    }
}

/// What [`supported_extension`] reports for a given extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionSupport {
    /// Pure-Rust symphonia tier handles it.
    Symphonia,
    /// Tier-2 ffmpeg shell-out required.
    Ffmpeg,
    /// Neither tier expected to succeed — likely not audio/video.
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extension_canonical_audio() {
        // The audio-only formats symphonia ships with — these must
        // always be tier-1 regardless of feature flags.
        for ext in ["wav", "mp3", "m4a", "flac", "ogg", "opus", "aac"] {
            assert_eq!(supported_extension(ext), ExtensionSupport::Symphonia, "{ext}");
            // Case-insensitive + leading-dot tolerance — callers
            // sometimes pass `.MP3` or `MP3` interchangeably.
            assert_eq!(supported_extension(&ext.to_uppercase()), ExtensionSupport::Symphonia);
            assert_eq!(supported_extension(&format!(".{ext}")), ExtensionSupport::Symphonia);
        }
    }

    #[test]
    fn supported_extension_video_containers_are_symphonia() {
        // We demux the audio stream from video containers; no video
        // decode happens.  All these should be tier-1.
        for ext in ["mp4", "mov", "mkv", "webm", "m4v"] {
            assert_eq!(supported_extension(ext), ExtensionSupport::Symphonia, "{ext}");
        }
    }

    #[test]
    fn supported_extension_long_tail_falls_to_ffmpeg() {
        // Containers symphonia doesn't support — we route to ffmpeg
        // shell-out as a last resort.  This list is curated, not
        // exhaustive; an unknown extension below would hit Unknown.
        for ext in ["avi", "wmv", "flv", "ts", "amr", "ra", "3gp", "asf"] {
            assert_eq!(supported_extension(ext), ExtensionSupport::Ffmpeg, "{ext}");
        }
    }

    #[test]
    fn supported_extension_unknown_for_non_av() {
        // Sanity: things that obviously aren't audio/video must
        // not be misrouted to either tier.  If you add an extension
        // to one of the curated lists above, audit this list too.
        for ext in ["txt", "pdf", "png", "rs", "", "audio.tar.gz"] {
            assert_eq!(supported_extension(ext), ExtensionSupport::Unknown, "{ext}");
        }
    }

    #[test]
    fn decode_tier_as_str() {
        // Stable strings for log lines / `doctor` output.
        assert_eq!(DecodeTier::Symphonia.as_str(), "symphonia");
        assert_eq!(DecodeTier::Ffmpeg.as_str(), "ffmpeg");
    }

    #[test]
    fn fallback_policy_default_is_allow_ffmpeg() {
        // Document the default — changing it would break installs
        // relying on transparent ffmpeg fallback for the long tail.
        assert_eq!(FallbackPolicy::default(), FallbackPolicy::AllowFfmpeg);
    }

    #[test]
    #[cfg(not(feature = "crispasr"))]
    fn decode_stub_errors_without_feature() {
        // Without the `crispasr` feature the entry point should
        // error with a clear, actionable message rather than silently
        // returning an empty Vec or compiling away.
        let err = decode_to_16khz_mono(Path::new("/dev/null"), FallbackPolicy::default())
            .expect_err("stub must error without feature");
        let msg = err.to_string();
        assert!(msg.contains("crispasr"), "error must name the feature: {msg}");
        assert!(msg.contains("--features"), "error must suggest the build flag: {msg}");
    }
}
