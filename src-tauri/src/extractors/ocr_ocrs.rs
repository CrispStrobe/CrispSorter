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
//! Per-process cache so the engine is loaded once (keyed by the model
//! files). Resolution order: `CRISPSORTER_OCRS_MODEL_DIR` (override, `.rten`
//! or `.onnx`), then the shared model cache where the OCR model manager
//! installs the pinned upstream ONNX models (`ocr_builtin_models`), then the
//! legacy hand-placed `.../models/ocrs` folder. A missing model returns the
//! `MODEL_DOWNLOAD_REQUIRED:` error so the app can offer the download.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::ExtractedDocument;

static ENGINE: OnceLock<std::sync::Mutex<Option<super::context_cache::CachedContext<ocrs::OcrEngine>>>> = OnceLock::new();

/// The pre-manager folder for hand-placed `.rten` files. Env var beats
/// default. Still searched, after the shared cache, so existing setups work.
fn resolve_model_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CRISPSORTER_OCRS_MODEL_DIR") {
        return PathBuf::from(d);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library/Application Support/com.crispstrobe.crispsorter/models/ocrs");
    }
    PathBuf::from("./ocrs-models")
}

/// (detection, recognition) model files, in priority order:
/// 1. `CRISPSORTER_OCRS_MODEL_DIR` (explicit override, `.rten` or `.onnx`);
/// 2. the shared model cache, installed by the model manager
///    (`ocr_builtin_models`, pinned ONNX);
/// 3. the legacy hand-placed folder.
fn model_files() -> Option<(PathBuf, PathBuf)> {
    use crate::ocr_builtin_models as builtin;
    let pair_in = |dir: &Path| ["rten", "onnx"].iter().find_map(|ext| {
        let det = dir.join(format!("text-detection.{ext}"));
        let rec = dir.join(format!("text-recognition.{ext}"));
        (det.is_file() && rec.is_file()).then_some((det, rec))
    });
    if std::env::var_os("CRISPSORTER_OCRS_MODEL_DIR").is_some() {
        if let Some(pair) = pair_in(&resolve_model_dir()) { return Some(pair); }
    }
    if let (Some(det), Some(rec)) = (builtin::locate(builtin::OCRS_DET), builtin::locate(builtin::OCRS_REC)) {
        return Some((det[0].clone(), rec[0].clone()));
    }
    pair_in(&resolve_model_dir())
}

/// Load both model files (ONNX or RTen; `rten` detects the format) and
/// construct an `OcrEngine`.
fn load_engine(det_path: &Path, rec_path: &Path) -> Result<ocrs::OcrEngine> {
    let detection = rten::Model::load_file(det_path).context("loading the ocrs detection model")?;
    let recognition = rten::Model::load_file(rec_path).context("loading the ocrs recognition model")?;
    let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
        detection_model: Some(detection),
        recognition_model: Some(recognition),
        ..Default::default()
    })
    .map_err(|e| anyhow!("constructing OcrEngine: {e}"))?;
    Ok(engine)
}

/// Run ocrs on a single image file. Missing models are reported with the
/// `MODEL_DOWNLOAD_REQUIRED:` error the app turns into its download dialog;
/// nothing is downloaded here.
pub fn ocr_via_ocrs(path: &Path) -> Result<ExtractedDocument> {
    use crate::operation_log::OperationLog;
    use serde_json::json;
    let log = OperationLog::start("ocr.ocrs", json!({"engine": "ocrs", "kind": "image"}));
    let result = recognize_ocrs(path, &log);
    match &result {
        Ok((doc, lines, cache_hit)) => log.succeed(json!({"pages": 1, "regions": lines,
            "characters": doc.full_text.chars().count(), "cache_hit": cache_hit})),
        Err(error) => {
            let text = format!("{error:#}");
            log.fail(if text.contains(crate::ocr_models::MODEL_DOWNLOAD_REQUIRED) || text.contains("not installed") { "model_missing" }
                else if text.contains("loading the ocrs") || text.contains("OcrEngine") { "model_load_failed" }
                else if text.contains("opening image") { "image_unreadable" }
                else { "recognition_failed" });
        }
    }
    result.map(|(doc, _, _)| doc)
}

