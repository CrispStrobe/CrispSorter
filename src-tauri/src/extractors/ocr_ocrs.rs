//! Pure-Rust OCR via the `ocrs` crate (PLAN P7.8 Tier 2).
//!
//! `ocrs` ships a custom CRAFT-style detector + recognizer pair
//! exported to RTen's tensor format (`.rten`). RTen is the project's
//! own ONNX-superset runtime — pure Rust, no system onnxruntime, no
//! C++ libs to bundle. The whole tier adds maybe 10-20 MB to the
//! binary's compiled-in code, plus the user-downloaded model files
//! (~10 MB detector + ~25 MB recognizer).
//!
//! Compared to Tier 1 (Tesseract shell-out) this trades a system
//! install requirement for a one-time model download. Compared to
//! Tier 3 (`usls` PaddleOCR via `ort`) it's lower-quality but
//! self-contained — no PaddleOCR model downloads in the gigabyte
//! range, no separate ONNX runtime.
//!
//! ## Limitations
//!
//! * **Latin script only.** ocrs doesn't ship Cyrillic / CJK /
//!   Arabic recognizers as of the model series we target. German
//!   users get usable but lower-quality results than Tier 1
//!   (Tesseract `eng+deu`); Tier 3 (PaddleOCR multilingual) is the
//!   right next step for those users.
//! * **Image inputs only.** No native PDF rendering — the PDF arm
//!   in the dispatcher still routes to Tesseract (which handles PDFs
//!   via Poppler). Adding PDF rendering would mean depending on
//!   `pdfium-render` (~10 MB pdfium DLL bundled per platform), which
//!   we'd want to do once, not just for ocrs.
//!
//! ## Model resolution
//!
//! Per-process `OnceLock` cache so the engine is loaded once on
//! first OCR call (~100ms cold, fractions of a ms warm). Resolution
//! order:
//!
//! 1. `CRISPSORTER_OCRS_MODEL_DIR` env var
//! 2. `<data_dir>/models/ocrs/` (default — same data_dir the rest
//!    of the index uses)
//!
//! Files expected under that dir:
//! * `text-detection.rten`
//! * `text-recognition.rten`
//!
//! Auto-download from <https://github.com/robertknight/ocrs-models>
//! is a future enhancement; for now an absent model file results in
//! a clear error message that the dispatcher swallows and falls
//! through to the next tier.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::ExtractedDocument;

/// Lazy-loaded `OcrEngine` keyed by the model dir we resolved to.
/// First successful load wins for the lifetime of the process; if a
/// later call would resolve to a different path (env var changed at
/// runtime) we ignore it — restart to pick up the change. Keeps the
/// per-call cost down to "Mutex lock + invoke pipeline".
static ENGINE: OnceLock<std::sync::Mutex<Option<ocrs::OcrEngine>>> = OnceLock::new();

fn engine_slot() -> &'static std::sync::Mutex<Option<ocrs::OcrEngine>> {
    ENGINE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Resolve the directory containing `text-detection.rten` and
/// `text-recognition.rten`. Env var beats default; default is
/// resolved at call time so a Settings-driven UI override can flow
/// through later by setting the env var before invoking.
fn resolve_model_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CRISPSORTER_OCRS_MODEL_DIR") {
        return PathBuf::from(d);
    }
    // Mirror the existing index resolve_model_cache_dir's default
    // shape; keep ocrs models alongside other model caches so the
    // user's "external SSD with all models" pattern just works.
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library/Application Support/com.<user>.crispsorter/models/ocrs");
    }
    PathBuf::from("./ocrs-models")
}

