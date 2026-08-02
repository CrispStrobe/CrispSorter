//! Text-to-speech bridge — two backends, one frontend contract.
//!
//! ## `sidecars` builds: platform synth
//!
//! Zero-dep, zero-model: macOS `say`, Windows PowerShell SAPI, Linux
//! `spd-say` or `espeak`. Text is piped via stdin where the underlying
//! tool supports it (avoids argv-quoting headaches for arbitrary chat
//! content). The running child lives in `AppState.tts_process` so a
//! `tts_stop` invocation can kill it mid-utterance — matches the Stop
//! button pattern used elsewhere in the app.
//!
//! ## Sandboxed builds (`desktop-mas`, mobile): CrispASR, in-process
//!
//! App Sandbox forbids spawning `say`, so P36.2 routes speech through
//! CrispASR instead: synthesise to PCM in-process, wrap as WAV, hand the
//! bytes to the webview to play. Nothing is spawned and nothing is
//! written to disk.
//!
//! This is a compliance *improvement*, not a workaround. `say` and SAPI
//! emit unmarked audio; `crispasr::Session::synthesize` watermarks every
//! sample it returns (AI Act Art 50(2) — see `docs/ai-act.md`, and
//! `compliance.rs::tts_never_bypasses_the_synthetic_audio_watermark`,
//! which forbids reaching for CrispASR's unmarked sibling call).
//!
//! That guard is a plain text scan of the whole tree, so it fires on the
//! forbidden identifier even inside a comment. Naming it here to explain
//! that we avoid it would trip it — hence the circumlocution above. Worth
//! the awkwardness: a guard that can be talked around is not a guard.
//!
//! Playback lives in the webview rather than in a native audio-output
//! crate on purpose: an `<audio>` element needs no new dependency, no
//! ALSA/CoreAudio linkage, and no sandbox entitlement, and it is the
//! same path iOS will use.

use anyhow::Result;

// Both speech paths add context to their errors, and they are mutually
// exclusive — so the import follows either one being compiled, not just the
// spawning one. (The `not(sidecars)` + `crispasr` combination is the only
// build that compiles `speak_in_process`, and it is easy to never build.)
#[cfg(any(feature = "sidecars", feature = "crispasr"))]
use anyhow::Context;
#[cfg(feature = "sidecars")]
use std::process::Stdio;
#[cfg(feature = "sidecars")]
use tokio::io::AsyncWriteExt;
#[cfg(feature = "sidecars")]
use tokio::process::{Child, Command};

/// What `tts_speak` did, so the frontend knows whether it still has work
/// to do. The native path is fire-and-forget; the in-process path returns
/// audio the webview must play.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SpeakOutcome {
    /// A platform synth is speaking already. Nothing for the caller to do;
    /// `tts_stop` interrupts it.
    Native,
    /// Watermarked WAV the caller must play itself, base64-encoded.
    /// `tts_stop` is a no-op for this mode — the caller owns the element.
    Webview {
        wav_base64: String,
        sample_rate: u32,
    },
}

/// CrispASR TTS emits 24 kHz mono Float32 across every supported backend
/// (per `Session::synthesize`'s docstring). Kept in sync with the CLI's
/// `chat tts` path, which writes WAVs at the same rate.
pub const TTS_SAMPLE_RATE: u32 = 24_000;

/// Synthesise `text` in-process via CrispASR and return it as a WAV.
///
/// The returned samples are watermarked by CrispASR itself — this goes
/// through `synthesize`, which marks, and never through the raw variant
/// that does not (see the module docs on why it is not named here).
#[cfg(all(not(feature = "sidecars"), feature = "crispasr"))]
pub async fn speak_in_process(
    handle: &crate::asr::AsrHandle,
    text: &str,
) -> Result<SpeakOutcome> {
    use base64::Engine as _;

    let pcm = handle
        .synthesize(text.to_owned())
        .await
        .context("CrispASR synthesis failed")?;
    if pcm.is_empty() {
        anyhow::bail!("CrispASR returned no audio for this text");
    }
    let wav = crate::audio::writer::wav_mono_bytes(&pcm, TTS_SAMPLE_RATE)
        .context("wrapping synthesised PCM as WAV")?;
    Ok(SpeakOutcome::Webview {
        wav_base64: base64::engine::general_purpose::STANDARD.encode(&wav),
        sample_rate: TTS_SAMPLE_RATE,
    })
}

