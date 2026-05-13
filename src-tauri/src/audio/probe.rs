//! Lightweight audio metadata probe — symphonia format reader only,
//! NO decode pass.  Returns the L2 metadata fields (duration / codec /
//! sample rate / channels / bitrate) that the UI surfaces in BatchReview
//! row tooltips and the LanceDB audio columns added by migration v101.
//!
//! Cost: a single `Probe::format` call plus reading `CodecParameters`
//! off the first audio track.  For a 200-MB mp3, this returns in
//! milliseconds — far cheaper than the full PCM decode in
//! `audio::decoder`.  Safe to call inline on the UI thread when the
//! file is small; bg_ingest still wraps it in `spawn_blocking` to be
//! safe with the long-tail of containers whose probe scans deeper.
//!
//! Codec name resolution: maps the `CodecType` opaque ID to a
//! human-readable family string ("mp3" / "aac" / "flac" / "opus" / …).
//! Falls back to `unknown` for codec IDs symphonia doesn't expose
//! through its public constants — fine for the UI which just needs
//! something to display next to the duration.

#[cfg(feature = "crispasr")]
use anyhow::{anyhow, Context, Result};
#[cfg(feature = "crispasr")]
use std::path::Path;

#[cfg(feature = "crispasr")]
use symphonia::core::codecs::{self, CodecType, CODEC_TYPE_NULL};
#[cfg(feature = "crispasr")]
use symphonia::core::formats::FormatOptions;
#[cfg(feature = "crispasr")]
use symphonia::core::io::MediaSourceStream;
#[cfg(feature = "crispasr")]
use symphonia::core::meta::MetadataOptions;
#[cfg(feature = "crispasr")]
use symphonia::core::probe::Hint;

/// L2 audio metadata returned by the JS-side `audio_metadata` Tauri
/// command and used by bg_ingest to populate the LanceDB audio
/// columns added in schema-migration v101.
///
/// Fields are all optional because symphonia doesn't always expose
/// every datapoint — VBR mp3s have `n_frames=None` until you scan
/// the whole stream, m4a captured from iPhone screen-recording lacks
/// `bits_per_sample` in some headers, etc.  The UI falls back
/// gracefully (renders "—" / hides the row entry) for missing fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioMetadata {
    /// Total playback duration in seconds (f64 for sub-second accuracy
    /// on short clips).  Computed as `n_frames * time_base`; `None`
    /// when the container header is incomplete or the codec is
    /// stream-only without a frame count.
    pub duration_seconds: Option<f64>,
    /// Friendly codec name ("mp3" / "aac" / "flac" / "opus" / "vorbis"
    /// / "pcm" / "alac" / "wav" / …).  Resolved by `codec_family_for`
    /// below.  `None` only on completely unexpected codec IDs.
    pub codec: Option<String>,
    /// Sample rate in Hz (44100 / 48000 / 96000 / …).
    pub sample_rate_hz: Option<u32>,
    /// Channel count (1=mono / 2=stereo / 6=5.1 / …).
    pub channels: Option<u16>,
    /// Average bitrate in kilobits/second.  For lossy codecs (mp3/aac/
    /// opus): derived from `file_size_bytes * 8 / duration / 1000`
    /// (the codec params don't always carry a `bit_rate` field).
    /// For lossless containers (wav/flac/aiff): `sample_rate * channels
    /// * bits_per_sample / 1000`.
    pub bitrate_kbps: Option<u32>,
}

/// Probe `path` and return its L2 metadata.  Cheap — no decode pass.
///
/// Errors only when the file can't be opened or symphonia rejects
/// the container outright; missing per-field data is reported as
/// `None` inside the returned struct rather than a hard error
/// (matches the UI's "best-effort metadata" expectation).
#[cfg(feature = "crispasr")]
pub fn probe_metadata(path: &Path) -> Result<AudioMetadata> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let file_size_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| anyhow!("symphonia probe failed for {}: {e}", path.display()))?;

    let format = probed.format;

    // First non-null audio track.  Matches decoder.rs's selection.
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no decodable audio track in {}", path.display()))?;

    let cp = &track.codec_params;
    let sample_rate_hz = cp.sample_rate;
    let channels = cp.channels.map(|c| c.count() as u16);

    // Duration: n_frames * time_base.  `time_base` is the inverse of
    // a "ticks per second" value; multiplying gives seconds.  Some
    // containers report `n_frames` in audio-frame units (== samples
    // for mono, samples/channel for multichannel).
    let duration_seconds = match (cp.n_frames, cp.time_base) {
        (Some(n), Some(tb)) => {
            let time = tb.calc_time(n);
            Some(time.seconds as f64 + time.frac)
        }
        _ => None,
    };

    // Bitrate: lossy → derive from file size; lossless → derive from
    // codec params.  Falls back to None on insufficient data.
    let bitrate_kbps = compute_bitrate_kbps(cp, duration_seconds, file_size_bytes);

    let codec = Some(codec_family_for(cp.codec).to_string());

    Ok(AudioMetadata {
        duration_seconds,
        codec,
        sample_rate_hz,
        channels,
        bitrate_kbps,
    })
}

