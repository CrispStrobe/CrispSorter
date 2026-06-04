//! Native text-to-speech bridge.
//!
//! v1 ships zero-dep platform synth — macOS `say`, Windows PowerShell
//! SAPI, Linux `spd-say` or `espeak` fallback. A GGUF Piper / Kokoro
//! sidecar would slot into the same `Tts::speak(text)` contract without
//! changing the frontend.
//!
//! Text is piped via stdin where the underlying tool supports it
//! (avoids argv-quoting headaches for arbitrary chat content).
//! The running child process lives in `AppState.tts_process` so a
//! `tts_stop` invocation can kill it mid-utterance — matches the
//! Stop button pattern used elsewhere in the app.

use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

/// Platform-specific synth invocation. Returns a spawned, running
/// child whose stdin has already been fed `text` (and closed). The
/// caller is responsible for waiting on / killing the child.
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

    // On Android/iOS, native TTS is accessed via platform APIs (JNI /
    // objc2), not subprocess spawning.  For now, echo the text to
    // /dev/null — the real implementation will call Android's
    // TextToSpeech or iOS's AVSpeechSynthesizer via the mobile_fs
    // bridge.  TODO: wire native mobile TTS.
    #[cfg(target_os = "android")]
    let mut cmd = {
        let mut c = Command::new("echo");
        c.arg(text).stdout(Stdio::null()).stderr(Stdio::null());
        c
    };

    #[cfg(target_os = "ios")]
    let mut cmd = {
        let mut c = Command::new("echo");
        c.arg(text).stdout(Stdio::null()).stderr(Stdio::null());
        c
    };

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

#[cfg(target_os = "linux")]
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
pub async fn kill_quietly(child: &mut Child) {
    // Try graceful first via SIGTERM (best-effort on Windows where
    // tokio maps it to Process Termination).
    let _ = child.kill().await;
    let _ = child.wait().await;
}
