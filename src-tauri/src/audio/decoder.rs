//! Pure-Rust audio + video-audio-stream decode via the `symphonia`
//! crate.
//!
//! Handles: WAV / MP3 / M4A / FLAC / OGG / OPUS / AAC for audio,
//! plus MP4 / MOV / MKV / WebM / M4V containers for video (we read
//! only the audio stream — video frames are skipped without
//! decode).
//!
//! Output is always 16 kHz mono Float32 PCM, ready to feed
//! `crispasr::Session::transcribe*`.  Channel downmix +
//! sample-rate conversion live in [`super::resampler`].
//!
//! ## Failure cases routed to the ffmpeg fallback
//!
//! - Container symphonia doesn't ship (`.avi` DivX, `.wmv`, …).
//! - Codec symphonia doesn't ship (e.g. AC-3, EAC-3).
//! - Files with non-trivial DRM (extremely rare; gated upstream).

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::{resampler, DecodeTier, DecodedAudio, ASR_SAMPLE_RATE};

/// Decode the audio stream from `path` to 16 kHz mono Float32 PCM
/// using symphonia.
///
/// Errors out with `Err(_)` when:
/// - the file can't be opened;
/// - symphonia can't probe the container format;
/// - no audio track in the file;
/// - the codec isn't shipped with symphonia (e.g. AC-3, EAC-3).
///
/// All of these fall through to [`super::glint_fallback`], and then to
/// [`super::ffmpeg_fallback`] under the default
/// [`super::FallbackPolicy::AllowFfmpeg`] policy.
pub fn decode_with_symphonia(path: &Path) -> Result<DecodedAudio> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    // Hint with the file extension so symphonia's probe can skip
    // the slow header-sniff path for common cases.  Hint is
    // optional — falls back to magic-byte detection if absent.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("symphonia probe failed: {e}"))?;

    let mut format = probed.format;

    // First track with a non-null codec is our audio target.
    // Containers like MP4 / MKV / WebM expose multiple tracks
    // (audio + video + subtitle); we just want the audio.
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no decodable audio track in {}", path.display()))?;

    let track_id = track.id;
    let source_sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("audio track exposes no sample rate"))?;
    let source_channels = track
        .codec_params
        .channels
        .map(|c| c.count() as u16)
        .unwrap_or(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow!("symphonia codec init failed: {e}"))?;

    // Accumulate interleaved Float32 samples across all packets.
    let mut all_interleaved: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // ResetRequired and end-of-stream both look like
            // unrecoverable I/O errors to the caller — break here.
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::IoError(e)) => return Err(anyhow!("packet read I/O: {e}")),
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(anyhow!("symphonia next_packet: {e}")),
        };

        if packet.track_id() != track_id {
            // Container has multiple tracks; ignore non-audio.
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                if sample_buf.is_none() {
                    let spec = *decoded.spec();
                    let duration = decoded.capacity() as u64;
                    sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
                }
                if let Some(buf) = sample_buf.as_mut() {
                    buf.copy_interleaved_ref(decoded);
                    all_interleaved.extend_from_slice(buf.samples());
                }
            }
            // Recoverable per-packet decode errors: log + continue.
            // Matches `symphonia-play`'s reference logic.
            Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::DecodeError(_)) => {
                continue;
            }
            Err(e) => return Err(anyhow!("symphonia decode: {e}")),
        }
    }

    if all_interleaved.is_empty() {
        return Err(anyhow!(
            "symphonia decoded zero samples from {} (silent or malformed)",
            path.display()
        ));
    }

    // Downmix → resample.  Order matters: resampling each channel
    // separately and downmixing after would double the work.
    let mono = resampler::downmix_to_mono(&all_interleaved, source_channels);
    let pcm = resampler::resample_linear_to_16khz(&mono, source_sample_rate);

    let duration_seconds = pcm.len() as f64 / ASR_SAMPLE_RATE as f64;

    Ok(DecodedAudio {
        pcm,
        source_sample_rate,
        source_channels,
        duration_seconds,
        tier: DecodeTier::Symphonia,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::writer;
    use std::f32::consts::PI;
    use std::path::PathBuf;

    /// Build a tempfile path under the OS temp dir with a unique
    /// suffix so parallel-test runs don't collide.
    fn tmp_path(suffix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("crispsorter_audio_test_{pid}_{nanos}{suffix}"));
        p
    }

    #[test]
    fn decode_synthetic_440hz_wav_roundtrips_through_symphonia() {
        // End-to-end smoke test: synthesise a 440 Hz sine wave at 16 kHz
        // mono, write it as a WAV via our hound writer, decode it via
        // symphonia, confirm:
        //  * sample count matches what we wrote
        //  * source_sample_rate is 16 kHz (no resample needed)
        //  * source_channels is 1
        //  * tier is Symphonia (not Ffmpeg)
        //  * peak amplitude survives the round-trip
        let path = tmp_path(".wav");
        let duration_samples = ASR_SAMPLE_RATE as usize; // 1 second
        let mut pcm: Vec<f32> = Vec::with_capacity(duration_samples);
        for i in 0..duration_samples {
            let t = i as f32 / ASR_SAMPLE_RATE as f32;
            pcm.push(0.5 * (2.0 * PI * 440.0 * t).sin());
        }

        writer::write_wav_16khz_mono(&path, &pcm).expect("write WAV");

        let decoded = decode_with_symphonia(&path).expect("decode WAV");

        assert_eq!(decoded.source_sample_rate, ASR_SAMPLE_RATE);
        assert_eq!(decoded.source_channels, 1);
        assert_eq!(decoded.tier, DecodeTier::Symphonia);

        // Linear resample is a no-op at 16 kHz → 16 kHz, so length
        // should match exactly.
        assert_eq!(decoded.pcm.len(), duration_samples);

        // Peak amplitude survives lossless WAV round-trip (allowing
        // a small floating-point tolerance — hound stores as i16 or
        // f32 depending on the spec we passed).
        let peak = decoded.pcm.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
        assert!(peak > 0.45 && peak <= 0.501, "peak={peak}, expected ~0.5");

        // Approximate duration: 1.0 second ± 1 sample's worth of
        // floating-point slack.
        assert!(
            (decoded.duration_seconds - 1.0).abs() < 1e-3,
            "duration_seconds={}",
            decoded.duration_seconds
        );

        // Best-effort cleanup; some platforms refuse delete-while-
        // open which doesn't apply here but we want graceful exits.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn decode_missing_file_errors_clearly() {
        // Non-existent path → useful error mentioning the path so
        // callers can route it through their normal error UI.
        let err = decode_with_symphonia(Path::new("/nonexistent/path.wav"))
            .expect_err("must fail for missing file");
        let msg = err.to_string();
        assert!(msg.contains("cannot open"), "got: {msg}");
        assert!(msg.contains("/nonexistent/path.wav"), "got: {msg}");
    }
}