fn recognize_ocrs(path: &Path, log: &crate::operation_log::OperationLog) -> Result<(ExtractedDocument, usize, bool)> {
    use crate::ocr_builtin_models as builtin;
    use serde_json::json;
    log.step("checking", json!({}));
    let (det_path, rec_path) = match model_files() {
        Some(pair) => pair,
        None => {
            // Report every missing file at once, in the dialog's format.
            let missing: Vec<_> = [builtin::OCRS_DET, builtin::OCRS_REC].iter()
                .filter(|name| builtin::locate(name).is_none())
                .filter_map(|name| builtin::find(name).map(|m| crate::ocr_models::planned_builtin(m, false)))
                .collect();
            return Err(anyhow!("{}", crate::ocr_models::gate(&missing).err()
                .unwrap_or_else(|| "The ocrs models are not installed. Install them in Settings > OCR models or with `crispsorter models pull ocrs`.".into())));
        }
    };
    let key = format!("{}\n{}", det_path.display(), rec_path.display());
    let mut built = false;
    log.step("loading", json!({}));
    let slot = super::context_cache::acquire(&ENGINE, key, || {
        built = true;
        load_engine(&det_path, &rec_path).map_err(|e| format!("{e:#}"))
    }).map_err(anyhow::Error::msg)?;
    let engine = &slot.as_ref().context("ocrs engine was not initialized")?.context;
    log.step("recognizing", json!({"cache_hit": !built}));

    let img = image::open(path)
        .context("opening image for ocrs")?
        .into_rgb8();
    let (w, h) = img.dimensions();
    let img_source = ocrs::ImageSource::from_bytes(img.as_raw(), (w, h))
        .map_err(|e| anyhow!("ocrs ImageSource::from_bytes: {e}"))?;
    let ocr_input = engine
        .prepare_input(img_source)
        .map_err(|e| anyhow!("ocrs prepare_input: {e}"))?;

    let word_rects = engine
        .detect_words(&ocr_input)
        .map_err(|e| anyhow!("ocrs detect_words: {e}"))?;
    let line_rects = engine.find_text_lines(&ocr_input, &word_rects);
    let line_texts = engine
        .recognize_text(&ocr_input, &line_rects)
        .map_err(|e| anyhow!("ocrs recognize_text: {e}"))?;

    let mut full_text = String::new();
    let mut lines = 0usize;
    for line in line_texts.into_iter().flatten() {
        let s = line.to_string();
        if s.trim().is_empty() {
            continue;
        }
        if !full_text.is_empty() {
            full_text.push('\n');
        }
        full_text.push_str(&s);
        lines += 1;
    }

    Ok((ExtractedDocument {
        full_text,
        headings: Vec::new(),
        ext: String::new(), // dispatcher fills
        language: None,     // post-LID hook fills
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url: None,
        tags: vec![],
        audio_pcm: None,
        ..Default::default()
    }, lines, !built))
}

/// Both models are on disk (automatic tier selection uses this, so
/// Automatic never picks an engine that would first need a download).
pub fn is_ocrs_available() -> bool {
    model_files().is_some()
}

/// Offer ocrs as a selectable engine: always on desktop (the model manager
/// installs its models on first use, after asking); on mobile only when the
/// models are already on the device.
pub fn is_ocrs_offered() -> bool {
    !crate::model_provisioning::enabled() || is_ocrs_available()
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
        // Desktop offers ocrs even before its models are installed.
        if !crate::model_provisioning::enabled() { assert!(is_ocrs_offered()); }
    }

    #[test]
    fn missing_models_ask_for_the_pinned_downloads() {
        if is_ocrs_available() { return; }
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/quality/ocr-task.png");
        let (result, events) = crate::operation_log::capture(|| ocr_via_ocrs(&fixture));
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains(crate::ocr_models::MODEL_DOWNLOAD_REQUIRED) || error.contains("not installed"), "{error}");
        let last = events.last().unwrap();
        assert_eq!((last.operation.as_str(), last.code.as_deref()), ("ocr.ocrs", Some("model_missing")));
        assert!(!serde_json::to_string(&events).unwrap().contains("ocrs-models.s3"));
    }

    /// Live: installs the pinned ocrs ONNX models through the model manager
    /// and recognises the fixture. CI runs it with `--ignored`.
    #[test]
    #[ignore = "live: downloads the ocrs models"]
    fn live_ocrs_installs_and_recognises_the_fixture_sentence() {
        use crate::ocr_builtin_models as builtin;
        crate::ocr_models::install_models(&[builtin::OCRS_DET.into(), builtin::OCRS_REC.into()], None, &|_| {})
            .expect("install pinned ocrs models");
        assert!(is_ocrs_available());
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/quality/ocr-task.png");
        let text = ocr_via_ocrs(&fixture).expect("ocrs runs").full_text.to_lowercase();
        for word in ["cobalt", "warranty", "november", "2031"] {
            assert!(text.contains(word), "missing {word:?} in {text:?}");
        }
    }
}
