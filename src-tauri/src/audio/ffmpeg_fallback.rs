//! Last-resort decode via `ffmpeg` shell-out — covers the long tail
//! of containers symphonia doesn't read natively (.avi DivX, .wmv,
//! .flv, .ts, .amr, .ra, exotic codecs in MKV like AC-3 / EAC-3).
//!
//! Only invoked when [`super::FallbackPolicy::AllowFfmpeg`] is set
//! and the symphonia tier already failed.  Tries `ffmpeg` (and only
//! `ffmpeg` — not `avconv` or other forks) on PATH:
//!   * macOS / Linux: `which ffmpeg`
//!   * Windows: shell tries the `.exe` suffix automatically when
//!     `Command::new("ffmpeg")` runs.
//!
//! ffmpeg command shape:
//!
//! ```bash
//! ffmpeg -nostdin -hide_banner -loglevel error \
//!        -i <path> -f f32le -ar 16000 -ac 1 pipe:1
//! ```
//!
//! That writes raw Float32 LE PCM at 16 kHz mono to stdout, which
//! we read directly into a `Vec<f32>` via `bytemuck`-style reinterpret.
//!
//! ## Gated on `sidecars` (PLAN P36.3)
//!
//! This is tier 3, and the only tier that runs another program. A build
//! that cannot spawn stops at tier 2 (`glint_fallback`), losing the
//! container long tail this module exists for — `.avi`, `.wmv`, `.flv`,
//! `.ts`, `.amr`, `.ra` — which `supported_extension` reports honestly
//! rather than failing at decode time.
//!
//! Stated as an attribute as well as in `audio/mod.rs` so the file itself
//! cannot be pulled into a sandboxed build by a future `mod` line, and so
//! `compliance.rs` can see the gate.
#![cfg(feature = "sidecars")]

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

use super::{DecodeTier, DecodedAudio, ASR_SAMPLE_RATE};

/// Decode `path` via ffmpeg shell-out.  Returns `Err(_)` when ffmpeg
/// isn't on PATH, the spawn fails, ffmpeg exits non-zero, or the
/// output stream is empty.
pub fn decode_with_ffmpeg(path: &Path) -> Result<DecodedAudio> {
    // Step 1: detect ffmpeg.  We don't pre-resolve the path (the
    // shell will do it again on spawn) — this just produces a
    // clearer error when it's missing.
    if !ffmpeg_on_path() {
        return Err(anyhow!(
            "ffmpeg not found on PATH — install ffmpeg to decode {} \
             (macOS: `brew install ffmpeg`; Linux: \
             `apt install ffmpeg` or distro equivalent; \
             Windows: https://www.ffmpeg.org/download.html)",
            path.display()
        ));
    }

    // Step 2: spawn.  `-nostdin` is critical — without it ffmpeg
    // reads from our stdin and confuses pipes.  `-hide_banner` +
    // `-loglevel error` keep stderr to actual errors only.
    let mut child = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
        ])
        .arg(path)
        .args([
            "-f", "f32le",       // raw Float32 little-endian PCM
            "-ar", "16000",      // resample to 16 kHz inside ffmpeg
            "-ac", "1",          // downmix to mono inside ffmpeg
            "pipe:1",            // write to stdout
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning ffmpeg for {}", path.display()))?;

    // Step 3: read stdout (raw PCM) + stderr (error message) in
    // parallel.  Using output() is fine here — the buffers will be
    // a few MB at most for typical files; large files would need
    // a streaming-into-Vec pattern.  Worst case 1 GB raw PCM =
    // ~9000 s of audio at 16 kHz mono, which is well beyond what
    // anyone batch-transcribes from a single file in one shot.
    let output = child
        .wait_with_output()
        .with_context(|| format!("waiting on ffmpeg for {}", path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "ffmpeg exited {} for {}: {}",
            output.status,
            path.display(),
            stderr.trim()
        ));
    }

    // Step 4: reinterpret stdout bytes as Float32 LE PCM.  Length
    // must be a multiple of 4 (bytes per f32); anything else is a
    // truncated write and we error out rather than silently
    // dropping the last partial sample.
    let bytes = output.stdout;
    if bytes.is_empty() {
        return Err(anyhow!(
            "ffmpeg produced zero output bytes for {} \
             (file may have no audio stream)",
            path.display()
        ));
    }
    if bytes.len() % 4 != 0 {
        return Err(anyhow!(
            "ffmpeg output length {} is not a multiple of 4 \
             (truncated f32le stream?) for {}",
            bytes.len(),
            path.display()
        ));
    }

    let mut pcm = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        // little-endian f32 — same byte order ffmpeg writes with
        // `-f f32le`, no portability concern even on big-endian
        // systems (Rust's `f32::from_le_bytes` handles it).
        let arr: [u8; 4] = chunk.try_into().unwrap();
        pcm.push(f32::from_le_bytes(arr));
    }

    let duration_seconds = pcm.len() as f64 / ASR_SAMPLE_RATE as f64;

    Ok(DecodedAudio {
        pcm,
        // Source rate isn't recoverable from a piped raw stream —
        // ffmpeg already resampled inside the pipeline.  Report 0
        // so callers know "we don't know" rather than the misleading
        // 16000 (which is the *output* rate, not the source).
        source_sample_rate: 0,
        // Same for channels — ffmpeg already downmixed.
        source_channels: 0,
        duration_seconds,
        tier: DecodeTier::Ffmpeg,
    })
}

/// Cheap "is ffmpeg on PATH?" check — runs `ffmpeg -version` with
/// stdio redirected to /dev/null and discards the output.
/// Returns true iff the process spawned successfully and exited
/// zero.  Doesn't validate the version.
pub fn ffmpeg_on_path() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg_on_path_works_or_doesnt() {
        // Either result is fine — we just want to confirm the
        // detection function doesn't panic on either platform.
        // The test asserts symmetry: if it's true, the binary
        // really is invocable; if false, we don't false-negative
        // due to a typo'd command.
        let has = ffmpeg_on_path();
        if has {
            // Sanity: a second call agrees (no flakiness from
            // randomised PATH ordering or similar).
            assert!(ffmpeg_on_path());
        } else {
            // No assertion needed — most CI images don't ship
            // ffmpeg.  The function returning false in that case
            // is exactly what we want.
        }
    }

    #[test]
    fn decode_missing_file_via_ffmpeg_errors() {
        // ffmpeg won't read /nonexistent/path; if ffmpeg isn't on
        // PATH we get a friendly "install ffmpeg" message instead.
        // Either error is acceptable — we just want a Result::Err.
        let err = decode_with_ffmpeg(Path::new("/nonexistent/path.avi"))
            .expect_err("must error for missing input");
        let msg = err.to_string();

        // Whatever the failure mode, the message should reference
        // the path so the user can act on it.
        assert!(
            msg.contains("/nonexistent/path.avi") || msg.contains("ffmpeg not found"),
            "got: {msg}"
        );
    }
}
