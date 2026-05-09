//! PLAN P7.8 Tier 3 — PaddleOCR text detection + recognition via `usls`.
//!
//! Two-stage pipeline:
//!   1. **DB** (text detection) — `ppocr_det_v4_ch` finds text-region polygons.
//!   2. **SVTR** (text recognition) — `ppocr_rec_v4_en` reads each cropped region.
//!
//! Models auto-download from HuggingFace on first use (~50 MB det + ~10 MB rec).
//! Results for the English/German use case; swap to `ppocr_rec_v4_ch` / `v5`
//! variants for CJK or multilingual documents.
//!
//! Gated behind the `paddle-ocr` Cargo feature to keep the default binary free
//! of the ndarray / fast_image_resize / aksr compilation overhead.

use anyhow::{Context, Result};
use std::path::Path;

use super::ExtractedDocument;

/// True when the `paddle-ocr` feature is compiled in.
pub fn is_paddle_ocr_available() -> bool {
    #[cfg(feature = "paddle-ocr")]
    return true;
    #[cfg(not(feature = "paddle-ocr"))]
    return false;
}

/// Run the DB+SVTR PaddleOCR pipeline on a single image file.
///
/// On the first call for a given model variant, models are downloaded from
/// HuggingFace into the usls cache directory (typically `~/.cache/usls/`).
/// The function is CPU-only by default; pass `use_coreml = true` on Apple
/// Silicon to accelerate via CoreML.
#[cfg(feature = "paddle-ocr")]
pub fn ocr_via_paddle(path: &Path) -> Result<ExtractedDocument> {
    use usls::{models::Model, Config, Device, Image};
    use usls::models::vision::{DB, SVTR};

    // ── Text detection ─────────────────────────────────────────────────────
    let det_config = Config::ppocr_det_v4_ch()
        .with_device_all(Device::Cpu(0))
        .with_num_dry_run_all(0)
        .commit()
        .context("loading PaddleOCR detection model")?;
    let mut detector = DB::new(det_config)
        .context("initialising DB text detector")?;

    // ── Text recognition ───────────────────────────────────────────────────
    let rec_config = Config::ppocr_rec_v4_en()
        .with_device_all(Device::Cpu(0))
        .with_num_dry_run_all(0)
        .commit()
        .context("loading PaddleOCR recognition model")?;
    let mut recogniser = SVTR::new(rec_config)
        .context("initialising SVTR text recogniser")?;

    // ── Load image ─────────────────────────────────────────────────────────
    let img = Image::try_read(path)
        .with_context(|| format!("loading image for OCR: {}", path.display()))?;
    let images = vec![img];

    // ── Detection ──────────────────────────────────────────────────────────
    let det_results = detector
        .forward(&images)
        .context("DB text detection failed")?;

    // Collect (y_centre, crop) pairs so we can sort by reading order.
    let mut region_pairs: Vec<(f32, Image)> = Vec::new();

    for (src_img, det_y) in images.iter().zip(det_results.iter()) {
        if det_y.polygons.is_empty() {
            continue;
        }

        // DB returns polygons; convert to axis-aligned bounding boxes.
        let hbbs: Vec<_> = det_y.polygons.iter().filter_map(|p| p.hbb()).collect();
        if hbbs.is_empty() {
            continue;
        }
        let y_centres: Vec<f32> = hbbs.iter().map(|h| (h.ymin() + h.ymax()) / 2.0).collect();

        let crops = src_img.crop(&hbbs).context("cropping text regions")?;
        for (yc, crop) in y_centres.into_iter().zip(crops.into_iter()) {
            region_pairs.push((yc, crop));
        }
    }

    if region_pairs.is_empty() {
        return Ok(ExtractedDocument {
            full_text: String::new(),
            headings: vec![],
            ext: extension_of(path),
        });
    }

    // Sort top-to-bottom for reading order.
    region_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let sorted_crops: Vec<Image> = region_pairs.into_iter().map(|(_, img)| img).collect();

    // ── Recognition ────────────────────────────────────────────────────────
    let rec_results = recogniser
        .forward(&sorted_crops)
        .context("SVTR text recognition failed")?;

    let mut lines: Vec<String> = Vec::new();
    for rec_y in &rec_results {
        for text in &rec_y.texts {
            let s = text.text().trim().to_owned();
            if !s.is_empty() {
                lines.push(s);
            }
        }
    }

    Ok(ExtractedDocument {
        full_text: lines.join("\n"),
        headings: vec![],
        ext: extension_of(path),
    })
}

#[cfg(feature = "paddle-ocr")]
fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Stub for non-paddle-ocr builds — always returns an error.
#[cfg(not(feature = "paddle-ocr"))]
pub fn ocr_via_paddle(_path: &Path) -> Result<ExtractedDocument> {
    anyhow::bail!("PaddleOCR Tier 3 is not compiled in (build with --features paddle-ocr)");
}
