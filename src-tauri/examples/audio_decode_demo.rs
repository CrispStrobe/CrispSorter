//! P13.5 Phase 1 — smoke example for the `audio` module.
//!
//! Decodes any audio or video file the module supports (WAV / MP3 /
//! M4A / FLAC / OGG / OPUS / AAC / MP4 / MOV / MKV / WebM / M4V via
//! symphonia, plus the AVI / WMV / FLV / TS / AMR long-tail via the
//! ffmpeg shell-out fallback) to 16 kHz mono Float32 PCM and optionally
//! writes the result back out as a WAV — so you can A/B it against the
//! original in your audio player of choice before slice A wires it into
//! `chat transcribe`.
//!
//! Not part of the production build.  Requires `--features crispasr`
//! (the module's symphonia tier is gated behind that feature to keep
//! it out of audio-less builds).
//!
//! ```bash
//! # Decode + print metadata only
//! cargo run -p crispsorter --features crispasr \
//!   --example audio_decode_demo -- /path/to/input.mp3
//!
//! # Decode + write 16 kHz mono PCM back as WAV for verification
//! cargo run -p crispsorter --features crispasr \
//!   --example audio_decode_demo -- /path/to/input.m4a /tmp/out.wav
//!
//! # Strict mode — fail rather than fall back to ffmpeg shell-out
//! cargo run -p crispsorter --features crispasr \
//!   --example audio_decode_demo -- --pure-rust /path/to/input.opus
//! ```

#[cfg(feature = "crispasr")]
fn main() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use std::time::Instant;
    use tauri_app_lib::audio::{
        decode_to_16khz_mono, supported_extension, writer, ExtensionSupport, FallbackPolicy,
        ASR_CHANNELS, ASR_SAMPLE_RATE,
    };

    // Tiny ad-hoc arg parse — no clap pulled in just for a demo
    // binary.  Strict mode (`--pure-rust`) is a flag, then 1–2
    // positional paths: input, optional output.
    let mut policy = FallbackPolicy::AllowFfmpeg;
    let mut positional: Vec<String> = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--pure-rust" => policy = FallbackPolicy::PureRust,
            "-h" | "--help" => {
                eprintln!(
                    "usage: audio_decode_demo [--pure-rust] <input> [output.wav]\n\
                     \n\
                     Decodes any audio / video file to 16 kHz mono Float32 PCM\n\
                     using symphonia, with ffmpeg fallback for the long tail.\n\
                     Pass --pure-rust to disallow the ffmpeg shell-out path.\n"
                );
                return Ok(());
            }
            _ => positional.push(arg),
        }
    }

    let input = positional
        .first()
        .ok_or_else(|| anyhow::anyhow!("missing <input> path (try --help)"))?;
    let output: Option<PathBuf> = positional.get(1).map(PathBuf::from);
    let input_path = PathBuf::from(input);

    // Up-front extension report so the user sees which tier we expect
    // to handle this file before we even open it.  This is also what
    // the `doctor` subcommand will surface in slice A.
    let ext = input_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let tier_hint = match supported_extension(ext) {
        ExtensionSupport::Symphonia => "tier 1 — pure-Rust symphonia",
        ExtensionSupport::Ffmpeg => "tier 2 — ffmpeg shell-out",
        ExtensionSupport::Unknown => "tier 3 — unknown extension, likely to fail",
    };
    println!("input:        {}", input_path.display());
    println!("extension:    .{ext} ({tier_hint})");
    println!("policy:       {policy:?}");
    println!();

    let started = Instant::now();
    let decoded = decode_to_16khz_mono(&input_path, policy)?;
    let elapsed = started.elapsed();

    // Compute a peak amplitude so the user has a quick "is this silent?"
    // signal without having to play the file back.  Useful when piping
    // through the example for batch-verifying a directory.
    let peak = decoded
        .pcm
        .iter()
        .fold(0.0f32, |acc, &s| acc.max(s.abs()));

    // realtime factor: source duration / wall-clock decode time.  >> 1
    // means we're decoding many seconds of audio per second of wall
    // time — the metric the eventual transcribe pipeline cares about.
    let rt_factor = if elapsed.as_secs_f64() > 0.0 {
        decoded.duration_seconds / elapsed.as_secs_f64()
    } else {
        f64::INFINITY
    };

    println!("decode tier:  {}", decoded.tier.as_str());
    println!(
        "source:       {} Hz, {} channel(s)",
        decoded.source_sample_rate, decoded.source_channels
    );
    println!(
        "output:       {} Hz, {} channel, {} samples ({:.3} s)",
        ASR_SAMPLE_RATE,
        ASR_CHANNELS,
        decoded.pcm.len(),
        decoded.duration_seconds
    );
    println!(
        "peak |amp|:   {:.4} ({})",
        peak,
        if peak < 1e-5 {
            "WARNING — looks silent"
        } else {
            "ok"
        }
    );
    println!(
        "wall time:    {:.3} s  ({:.1}× realtime)",
        elapsed.as_secs_f64(),
        rt_factor
    );

    if let Some(out_path) = output {
        // Round-trip back to WAV so the user can verify the resample
        // / downmix sounds right.  Writer creates parent dirs as
        // needed — useful when output points into /tmp/foo/bar/.wav
        // for an ad-hoc batch check.
        writer::write_wav_16khz_mono(&out_path, &decoded.pcm)?;
        println!("wrote:        {}", out_path.display());
    }

    Ok(())
}

#[cfg(not(feature = "crispasr"))]
fn main() -> anyhow::Result<()> {
    eprintln!(
        "audio_decode_demo requires the `crispasr` cargo feature — \
         rerun with `--features crispasr` (or one of the backend \
         sub-features: crispasr-metal / -cuda / -vulkan)."
    );
    std::process::exit(2);
}