/// Stub for builds without the `crispasr` feature.  Same shape as
/// the other audio module entry points so call sites compile
/// unconditionally.
#[cfg(not(feature = "crispasr"))]
pub fn probe_metadata(_path: &std::path::Path) -> anyhow::Result<AudioMetadata> {
    anyhow::bail!(
        "audio metadata probe requires the `crispasr` cargo feature \
         (build with --features crispasr-metal / -cuda / -vulkan)"
    )
}

/// Map symphonia's opaque `CodecType` IDs to friendly family
/// strings.  Returns "unknown" for codec IDs not covered — those
/// would typically come from containers that hint at a codec
/// symphonia doesn't ship the decoder for (e.g. AC-3, EAC-3), in
/// which case the decode path already errors out earlier.
///
/// PCM family handled by enumerating the explicit
/// `CODEC_TYPE_PCM_*` constants — `CodecType` is a tuple struct
/// with a private inner `u32`, so a numeric-range check on
/// `.0` doesn't compile.  The explicit list is verbose but stable
/// across symphonia patch releases.
#[cfg(feature = "crispasr")]
fn codec_family_for(t: CodecType) -> &'static str {
    use symphonia::core::codecs as c;
    // PCM family — all bit-depth / endianness / planar variants.
    if t == c::CODEC_TYPE_PCM_S32LE
        || t == c::CODEC_TYPE_PCM_S32LE_PLANAR
        || t == c::CODEC_TYPE_PCM_S32BE
        || t == c::CODEC_TYPE_PCM_S32BE_PLANAR
        || t == c::CODEC_TYPE_PCM_S24LE
        || t == c::CODEC_TYPE_PCM_S24LE_PLANAR
        || t == c::CODEC_TYPE_PCM_S24BE
        || t == c::CODEC_TYPE_PCM_S24BE_PLANAR
        || t == c::CODEC_TYPE_PCM_S16LE
        || t == c::CODEC_TYPE_PCM_S16LE_PLANAR
        || t == c::CODEC_TYPE_PCM_S16BE
        || t == c::CODEC_TYPE_PCM_S16BE_PLANAR
        || t == c::CODEC_TYPE_PCM_S8
        || t == c::CODEC_TYPE_PCM_S8_PLANAR
        || t == c::CODEC_TYPE_PCM_U32LE
        || t == c::CODEC_TYPE_PCM_U32LE_PLANAR
        || t == c::CODEC_TYPE_PCM_U32BE
        || t == c::CODEC_TYPE_PCM_U32BE_PLANAR
        || t == c::CODEC_TYPE_PCM_U24LE
    {
        return "pcm";
    }
    match t {
        codecs::CODEC_TYPE_MP3 => "mp3",
        codecs::CODEC_TYPE_AAC => "aac",
        codecs::CODEC_TYPE_FLAC => "flac",
        codecs::CODEC_TYPE_ALAC => "alac",
        codecs::CODEC_TYPE_VORBIS => "vorbis",
        codecs::CODEC_TYPE_OPUS => "opus",
        codecs::CODEC_TYPE_ADPCM_MS => "adpcm",
        codecs::CODEC_TYPE_ADPCM_IMA_WAV => "adpcm",
        // u-law / a-law constants aren't exposed in symphonia 0.5 —
        // fall through to "unknown" for those rare codecs.
        _ => "unknown",
    }
}

/// Bitrate derivation.  Two strategies:
/// 1. Lossless (wav/flac/alac): `sample_rate * channels *
///    bits_per_sample / 1000`.  Exact in theory; some FLAC blocks
///    drop `bits_per_sample` so we fall through to (2).
/// 2. Lossy or fallback: `file_size_bytes * 8 / duration_seconds /
///    1000`.  Approximate (includes container overhead, ID3 tags,
///    etc.) but accurate enough for the "show this in a tooltip"
///    use case.
#[cfg(feature = "crispasr")]
fn compute_bitrate_kbps(
    cp: &symphonia::core::codecs::CodecParameters,
    duration_seconds: Option<f64>,
    file_size_bytes: u64,
) -> Option<u32> {
    // Lossless path: precise per spec.
    if let (Some(sr), Some(ch), Some(bps)) =
        (cp.sample_rate, cp.channels, cp.bits_per_sample)
    {
        let chc = ch.count() as u32;
        let rate = sr as u64 * chc as u64 * bps as u64 / 1000;
        if rate > 0 {
            return Some(rate as u32);
        }
    }
    // Lossy / fallback: file_size / duration.
    if let Some(d) = duration_seconds {
        if d > 0.0 && file_size_bytes > 0 {
            let bps = (file_size_bytes as f64 * 8.0 / d / 1000.0).round();
            if bps.is_finite() && bps > 0.0 && bps < u32::MAX as f64 {
                return Some(bps as u32);
            }
        }
    }
    None
}