/// Stub for sandboxed builds compiled without CrispASR. Speech needs a
/// synthesiser; without one there is nothing honest to return.
#[cfg(all(not(feature = "sidecars"), not(feature = "crispasr")))]
pub async fn speak_in_process(
    _handle: &crate::asr::AsrHandle,
    _text: &str,
) -> Result<SpeakOutcome> {
    anyhow::bail!(
        "this build has no speech synthesiser: platform synth needs the \
         `sidecars` feature (which spawns a process, and so is absent from \
         sandboxed builds), and in-process synthesis needs `crispasr`"
    )
}

/// Platform-specific synth invocation. Returns a spawned, running
/// child whose stdin has already been fed `text` (and closed). The
/// caller is responsible for waiting on / killing the child.
#[cfg(feature = "sidecars")]
pub async fn spawn_speak(text: &str) -> Result<Child> {
    if text.trim().is_empty() {
        anyhow::bail!("TTS: empty text");
    }

    #[cfg(target_os = "macos")]
    let mut cmd = {
        // `say` reads stdin when no positional text is given.
        let mut c = Command::new("say");
        c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        c
    };

    #[cfg(target_os = "linux")]
    let mut cmd = {
        // Prefer speech-dispatcher (`spd-say`) when present, else espeak/espeak-ng.
        // Both accept stdin.
        let bin = which_first(&["spd-say", "espeak-ng", "espeak"])
            .ok_or_else(|| anyhow::anyhow!("TTS: no native synth found (install spd-say or espeak)"))?;
        let mut c = Command::new(&bin);
        if bin.ends_with("spd-say") {
            // spd-say reads positional text or stdin via `-e`.
            c.arg("-e");
        } else {
            // espeak / espeak-ng: --stdin
            c.arg("--stdin");
        }
        c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
        c
    };

    #[cfg(target_os = "windows")]
    let mut cmd = {
        // PowerShell SAPI synthesizer reads from stdin. Single-quote-safe
        // because we never embed the text into the script string.
        let script = "\
            $reader = New-Object System.IO.StreamReader([Console]::OpenStandardInput()); \
            $text = $reader.ReadToEnd(); \
            Add-Type -AssemblyName System.Speech; \
            (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak($text)\
        ";
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        c
    };

    // Android/iOS used to land here on two `echo` stubs that spawned a
    // process to discard the text — speech that never spoke. They are gone
    // rather than ported: `sidecars` implies `desktop`, which no mobile
    // build enables, so this whole function is absent there and
    // `speak_in_process` above is the only path. Real mobile speech is
    // CrispASR + the webview, same as `desktop-mas`.

    let mut child = cmd.spawn().context("TTS: failed to spawn native synth")?;

    if let Some(mut stdin) = child.stdin.take() {
        let bytes = text.as_bytes().to_vec();
        // Write+close the stdin half on a separate task so the spawn
        // call returns immediately. The synth then runs to completion
        // (or gets killed) without us holding the writer lock.
        tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        });
    }

    Ok(child)
}

#[cfg(all(feature = "sidecars", target_os = "linux"))]
fn which_first(candidates: &[&str]) -> Option<String> {
    use std::path::Path;
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in candidates {
            let p = Path::new(&dir).join(name);
            if p.is_file() {
                return Some(p.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Convenience: kill an in-flight speak child, ignoring `NotFound`-ish
/// errors that mean the process already exited cleanly.
#[cfg(feature = "sidecars")]
pub async fn kill_quietly(child: &mut Child) {
    // Try graceful first via SIGTERM (best-effort on Windows where
    // tokio maps it to Process Termination).
    let _ = child.kill().await;
    let _ = child.wait().await;
}
