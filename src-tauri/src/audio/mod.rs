//! Cross-platform audio + video decoding to 16 kHz mono Float32 PCM —
//! the canonical input format for CrispASR.
//!
//! ## Three decode tiers
//!
//! 1. **Pure-Rust via [symphonia](https://github.com/pdeljanov/Symphonia)
//!    + [rubato](https://github.com/HEnquist/rubato):**
//!    - audio: WAV / MP3 / M4A / FLAC / OGG / OPUS / AAC
//!    - video: MP4 / MOV / MKV / WebM / M4V (audio stream demuxed,
//!      video frames skipped)
//!    No system deps — ships everywhere CrispSorter does.
//!
//! 2. **glint** (`glint_fallback`, `feature = "audio-glint"`) — a
//!    clean-room MIT C++17 codec suite linked into the binary. Decodes
//!    MP3, AAC-LC, Ogg-Opus, Ogg-Vorbis and FLAC from their own headers,
//!    so it covers *elementary streams and bare codec files* symphonia
//!    rejects (a raw ADTS `.aac`, an `.mp3` with a damaged first frame,
//!    an Opus stream symphonia's demuxer won't take). It is a codec
//!    suite, not a demuxer: it cannot open AVI, WMV, FLV or MPEG-TS,
//!    and does not pretend to.
//!
//! 3. **ffmpeg shell-out** (`ffmpeg_fallback`, `feature = "sidecars"`)
//!    for the container long tail glint cannot reach: AVI DivX, WMV,
//!    FLV, TS, AMR, RA. Only fires when both tiers above reject the file
//!    AND `ffmpeg` is on PATH; otherwise emits a clear "install ffmpeg
//!    for .<ext>" message rather than silent failure.
//!
//! Tier 3 is the one that disappears under App Sandbox (PLAN P36.3), and
//! tier 2 exists so that losing it costs codecs rather than the whole
//! fallback. What is genuinely lost on `desktop-mas` is the *container*
//! long tail — recorded in `ExtensionSupport::Ffmpeg` and reported
//! honestly by `supported_extension`.
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
pub mod probe;
#[cfg(all(feature = "crispasr", feature = "sidecars"))]
pub mod ffmpeg_fallback;
#[cfg(all(feature = "crispasr", feature = "audio-glint"))]
pub mod glint_fallback;
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
    /// Which tier handled the decode — `"symphonia"`, `"glint"` or
    /// `"ffmpeg"`.  Lets callers tag log lines / emit warnings about
    /// shell-out usage on shipping installs.
    pub tier: DecodeTier,
}

/// Which decode tier produced [`DecodedAudio`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeTier {
    /// Pure-Rust path (symphonia + rubato).
    Symphonia,
    /// In-process codec suite (glint), linked in — no process, no PATH.
    Glint,
    /// ffmpeg shell-out fallback.
    Ffmpeg,
}

impl DecodeTier {
    pub fn as_str(self) -> &'static str {
        match self {
            DecodeTier::Symphonia => "symphonia",
            DecodeTier::Glint => "glint",
            DecodeTier::Ffmpeg => "ffmpeg",
        }
    }

    /// Whether reaching this tier required running another program.
    /// The distinction the sandbox cares about, and the one worth
    /// logging: an in-process tier is available everywhere the binary is.
    pub fn spawns_a_process(self) -> bool {
        matches!(self, DecodeTier::Ffmpeg)
    }
}

/// Whether to allow the ffmpeg shell-out fallback when the in-process
/// tiers can't read a file.  Default: [`FallbackPolicy::AllowFfmpeg`].
///
/// The policy only governs tier 3. glint is in-process, so it runs under
/// both policies — `PureRust` means "nothing gets spawned", not "symphonia
/// only", and a file glint can decode is decodable purely in-process by
/// definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FallbackPolicy {
    /// Try symphonia, then glint; on failure, try `ffmpeg` if it's on
    /// PATH.  Errors with a clear "install ffmpeg for .<ext>" message
    /// when every in-process tier fails AND ffmpeg isn't found.
    #[default]
    AllowFfmpeg,
    /// In-process tiers only.  Errors if the file isn't decodable
    /// without spawning.  Useful for environments where shell-out
    /// is forbidden (sandboxes, embedded use) — and the only behaviour
    /// available at all on builds without the `sidecars` feature, which
    /// carry no ffmpeg tier to permit.
    PureRust,
}

