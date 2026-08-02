//! Tier-2 decode via [glint](https://github.com/CrispStrobe/glint) — a
//! clean-room MIT C++17 codec suite linked into the binary.
//!
//! PLAN P36.3. This tier exists so that losing the ffmpeg shell-out costs
//! *containers* rather than the entire fallback: everything below is
//! reachable inside an App Sandbox, on a machine with nothing installed,
//! with no PATH lookup and no child process.
//!
//! ## What it covers, and what it does not
//!
//! glint is a **codec** suite with just enough container handling to read
//! the files those codecs normally arrive in. `glint_decode_audio` sniffs
//! the header and decodes:
//!
//! | Format | Reachable here |
//! |---|---|
//! | MP3 (MPEG-1/2 Layer III) | ✅ elementary stream, ID3-tagged files |
//! | AAC-LC in ADTS | ✅ bare `.aac` |
//! | Ogg-Opus | ✅ `.opus`, incl. multistream/surround |
//! | Ogg-Vorbis I | ✅ `.ogg` |
//! | FLAC (native) | ✅ `.flac` |
//! | WAV | ✅ (symphonia gets there first) |
//! | MP4/M4A, MKV, WebM, AVI, WMV, FLV, MPEG-TS | ❌ **no demuxer** |
//!
//! The last row is the honest limit and the reason this is a *second*
//! tier rather than a replacement for the third. symphonia already demuxes
//! MP4/MKV/WebM, so the practical residue that only ffmpeg can reach is
//! AVI, WMV, FLV, MPEG-TS, AMR and RealAudio — recorded as
//! [`super::ExtensionSupport::Ffmpeg`], and genuinely absent from
//! `desktop-mas` builds.
//!
//! Where this tier earns its place is the case symphonia fails on a file
//! whose *codec* is mainstream: a raw ADTS stream with no container, an
//! MP3 whose first frame is damaged, an Opus file symphonia's demuxer
//! rejects. Those are decoded here instead of demanding ffmpeg.
//!
//! ## Shape of the output
//!
//! `glint::decode_audio_rate(bytes, 16_000)` resamples inside the codec
//! (its own polyphase resampler) and returns interleaved f32. We downmix
//! to mono ourselves — glint has no channel-mix step — so the result
//! matches the symphonia tier exactly: 16 kHz, mono, Float32 in
//! `[-1.0, 1.0]`.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use super::{DecodeTier, DecodedAudio, ASR_SAMPLE_RATE};

/// Files above this are not read into memory for a fallback decode.
///
/// The whole file has to be resident because glint's API takes a byte
/// slice, not a reader. That is fine for the audio this tier exists to
/// rescue — a 500 MB MP3 does not occur — and refusing early is better
/// than an allocation failure inside FFI. symphonia (streaming) has no
/// such limit, and it is tier 1, so the cap only ever bites on files it
/// already rejected.
const MAX_IN_MEMORY_BYTES: u64 = 512 * 1024 * 1024;