/// Load both `.rten` model files from `dir` and construct an
/// `OcrEngine`. Returns a clear error if either file is missing so
/// the dispatcher can fall through to Tier 1 (Tesseract).
fn load_engine(dir: &Path) -> Result<ocrs::OcrEngine> {
    let det_path = dir.join("text-detection.rten");
    let rec_path = dir.join("text-recognition.rten");
    if !det_path.exists() {
        return Err(anyhow!(
            "ocrs detection model not found at {} — download from \
             https://github.com/robertknight/ocrs-models/releases or \
             set CRISPSORTER_OCRS_MODEL_DIR",
            det_path.display()
        ));
    }
    if !rec_path.exists() {
        return Err(anyhow!(
            "ocrs recognition model not found at {} — download from \
             https://github.com/robertknight/ocrs-models/releases or \
             set CRISPSORTER_OCRS_MODEL_DIR",
            rec_path.display()
        ));
    }
    let detection = rten::Model::load_file(&det_path)
        .with_context(|| format!("loading {}", det_path.display()))?;
    let recognition = rten::Model::load_file(&rec_path)
        .with_context(|| format!("loading {}", rec_path.display()))?;
    let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
        detection_model: Some(detection),
        recognition_model: Some(recognition),
        ..Default::default()
    })
    .map_err(|e| anyhow!("constructing OcrEngine: {e}"))?;
    Ok(engine)
}

/// OCR `path` (an image file) via ocrs. Loads the engine once per
/// process; subsequent calls reuse the cached instance.
///
/// Returns the recognized text joined by newlines. ocrs' line
/// detection orders by reading flow, so the result is suitable for
/// chunker / embedder input as-is.
pub fn ocr_via_ocrs(path: &Path) -> Result<ExtractedDocument> {
    // Load + cache the engine on first successful call.
    let dir = resolve_model_dir();
    let mut slot = engine_slot()
        .lock()
        .map_err(|e| anyhow!("ocrs engine mutex poisoned: {e}"))?;
    if slot.is_none() {
        *slot = Some(load_engine(&dir)?);
    }
    let engine = slot.as_ref().unwrap();

    // Load image to RGB8.
    let img = image::open(path)
        .with_context(|| format!("opening image {}", path.display()))?
        .into_rgb8();
    let (w, h) = img.dimensions();
    let img_source = ocrs::ImageSource::from_bytes(img.as_raw(), (w, h))
        .map_err(|e| anyhow!("ocrs ImageSource::from_bytes: {e}"))?;
    let ocr_input = engine
        .prepare_input(img_source)
        .map_err(|e| anyhow!("ocrs prepare_input: {e}"))?;

    // Detect → group → recognize.
    let word_rects = engine
        .detect_words(&ocr_input)
        .map_err(|e| anyhow!("ocrs detect_words: {e}"))?;
    let line_rects = engine.find_text_lines(&ocr_input, &word_rects);
    let line_texts = engine
        .recognize_text(&ocr_input, &line_rects)
        .map_err(|e| anyhow!("ocrs recognize_text: {e}"))?;

    // Flatten Vec<Option<TextLine>> into a newline-joined string.
    let mut full_text = String::new();
    for line in line_texts.into_iter().flatten() {
        let s = line.to_string();
        if s.trim().is_empty() {
            continue;
        }
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(&s);
    }

    Ok(ExtractedDocument {
        full_text,
        // ocrs doesn't surface heading structure (it only knows about
        // lines) — leaving empty mirrors Tier 1 + the markdown text
        // extractor's behaviour for non-marked-up content.
        headings: Vec::new(),
        ext: String::new(), // dispatcher fills
        language: None,     // post-LID hook fills
        translated_text: None,
        translated_to_lang: None,
    })
}

/// Cheap availability probe — true if both model files exist where
/// we'd resolve them. Used by the dispatcher to skip ocrs without
/// trying to load the engine when the user hasn't downloaded the
/// models yet (saves a useless error log line per attempted file).
pub fn is_ocrs_available() -> bool {
    let dir = resolve_model_dir();
    dir.join("text-detection.rten").exists()
        && dir.join("text-recognition.rten").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check that resolution code paths don't panic on
    /// machines without the env var or HOME set.
    #[test]
    fn resolve_model_dir_returns_a_path() {
        let p = resolve_model_dir();
        assert!(p.as_os_str().len() > 0);
    }

    /// Availability probe never panics, regardless of whether models
    /// are actually present on the test runner.
    #[test]
    fn availability_probe_runs() {
        let _ = is_ocrs_available();
    }
}
