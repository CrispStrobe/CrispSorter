//! Zoned OCR extraction engine (P26.4).
//!
//! Given an image file and a `Template` (list of named zones with
//! normalised coordinates), crops each zone from the image, runs OCR
//! on the crop, and returns structured `{label, text}` pairs.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::templates::Template;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneResult {
    pub label: String,
    pub text: String,
    /// OCR confidence (0.0–1.0) when available, else -1.0.
    pub confidence: f32,
}

/// Extract text from each zone of a template applied to an image.
///
/// Zones with coordinates outside the image bounds are soft-failed
/// (empty text, confidence -1.0) — not panicked.
pub fn extract_zones(
    image_path: &Path,
    template: &Template,
) -> Result<Vec<ZoneResult>> {
    use image::GenericImageView;

    if template.zones.is_empty() {
        return Ok(vec![]);
    }

    let img = image::open(image_path)
        .with_context(|| format!("opening image {}", image_path.display()))?;
    let (img_w, img_h) = img.dimensions();

    let mut results = Vec::with_capacity(template.zones.len());

    for zone in &template.zones {
        // Denormalise from 0.0–1.0 to pixel coordinates
        let px_x = (zone.x * img_w as f64).round() as u32;
        let px_y = (zone.y * img_h as f64).round() as u32;
        let px_w = (zone.w * img_w as f64).round().max(1.0) as u32;
        let px_h = (zone.h * img_h as f64).round().max(1.0) as u32;

        // Bounds check — soft-fail if zone falls outside image
        if px_x >= img_w || px_y >= img_h {
            results.push(ZoneResult {
                label: zone.label.clone(),
                text: String::new(),
                confidence: -1.0,
            });
            continue;
        }

        // Clamp to image bounds
        let clamped_w = px_w.min(img_w - px_x);
        let clamped_h = px_h.min(img_h - px_y);

        // Crop and save to temp file
        let crop = img.crop_imm(px_x, px_y, clamped_w, clamped_h);
        let tmp = tempfile::Builder::new()
            .suffix(".png")
            .tempfile()
            .context("creating temp file for zone crop")?;
        let tmp_path = tmp.path().to_path_buf();
        crop.save(&tmp_path)
            .with_context(|| format!("saving zone crop for '{}'", zone.label))?;

        // Dispatch by zone type: "checkbox" → OMR, else → OCR
        if zone.zone_type == "checkbox" {
            let omr = super::omr::detect_checkmark(
                image_path, &zone.label,
                zone.x, zone.y, zone.w, zone.h,
                super::omr::DEFAULT_THRESHOLD,
            ).unwrap_or_else(|_| super::omr::CheckmarkResult {
                label: zone.label.clone(),
                filled: false,
                fill_ratio: 0.0,
                confidence: 0.0,
            });
            results.push(ZoneResult {
                label: omr.label,
                text: if omr.filled { "true".into() } else { "false".into() },
                confidence: omr.confidence as f32,
            });
            continue;
        }

        // OCR the crop
        let text = ocr_crop(&tmp_path);

        results.push(ZoneResult {
            label: zone.label.clone(),
            text,
            confidence: -1.0, // individual zone confidence not available from tier-1 OCR
        });
    }

    Ok(results)
}

/// OCR a single cropped image file. Returns the extracted text.
/// Uses the existing extractor dispatch (tesseract shell-out as
/// tier 1, CrispEmbed pipeline when available via the standard
/// ocr_one_image path). Returns empty string on failure.
fn ocr_crop(path: &Path) -> String {
    // Use tesseract shell-out — the simplest always-available OCR.
    // CrispEmbed pipeline integration is a follow-up (requires
    // threading the OcrPipelineConfig through, which is wired at
    // the bg_ingest level but not exposed as a standalone call).
    match crate::extractors::ocr::ocr_via_tesseract(path) {
        Ok(doc) if !doc.full_text.trim().is_empty() => doc.full_text,
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::templates::Zone;

    fn dummy_template(zones: Vec<Zone>) -> Template {
        Template {
            id: 1,
            name: "test".into(),
            width: 100,
            height: 100,
            zones,
        }
    }

    #[test]
    fn empty_template_returns_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        // Create a tiny 10x10 white image
        let img_path = dir.path().join("test.png");
        let img = image::RgbImage::from_fn(10, 10, |_, _| image::Rgb([255u8, 255, 255]));
        img.save(&img_path).unwrap();

        let t = dummy_template(vec![]);
        let results = extract_zones(&img_path, &t).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn out_of_bounds_zone_soft_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let img_path = dir.path().join("test.png");
        let img = image::RgbImage::from_fn(10, 10, |_, _| image::Rgb([255u8, 255, 255]));
        img.save(&img_path).unwrap();

        let t = dummy_template(vec![
            Zone { id: 1, label: "oob".into(), x: 2.0, y: 2.0, w: 0.5, h: 0.5, zone_type: "text".into() },
        ]);
        let results = extract_zones(&img_path, &t).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label, "oob");
        assert!(results[0].text.is_empty());
        assert!(results[0].confidence < 0.0);
    }

    #[test]
    fn zone_clamped_to_image_bounds() {
        let dir = tempfile::TempDir::new().unwrap();
        let img_path = dir.path().join("test.png");
        let img = image::RgbImage::from_fn(100, 100, |_, _| image::Rgb([255u8, 255, 255]));
        img.save(&img_path).unwrap();

        // Zone that extends past the right edge
        let t = dummy_template(vec![
            Zone { id: 1, label: "edge".into(), x: 0.8, y: 0.0, w: 0.5, h: 0.1, zone_type: "text".into() },
        ]);
        // Should not panic — zone is clamped
        let results = extract_zones(&img_path, &t).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label, "edge");
    }

    #[test]
    fn nonexistent_image_returns_error() {
        let t = dummy_template(vec![
            Zone { id: 1, label: "x".into(), x: 0.0, y: 0.0, w: 1.0, h: 1.0, zone_type: "text".into() },
        ]);
        assert!(extract_zones(Path::new("/nonexistent/img.png"), &t).is_err());
    }
}
