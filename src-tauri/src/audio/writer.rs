//! WAV writer for synthesised speech (slice A's `chat tts --output
//! file.wav`) and for testing the symphonia decoder via round-trip.
//!
//! Writes 16 kHz mono Float32 WAVs by default — same shape every
//! CrispASR-friendly tool consumes.  CrispASR's `Session::synthesize`
//! returns 24 kHz mono Float32; callers that want to preserve that
//! rate should use [`write_wav_mono`] instead.

use anyhow::{Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use std::path::Path;

use super::ASR_SAMPLE_RATE;

/// Write `pcm` (Float32 in `[-1.0, 1.0]`) as a 16 kHz mono Float32
/// WAV file at `path`.  Overwrites any existing file at that path.
pub fn write_wav_16khz_mono(path: &Path, pcm: &[f32]) -> Result<()> {
    write_wav_mono(path, pcm, ASR_SAMPLE_RATE)
}

/// Write `pcm` (Float32 in `[-1.0, 1.0]`) as a mono Float32 WAV at
/// the given `sample_rate`.  Use this for TTS output where the
/// backend's native rate is not 16 kHz (kokoro = 24 kHz, qwen3-tts
/// = 12 kHz, etc.) — preserving the native rate avoids one extra
/// resample step.
pub fn write_wav_mono(path: &Path, pcm: &[f32], sample_rate: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating parent directory {}", parent.display())
            })?;
        }
    }

    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };

    let mut writer = WavWriter::create(path, spec)
        .with_context(|| format!("creating WAV writer at {}", path.display()))?;

    for &sample in pcm {
        writer
            .write_sample(sample)
            .with_context(|| format!("writing sample to {}", path.display()))?;
    }

    writer
        .finalize()
        .with_context(|| format!("finalising WAV {}", path.display()))?;

    Ok(())
}

/// Same WAV as [`write_wav_mono`], returned as bytes instead of written
/// to a file.
///
/// P36.2 — the sandboxed TTS path hands synthesised speech to the webview
/// to play, so the audio must never touch the filesystem: a container has
/// nowhere obvious to put it and no reason to. `hound` writes into any
/// `Write + Seek`, and `Cursor<Vec<u8>>` is both.
pub fn wav_mono_bytes(pcm: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };

    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer =
            WavWriter::new(&mut buffer, spec).context("creating in-memory WAV writer")?;
        for &sample in pcm {
            writer.write_sample(sample).context("writing sample to WAV buffer")?;
        }
        // Explicit rather than on drop: `finalize` is what backfills the
        // RIFF/data chunk lengths, and a drop-finalised writer swallows the
        // error if that seek fails, yielding a header that lies about its
        // own payload size.
        writer.finalize().context("finalising in-memory WAV")?;
    }
    Ok(buffer.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(suffix: &str) -> std::path::PathBuf {
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
    fn write_then_read_back_via_hound() {
        // Write a known buffer through our writer, read it back via
        // hound directly (bypassing our symphonia decoder so this
        // test isolates the writer side of the round-trip).
        let path = tmp_path(".wav");
        let pcm: Vec<f32> = (0..ASR_SAMPLE_RATE as usize)
            .map(|i| (i as f32 / 1000.0).sin() * 0.3)
            .collect();

        write_wav_16khz_mono(&path, &pcm).expect("write");

        let mut reader = hound::WavReader::open(&path).expect("hound::open");
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, ASR_SAMPLE_RATE);
        assert_eq!(spec.bits_per_sample, 32);
        assert_eq!(spec.sample_format, SampleFormat::Float);

        let samples: Vec<f32> = reader.samples::<f32>().filter_map(|s| s.ok()).collect();
        assert_eq!(samples.len(), pcm.len());

        // Bit-exact: f32 WAV preserves values exactly.
        for (a, b) in samples.iter().zip(pcm.iter()) {
            assert_eq!(a.to_bits(), b.to_bits());
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_creates_parent_dir() {
        // mkdir -p the parent path so callers don't have to.  Matches
        // how the rest of CrispSorter handles output paths (catalog
        // scan --out, batch run --out-dir, …).
        let mut base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        base.push(format!("crispsorter_audio_writer_dir_{nanos}"));
        let path = base.join("subdir").join("out.wav");

        let pcm = vec![0.0f32; 100];
        write_wav_16khz_mono(&path, &pcm).expect("write");

        assert!(path.exists(), "output file not created");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn in_memory_wav_matches_the_file_writer_byte_for_byte() {
        // The sandboxed TTS path depends on these two producing the same
        // container — if the in-memory variant ever drifts (a missing
        // finalize, a different chunk order), the webview gets a WAV that
        // decodes differently from the one `chat tts` writes, and nothing
        // else in the tree would notice.
        let pcm: Vec<f32> = (0..2_400).map(|i| (i as f32 / 97.0).sin() * 0.4).collect();

        let path = tmp_path("_bytes.wav");
        write_wav_mono(&path, &pcm, 24_000).expect("write file");
        let from_file = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);

        let from_memory = wav_mono_bytes(&pcm, 24_000).expect("write memory");
        assert_eq!(from_file, from_memory);

        // And it must be a WAV a decoder will accept, not just an equal
        // pile of bytes.
        let mut reader =
            hound::WavReader::new(std::io::Cursor::new(from_memory)).expect("hound::new");
        assert_eq!(reader.spec().sample_rate, 24_000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.samples::<f32>().count(), pcm.len());
    }

    #[test]
    fn write_at_24khz_for_tts_output() {
        // TTS backends (kokoro = 24 kHz, qwen3-tts = 12 kHz) want
        // their native rate preserved.  write_wav_mono must respect
        // the sample_rate the caller passes.
        let path = tmp_path("_tts.wav");
        let pcm = vec![0.1f32; 24_000];
        write_wav_mono(&path, &pcm, 24_000).expect("write 24k");

        let reader = hound::WavReader::open(&path).expect("hound::open");
        assert_eq!(reader.spec().sample_rate, 24_000);
        let _ = std::fs::remove_file(&path);
    }
}