/// Decode `path` in-process via glint. Returns `Err(_)` when the file is
/// unreadable, too large to buffer, in a container glint has no demuxer
/// for, or decodes to zero samples.
pub fn decode_with_glint(path: &Path) -> Result<DecodedAudio> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    if metadata.len() > MAX_IN_MEMORY_BYTES {
        return Err(anyhow!(
            "{} is {:.1} MB — too large for the in-process decode tier \
             (limit {} MB); it needs a streaming demuxer",
            path.display(),
            metadata.len() as f64 / (1024.0 * 1024.0),
            MAX_IN_MEMORY_BYTES / (1024 * 1024)
        ));
    }

    let bytes =
        std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.is_empty() {
        return Err(anyhow!("{} is empty", path.display()));
    }

    // `decode_audio_rate` resamples to our target inside the codec, so
    // there is no rubato pass on this tier.
    let decoded = glint::decode_audio_rate(&bytes, ASR_SAMPLE_RATE).ok_or_else(|| {
        anyhow!(
            "glint could not decode {} — it reads MP3, AAC-LC (ADTS), \
             Ogg-Opus, Ogg-Vorbis, FLAC and WAV, but has no demuxer for \
             AVI / WMV / FLV / MPEG-TS containers",
            path.display()
        )
    })?;

    if decoded.channels == 0 {
        return Err(anyhow!("glint reported zero channels for {}", path.display()));
    }
    if decoded.pcm.is_empty() {
        return Err(anyhow!(
            "glint decoded zero samples from {} (no audio stream?)",
            path.display()
        ));
    }

    let source_channels = u16::try_from(decoded.channels).unwrap_or(u16::MAX);
    let pcm = downmix_to_mono(&decoded.pcm, decoded.channels as usize);
    let duration_seconds = pcm.len() as f64 / ASR_SAMPLE_RATE as f64;

    Ok(DecodedAudio {
        pcm,
        // glint resampled internally, so the file's original rate is not
        // recoverable from what it returns — `decoded.sample_rate` is the
        // *output* rate we asked for. Report 0 ("we don't know") rather
        // than the misleading 16000, matching the ffmpeg tier's contract.
        source_sample_rate: 0,
        source_channels,
        duration_seconds,
        tier: DecodeTier::Glint,
    })
}

/// Average interleaved frames down to one channel.
///
/// Averaging, not summing: summing a 5.1 mix clips hard, and every
/// consumer of this PCM expects `[-1.0, 1.0]`. Mono input is returned
/// unchanged rather than run through the loop.
fn downmix_to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let scale = 1.0 / channels as f32;
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() * scale)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_input_passes_through_untouched() {
        let pcm = vec![0.25, -0.5, 0.75];
        assert_eq!(downmix_to_mono(&pcm, 1), pcm);
        // Zero channels is not a shape we should ever be handed, but
        // dividing by it would produce NaNs rather than an error, so the
        // guard is the same branch.
        assert_eq!(downmix_to_mono(&pcm, 0), pcm);
    }

    #[test]
    fn stereo_frames_average_rather_than_sum() {
        // Both channels at full scale must stay at full scale. Summing
        // would give 2.0 and clip everywhere downstream — the specific
        // bug this function's shape is chosen to avoid.
        let interleaved = vec![1.0, 1.0, -1.0, -1.0, 1.0, 0.0];
        assert_eq!(downmix_to_mono(&interleaved, 2), vec![1.0, -1.0, 0.5]);
    }

    #[test]
    fn a_trailing_partial_frame_is_dropped_not_misaligned() {
        // `chunks_exact` leaves a partial frame behind. That is the right
        // call: half a frame has no defined mono value, and including it
        // would shift every subsequent sample's channel assignment.
        let interleaved = vec![1.0, 1.0, 0.5];
        assert_eq!(downmix_to_mono(&interleaved, 2), vec![1.0]);
    }

    #[test]
    fn a_missing_file_errors_rather_than_panicking() {
        let err = decode_with_glint(Path::new("/nonexistent/clip.mp3"))
            .expect_err("must error for a missing input");
        assert!(err.to_string().contains("/nonexistent/clip.mp3"), "got: {err}");
    }

    #[test]
    fn a_non_audio_file_is_rejected_with_an_actionable_message() {
        // The realistic failure: a container glint has no demuxer for.
        // The message has to say what it *can* read, or the user has no
        // way to tell whether the file or the build is the problem.
        let mut path = std::env::temp_dir();
        path.push(format!("crispsorter_glint_not_audio_{}.bin", std::process::id()));
        std::fs::write(&path, b"RIFFnope not a real audio file at all").expect("write");

        let err = decode_with_glint(&path).expect_err("must reject non-audio");
        let msg = err.to_string();
        assert!(msg.contains("MP3"), "should name the covered codecs: {msg}");

        let _ = std::fs::remove_file(&path);
    }
}
