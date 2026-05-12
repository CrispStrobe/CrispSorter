//! Sample-rate conversion to the canonical 16 kHz mono ASR input
//! format.  Linear interpolation — same approach `whisper.cpp` uses
//! internally before feeding its encoder.
//!
//! For ASR the model is robust to minor aliasing so linear is fine;
//! a future quality upgrade would swap in rubato's `SincFixedIn`.
//!
//! Also exposes a channel-downmix helper that averages N channels
//! into a single mono channel.

use crate::audio::ASR_SAMPLE_RATE;

/// Resample `input` from `source_sr` to [`ASR_SAMPLE_RATE`] (16 kHz)
/// using linear interpolation between adjacent input samples.
///
/// Returns `input` cloned if `source_sr == ASR_SAMPLE_RATE` already
/// (zero-copy fast path).  For up-sampling the output is larger;
/// for down-sampling it's smaller — caller doesn't need to size
/// the buffer in advance.
pub fn resample_linear_to_16khz(input: &[f32], source_sr: u32) -> Vec<f32> {
    if source_sr == ASR_SAMPLE_RATE {
        return input.to_vec();
    }
    if input.is_empty() || source_sr == 0 {
        return Vec::new();
    }

    // Output length: input_len * (target / source).  Using f64 to
    // avoid precision loss at long inputs.
    let ratio = ASR_SAMPLE_RATE as f64 / source_sr as f64;
    let out_len = ((input.len() as f64) * ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);

    // Step through output indices, interpolate input.  Equivalent to
    // `whisper.cpp::resample_linear` modulo the index-end clamp.
    let inv_ratio = 1.0 / ratio;
    for i in 0..out_len {
        let src_pos = i as f64 * inv_ratio;
        let src_idx = src_pos.floor() as usize;
        let frac = (src_pos - src_idx as f64) as f32;

        let a = input[src_idx.min(input.len() - 1)];
        let b = input[(src_idx + 1).min(input.len() - 1)];
        out.push(a + (b - a) * frac);
    }

    out
}

/// Downmix `interleaved` from `channels` channels to mono by
/// averaging the samples of each frame.
///
/// `interleaved` is laid out `[L, R, L, R, ...]` for stereo, etc.
/// `channels = 1` is a no-op (returns the input cloned).
pub fn downmix_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let c = channels as usize;
    let frames = interleaved.len() / c;
    let mut out = Vec::with_capacity(frames);
    let inv_c = 1.0 / c as f32;
    for frame in interleaved.chunks_exact(c) {
        let sum: f32 = frame.iter().sum();
        out.push(sum * inv_c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_noop_when_already_16khz() {
        // Zero-copy fast path: same sample rate in → exact same
        // samples out.  Bit-exact, not approximate.
        let input: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let out = resample_linear_to_16khz(&input, ASR_SAMPLE_RATE);
        assert_eq!(out, input);
    }

    #[test]
    fn resample_44khz_to_16khz_roughly_decimates() {
        // 44.1 → 16 = ~0.363 ratio.  1000 input samples should give
        // ~363 output samples (allowing ceil() rounding).
        let input: Vec<f32> = vec![0.5; 1000];
        let out = resample_linear_to_16khz(&input, 44_100);
        let expected = (1000.0_f64 * 16_000.0 / 44_100.0).ceil() as usize;
        assert_eq!(out.len(), expected);
        // Constant signal stays constant under linear interpolation —
        // any drift here would mean a bug in the index math.
        for s in &out {
            assert!((s - 0.5).abs() < 1e-5, "got {s}");
        }
    }

    #[test]
    fn resample_8khz_to_16khz_doubles_length() {
        // 8 → 16 = 2.0× ratio (upsampling).  500 in → ~1000 out.
        let input: Vec<f32> = vec![0.1; 500];
        let out = resample_linear_to_16khz(&input, 8_000);
        let expected = (500.0_f64 * 16_000.0 / 8_000.0).ceil() as usize;
        assert_eq!(out.len(), expected);
        for s in &out {
            assert!((s - 0.1).abs() < 1e-5);
        }
    }

    #[test]
    fn resample_linear_ramp_preserves_slope() {
        // A linear ramp 0.0 → 1.0 over 1000 input samples should
        // resample to a (different-length) linear ramp 0.0 → 1.0.
        // This is the cleanest invariant to test for interpolation
        // correctness without setting an explicit tolerance per
        // sample.
        let input: Vec<f32> = (0..1000).map(|i| i as f32 / 999.0).collect();
        let out = resample_linear_to_16khz(&input, 48_000);
        assert!(out.first().copied().unwrap_or(1.0) < 0.01);
        let last = *out.last().unwrap();
        assert!(last > 0.95, "last sample should approach 1.0, got {last}");
    }

    #[test]
    fn resample_empty_input() {
        // Edge case: empty input → empty output, no panic.  Matters
        // for files where the audio track has zero samples (rare,
        // but symphonia returns it on some malformed files).
        let out = resample_linear_to_16khz(&[], 44_100);
        assert!(out.is_empty());
    }

    #[test]
    fn downmix_mono_is_noop() {
        // 1-channel input → 1-channel output, exact same samples.
        let input = vec![0.1f32, 0.2, 0.3, 0.4];
        let out = downmix_to_mono(&input, 1);
        assert_eq!(out, input);
    }

    #[test]
    fn downmix_stereo_averages_channels() {
        // Stereo `[L, R, L, R]` with L = 0.5, R = -0.5 must average
        // to 0.0 across every frame — the canonical mid/side test.
        let input = vec![0.5, -0.5, 0.5, -0.5, 0.5, -0.5];
        let out = downmix_to_mono(&input, 2);
        assert_eq!(out, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn downmix_stereo_in_phase() {
        // Stereo `[L, R, L, R]` with L = R = 0.7 stays at 0.7 mono
        // (no attenuation — we average, not sum).
        let input = vec![0.7, 0.7, 0.7, 0.7];
        let out = downmix_to_mono(&input, 2);
        assert_eq!(out, vec![0.7, 0.7]);
    }

    #[test]
    fn downmix_surround_5point1_averages_six_channels() {
        // 5.1 layout `[FL, FR, FC, LFE, RL, RR]` × N frames.  All
        // channels at 0.6 → mono output at 0.6 (average preserved).
        let input = vec![0.6; 6 * 100];
        let out = downmix_to_mono(&input, 6);
        assert_eq!(out.len(), 100);
        for s in &out {
            assert!((s - 0.6).abs() < 1e-5);
        }
    }
}
