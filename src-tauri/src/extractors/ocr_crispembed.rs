//! P17.2 — CrispEmbed GGUF OCR (Tier 4).
//!
//! Wraps `crispembed::OcrPipeline` (DBNet text detection + TrOCR/Surya/
//! Qwen2.5-VL recognition) as the highest-priority OCR tier.  All models
//! are GGUF — no ORT or Tesseract dependency.
//!
//! The pipeline auto-downloads models from HuggingFace on first use via
//! `CrispEmbed::resolve_model`.  Detection and recognition models are
//! specified by registry name; the default pairing is `surya-det` (91
//! languages) + `qwen2vl-ocr` (German support).
//!
//! Gated behind `--features crispembed`.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Mutex;

use super::ExtractedDocument;

/// Default detection model (Surya-OCR-2, EfficientViT, 91 languages).
const DEFAULT_DET_MODEL: &str = "surya-det";
/// Default recognition model (Qwen2.5-VL, German support).
const DEFAULT_REC_MODEL: &str = "qwen2vl-ocr";

/// Process-global lazy-loaded OCR pipeline.  The CrispEmbed OcrPipeline
/// is not `Sync` so we wrap it in a `Mutex`.  This trades concurrency for
/// zero-overhead repeated calls (model stays loaded across documents).
#[cfg(feature = "crispembed")]
static OCR_PIPELINE: std::sync::OnceLock<Mutex<crispembed::OcrPipeline>> =
    std::sync::OnceLock::new();

/// Check if CrispEmbed OCR is available at runtime.
pub fn is_crispembed_ocr_available() -> bool {
    cfg!(feature = "crispembed")
}

