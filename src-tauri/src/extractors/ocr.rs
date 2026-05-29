//! OCR backend — Tesseract via shell-out.
//!
//! Phase 7.8 of PLAN P7. For images and scanned PDFs (where the text
//! layer is empty / negligible), the regular extractors return nothing
//! useful. OCR fills that gap for users who actually have scanned
//! material to index.
//!
//! ## Why shell-out, not `leptess` / native bindings
//!
//! `leptess` requires Tesseract + Leptonica linked at build time, which
//! complicates cross-compile (CrispASR-style native deps), bundling
//! (the .app would have to ship libtesseract + libleptonica + every
//! language's traineddata file), and packaging (license headers from
//! a half-dozen GPL/Apache-mixed deps). A shell-out to `/usr/local/bin/
//! tesseract` keeps Cargo.toml unchanged, lets the user pick when to
//! install (Homebrew / apt / chocolatey / not at all), and trivially
//! handles the "OCR is opt-in" UX.
//!
//! For Apple Silicon Macs, the native Vision framework
//! (`VNRecognizeTextRequest`) is materially better — higher quality,
//! GPU-accelerated, no install. Shipping a tiny Swift sidecar is the
//! natural follow-up; the public API here stays the same so the
//! call-sites don't move.
//!
//! ## What this module does NOT do
//!
//! * **PDF rendering.** Tesseract reads PDFs only via `pdftoppm`
//!   (Poppler). Calling `tesseract input.pdf - -l eng` works on most
//!   Linux distros where Poppler is installed alongside; macOS via
//!   Homebrew picks it up automatically; Windows installs vary. We
//!   pass the path through and let Tesseract decide. Users with
//!   broken Tesseract+Poppler installs will see a clear stderr error.
//!
//! * **Language detection.** The first cut hard-codes `eng+deu` (the
//!   project's primary languages — match the Settings i18n). Future:
//!   read languages from the catalog's metadata or from a Settings
//!   knob, append to the `-l` arg.
//!
//! * **Page-level OCR for PDFs.** Tesseract returns one big text dump
//!   per file by default. That's fine for indexing (chunker re-splits
//!   later); for per-page metadata we'd need PDF rendering + per-page
//!   OCR + re-aggregation, which is too much for this phase.

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

use super::ExtractedDocument;

/// Run Tesseract on `path`, capture stdout as the extracted text.
/// Returns the raw OCR output without further processing — chunking /
/// heading detection / language tagging happen downstream as for any
/// other extractor.
///
/// Hardcodes `eng+deu` as the language pack hint. Tesseract is
/// surprisingly good at multi-language documents when given the right
/// hint; CrispSorter's primary use case (German + English academic
/// PDFs) lines up exactly.
pub fn ocr_via_tesseract(path: &Path) -> Result<ExtractedDocument> {
    // `tesseract <input> - -l eng+deu` writes plain text to stdout.
    // The `-` output target is the magic stdout sentinel.
    let out = Command::new("tesseract")
        .arg(path)
        .arg("-") // stdout
        .arg("-l")
        .arg("eng+deu")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!(
                "failed to spawn `tesseract` for {} — is it installed and on PATH?",
                path.display()
            )
        })?;
    if !out.status.success() {
        // Tesseract writes diagnostics to stderr; surface the first
        // 200 bytes so the caller can diagnose missing language packs,
        // unsupported input, etc.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let snippet: String = stderr.chars().take(200).collect();
        return Err(anyhow!(
            "tesseract exited {}: {snippet}",
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into())
        ));
    }
    let text = String::from_utf8(out.stdout).context("tesseract stdout not utf-8")?;
    Ok(ExtractedDocument {
        full_text: text,
        headings: Vec::new(),
        ext: String::new(), // dispatcher fills
        language: None,     // post-LID hook fills
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url: None,
    })
}

/// Cheap availability check — does `tesseract --version` exit 0?
/// Cached lookup is overkill for the call frequency we have; just
/// invoke it on demand.
pub fn is_tesseract_installed() -> bool {
    Command::new("tesseract")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Skip-on-missing test that doubles as an "OCR was wired in"
    /// smoke check. CI runners may or may not have Tesseract; a
    /// passing test on a runner without it is fine — the real
    /// signal is the call-site compiles.
    #[test]
    fn version_check_runs() {
        // Just make sure the call doesn't panic. The bool result is
        // environment-dependent.
        let _ = is_tesseract_installed();
    }
}