/// Decode `path` to 16 kHz mono Float32 PCM.  Tries symphonia, then
/// glint, then optionally ffmpeg per `policy`.
///
/// **Errors** when every compiled-in tier rejects the file, or the audio
/// stream is silent (zero samples).  The error names each tier that was
/// tried and why it failed, because "audio decode failed" on its own
/// tells the user nothing about whether to install ffmpeg, re-encode, or
/// give up.
#[cfg(feature = "crispasr")]
pub fn decode_to_16khz_mono(path: &Path, policy: FallbackPolicy) -> Result<DecodedAudio> {
    // Tier 1: symphonia.
    let symphonia_err = match decoder::decode_with_symphonia(path) {
        Ok(d) => return Ok(d),
        Err(e) => e,
    };

    // Tier 2: glint — in-process, so no policy check. Runs before ffmpeg
    // even when ffmpeg is allowed: an in-process decode is faster, has no
    // PATH dependency, and cannot be affected by whatever ffmpeg build the
    // host happens to have.
    #[cfg(feature = "audio-glint")]
    let glint_err = match glint_fallback::decode_with_glint(path) {
        Ok(d) => return Ok(d),
        Err(e) => e.to_string(),
    };
    #[cfg(not(feature = "audio-glint"))]
    let glint_err = String::from("not compiled in (feature `audio-glint`)");

    // Tier 3: ffmpeg.
    #[cfg(feature = "sidecars")]
    {
        if policy == FallbackPolicy::AllowFfmpeg {
            return ffmpeg_fallback::decode_with_ffmpeg(path).map_err(|ffmpeg_err| {
                anyhow::anyhow!(
                    "audio decode failed for {}\n  \
                     tier-1 symphonia: {symphonia_err}\n  \
                     tier-2 glint: {glint_err}\n  \
                     tier-3 ffmpeg: {ffmpeg_err}",
                    path.display()
                )
            });
        }
    }

    // Either the caller forbade it or this build has no tier 3 at all.
    // Say which — "ffmpeg fallback not allowed" would be misleading on a
    // sandboxed build, where there was never an ffmpeg tier to allow.
    let why_no_ffmpeg = if cfg!(feature = "sidecars") {
        "PureRust policy — ffmpeg fallback not allowed"
    } else {
        "this build cannot spawn ffmpeg (feature `sidecars` is off)"
    };
    let _ = policy;
    Err(anyhow::anyhow!(
        "audio decode failed for {} ({why_no_ffmpeg})\n  \
         tier-1 symphonia: {symphonia_err}\n  \
         tier-2 glint: {glint_err}",
        path.display()
    ))
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
///
/// **glint does not appear in this map, on purpose.** It adds no new
/// extension: every codec it decodes (MP3, AAC, Opus, Vorbis, FLAC, WAV)
/// is one symphonia already claims. What it adds is a second attempt at
/// files symphonia claims and then fails on — a damaged first frame, a
/// bare ADTS stream, an Opus file its demuxer rejects — which is a
/// per-*file* property this per-*extension* lookup cannot express. The
/// tier-3 row below is therefore the real answer to "what does a
/// sandboxed build lose": AVI, WMV, FLV, MPEG-TS, AMR and RealAudio,
/// which need a demuxer neither in-process tier has.
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
    /// Needs the tier-3 ffmpeg shell-out: a container no in-process
    /// demuxer in the tree can open.  Absent from builds without the
    /// `sidecars` feature — these extensions simply do not decode there.
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