/// Run CrispEmbed OCR on an image file.
///
/// Returns an `ExtractedDocument` with the concatenated recognized text
/// (reading order: top-to-bottom, left-to-right by region centroid).
/// Empty text on zero detections is not an error — some images genuinely
/// have no text.
#[cfg(feature = "crispembed")]
pub fn ocr_via_crispembed(path: &Path) -> Result<ExtractedDocument> {
    let path_str = path
        .to_str()
        .context("image path is not valid UTF-8")?;

    let pipeline = OCR_PIPELINE.get_or_init(|| {
        let det = crispembed::CrispEmbed::resolve_model(DEFAULT_DET_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_DET_MODEL.to_string());
        let rec = crispembed::CrispEmbed::resolve_model(DEFAULT_REC_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_REC_MODEL.to_string());
        let p = crispembed::OcrPipeline::new(&det, &rec, 0)
            .expect("CrispEmbed OCR pipeline init failed");
        Mutex::new(p)
    });

    let mut guard = pipeline
        .lock()
        .map_err(|e| anyhow::anyhow!("OCR pipeline lock poisoned: {e}"))?;

    let results = guard.run(path_str);

    // Sort regions in reading order (top-to-bottom, left-to-right).
    let mut sorted = results;
    sorted.sort_by(|a, b| {
        let ay = a.y + a.h / 2.0;
        let by = b.y + b.h / 2.0;
        ay.partial_cmp(&by)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.x.partial_cmp(&b.x)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let full_text = sorted
        .iter()
        .map(|r| r.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    Ok(ExtractedDocument {
        full_text,
        headings: Vec::new(),
        ext: path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase(),
        language: None,
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url: None,
        tags: vec![],
    })
}

/// Stub when crispembed is not compiled.
#[cfg(not(feature = "crispembed"))]
pub fn ocr_via_crispembed(path: &Path) -> Result<ExtractedDocument> {
    Err(anyhow::anyhow!(
        "CrispEmbed OCR requires --features crispembed; skipped {}",
        path.display()
    ))
}

/// Run CrispEmbed OCR with custom detection + recognition models.
///
/// For advanced users who want to pick specific model variants
/// (e.g. `dbnet-det` + `trocr-rec` for a lightweight pipeline).
#[cfg(feature = "crispembed")]
pub fn ocr_via_crispembed_custom(
    path: &Path,
    det_model: &str,
    rec_model: &str,
) -> Result<ExtractedDocument> {
    let path_str = path
        .to_str()
        .context("image path is not valid UTF-8")?;

    let det = crispembed::CrispEmbed::resolve_model(det_model, Some(true))
        .unwrap_or_else(|_| det_model.to_string());
    let rec = crispembed::CrispEmbed::resolve_model(rec_model, Some(true))
        .unwrap_or_else(|_| rec_model.to_string());

    let mut pipeline = crispembed::OcrPipeline::new(&det, &rec, 0)
        .map_err(|e| anyhow::anyhow!("CrispEmbed OCR init failed: {e}"))?;

    let results = pipeline.run(path_str);

    let mut sorted = results;
    sorted.sort_by(|a, b| {
        let ay = a.y + a.h / 2.0;
        let by = b.y + b.h / 2.0;
        ay.partial_cmp(&by)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.x.partial_cmp(&b.x)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let full_text = sorted
        .iter()
        .map(|r| r.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    Ok(ExtractedDocument {
        full_text,
        headings: Vec::new(),
        ext: path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase(),
        language: None,
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url: None,
        tags: vec![],
    })
}

#[cfg(not(feature = "crispembed"))]
pub fn ocr_via_crispembed_custom(
    path: &Path,
    _det_model: &str,
    _rec_model: &str,
) -> Result<ExtractedDocument> {
    Err(anyhow::anyhow!(
        "CrispEmbed OCR requires --features crispembed; skipped {}",
        path.display()
    ))
}

/// NAFNet denoise GGUF registry name (`cstr/nafnet-sidd-GGUF`, MIT, ~30 MB).
#[cfg(feature = "crispembed")]
const NAFNET_MODEL: &str = "nafnet-denoise";

/// Process-global lazy-loaded OCR pipeline orchestrator (cleanup + routing +
/// accept-gate). Like [`OCR_PIPELINE`], cached behind a `Mutex` (not `Sync`)
/// so the models stay loaded across documents. The first call's config wins
/// for the process; changing OCR-pipeline settings takes effect on index
/// re-init (same lazy-once contract as the embedder/reranker).
#[cfg(feature = "crispembed")]
static OCR_ORCH: std::sync::OnceLock<Mutex<crispembed::CrispOcrPipeline>> =
    std::sync::OnceLock::new();

/// Run the C++ OCR pipeline orchestrator (source-type routing + per-stage
/// cleanup + NAFNet denoise + accept-gate escalation) on an image.
#[cfg(feature = "crispembed")]
pub fn ocr_via_pipeline(
    path: &Path,
    cfg: &super::OcrPipelineConfig,
) -> Result<ExtractedDocument> {
    let path_str = path.to_str().context("image path is not valid UTF-8")?;

    let orch = OCR_ORCH.get_or_init(|| {
        let det_name = cfg.det_model.as_deref().unwrap_or(DEFAULT_DET_MODEL);
        let rec_name = cfg.rec_model.as_deref().unwrap_or(DEFAULT_REC_MODEL);
        let det = crispembed::CrispEmbed::resolve_model(det_name, Some(true))
            .unwrap_or_else(|_| det_name.to_string());
        let rec = crispembed::CrispEmbed::resolve_model(rec_name, Some(true))
            .unwrap_or_else(|_| rec_name.to_string());
        // Resolve NAFNet only when tier-2 denoise is requested.
        let nafnet: Option<String> = if cfg.denoise {
            crispembed::CrispEmbed::resolve_model(NAFNET_MODEL, Some(true)).ok()
        } else {
            None
        };
        let p = crispembed::CrispOcrPipeline::new(
            &det,
            &rec,
            nafnet.as_deref(),
            cfg.router,
            cfg.cleanup_enabled,
            cfg.min_chars,
            cfg.min_confidence,
            0,
        )
        .expect("CrispEmbed OCR pipeline (orchestrator) init failed");
        Mutex::new(p)
    });

    let mut guard = orch
        .lock()
        .map_err(|e| anyhow::anyhow!("OCR pipeline lock poisoned: {e}"))?;
    let res = guard
        .run(path_str)
        .map_err(|e| anyhow::anyhow!("CrispEmbed OCR pipeline run failed: {e}"))?;

    Ok(ExtractedDocument {
        full_text: res.full_text,
        headings: Vec::new(),
        ext: path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase(),
        language: None,
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url: None,
        tags: vec![],
    })
}

#[cfg(not(feature = "crispembed"))]
pub fn ocr_via_pipeline(
    path: &Path,
    _cfg: &super::OcrPipelineConfig,
) -> Result<ExtractedDocument> {
    Err(anyhow::anyhow!(
        "CrispEmbed OCR pipeline requires --features crispembed; skipped {}",
        path.display()
    ))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_available_matches_feature() {
        let available = is_crispembed_ocr_available();
        if cfg!(feature = "crispembed") {
            assert!(available);
        } else {
            assert!(!available);
        }
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_returns_error_without_feature() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("test.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        let err = ocr_via_crispembed(&p);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("requires --features crispembed"));
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn custom_stub_returns_error_without_feature() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("test.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        let err = ocr_via_crispembed_custom(&p, "det", "rec");
        assert!(err.is_err());
    }

    // ── Live tests (require models on disk) ─────────────────────────

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed ocr_crispembed_live -- --ignored
    fn ocr_crispembed_live() {
        // Create a test image with text rendered.  For a real test,
        // use an actual document image.  Here we just verify the
        // pipeline loads and runs without crashing.
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("test.png");
        let img = image::RgbImage::new(200, 200);
        img.save(&img_path).unwrap();
        let result = ocr_via_crispembed(&img_path);
        // Blank image → Ok with empty or minimal text.
        let doc = result.expect("pipeline should not crash on blank image");
        println!("OCR text length: {}", doc.full_text.len());
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn ocr_crispembed_custom_models_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("test.png");
        let img = image::RgbImage::new(200, 200);
        img.save(&img_path).unwrap();
        // Use the default models explicitly.
        let result = ocr_via_crispembed_custom(&img_path, "surya-det", "qwen2vl-ocr");
        let doc = result.expect("custom pipeline should not crash");
        println!("OCR text length: {}", doc.full_text.len());
    }
}
