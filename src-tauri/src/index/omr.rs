//! Optical Mark Recognition — checkmark / checkbox detection (P27.8).
//!
//! Detects filled checkboxes, radio buttons, and bubble marks in scanned
//! forms via classical CV: crop the candidate region, convert to
//! grayscale, adaptive threshold (Otsu-like), count dark pixels / total
//! pixels → `fill_ratio`.  If `fill_ratio > threshold`, mark as filled.
//!
//! No external CV dep — uses the `image` crate already in deps.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default fill-ratio threshold for checkbox detection.
/// A typical empty checkbox has fill_ratio ~0.02–0.05 (just the border).
/// A filled checkbox has fill_ratio ~0.15–0.60 depending on pen thickness.
pub const DEFAULT_THRESHOLD: f64 = 0.15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckmarkResult {
    pub label: String,
    pub filled: bool,
    pub fill_ratio: f64,
    /// Confidence: how far the fill_ratio is from the threshold (0.0–1.0).
    pub confidence: f64,
}

/// Detect whether a checkbox region in an image is filled.
///
/// `x, y, w, h` are normalised 0.0–1.0 coordinates (fraction of image
/// width/height), matching the template zone convention.
pub fn detect_checkmark(
    image_path: &Path,
    label: &str,
    x: f64, y: f64, w: f64, h: f64,
    threshold: f64,
) -> Result<CheckmarkResult> {
    use image::GenericImageView;

    let img = image::open(image_path)
        .with_context(|| format!("opening image {}", image_path.display()))?;
    let (img_w, img_h) = img.dimensions();

    // Denormalise to pixel coordinates
    let px_x = (x * img_w as f64).round() as u32;
    let px_y = (y * img_h as f64).round() as u32;
    let px_w = (w * img_w as f64).round().max(1.0) as u32;
    let px_h = (h * img_h as f64).round().max(1.0) as u32;

    // Bounds check
    if px_x >= img_w || px_y >= img_h {
        return Ok(CheckmarkResult {
            label: label.to_string(),
            filled: false,
            fill_ratio: 0.0,
            confidence: 0.0,
        });
    }

    let clamped_w = px_w.min(img_w - px_x);
    let clamped_h = px_h.min(img_h - px_y);

    // Crop and convert to grayscale
    let crop = img.crop_imm(px_x, px_y, clamped_w, clamped_h);
    let gray = crop.to_luma8();

    // Compute Otsu threshold for binarisation
    let otsu = otsu_threshold(&gray);

    // Count dark pixels (below Otsu threshold)
    let total = gray.len() as f64;
    if total == 0.0 {
        return Ok(CheckmarkResult {
            label: label.to_string(),
            filled: false,
            fill_ratio: 0.0,
            confidence: 0.0,
        });
    }
    // Use <= for the threshold comparison: pixels AT the Otsu threshold
    // are considered dark (handles uniform-black images where otsu == 0).
    let dark = gray.iter().filter(|&&p| p <= otsu).count() as f64;
    let fill_ratio = dark / total;

    let filled = fill_ratio > threshold;
    // Confidence: how far from the decision boundary (saturate at 1.0)
    let confidence = ((fill_ratio - threshold).abs() / threshold).min(1.0);

    Ok(CheckmarkResult {
        label: label.to_string(),
        filled,
        fill_ratio,
        confidence,
    })
}

/// Batch variant: detect checkmarks for multiple zones.
pub fn detect_checkmarks(
    image_path: &Path,
    zones: &[(String, f64, f64, f64, f64)], // (label, x, y, w, h)
    threshold: f64,
) -> Result<Vec<CheckmarkResult>> {
    zones.iter()
        .map(|(label, x, y, w, h)| detect_checkmark(image_path, label, *x, *y, *w, *h, threshold))
        .collect()
}

/// Otsu's method for automatic threshold selection.
/// Returns the optimal grayscale threshold that minimises intra-class variance.
fn otsu_threshold(gray: &image::GrayImage) -> u8 {
    let mut histogram = [0u64; 256];
    for &p in gray.iter() {
        histogram[p as usize] += 1;
    }

    let total = gray.len() as f64;
    let mut sum_total = 0.0f64;
    for (i, &count) in histogram.iter().enumerate() {
        sum_total += i as f64 * count as f64;
    }

    let mut sum_bg = 0.0f64;
    let mut weight_bg = 0.0f64;
    let mut max_variance = 0.0f64;
    let mut best_threshold = 0u8;

    for (t, &count) in histogram.iter().enumerate() {
        weight_bg += count as f64;
        if weight_bg == 0.0 { continue; }
        let weight_fg = total - weight_bg;
        if weight_fg == 0.0 { break; }

        sum_bg += t as f64 * count as f64;
        let mean_bg = sum_bg / weight_bg;
        let mean_fg = (sum_total - sum_bg) / weight_fg;

        let variance = weight_bg * weight_fg * (mean_bg - mean_fg).powi(2);
        if variance > max_variance {
            max_variance = variance;
            best_threshold = t as u8;
        }
    }

    best_threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_image(width: u32, height: u32, fill: u8) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.png");
        let img = image::GrayImage::from_fn(width, height, |_, _| image::Luma([fill]));
        img.save(&path).unwrap();
        dir
    }

    #[test]
    fn white_image_not_filled() {
        let dir = make_image(50, 50, 255); // all white
        let result = detect_checkmark(
            &dir.path().join("test.png"), "check1",
            0.0, 0.0, 1.0, 1.0, DEFAULT_THRESHOLD,
        ).unwrap();
        assert!(!result.filled, "white image should not be filled: {}", result.fill_ratio);
        assert!(result.fill_ratio < 0.01);
    }

    #[test]
    fn black_image_is_filled() {
        let dir = make_image(50, 50, 0); // all black
        let result = detect_checkmark(
            &dir.path().join("test.png"), "check2",
            0.0, 0.0, 1.0, 1.0, DEFAULT_THRESHOLD,
        ).unwrap();
        assert!(result.filled, "black image should be filled: {}", result.fill_ratio);
        assert!(result.fill_ratio > 0.9);
    }

    #[test]
    fn partial_fill() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.png");
        // Top half black, bottom half white → ~50% fill
        let img = image::GrayImage::from_fn(50, 50, |_, y| {
            if y < 25 { image::Luma([0]) } else { image::Luma([255]) }
        });
        img.save(&path).unwrap();

        let result = detect_checkmark(
            &path, "partial", 0.0, 0.0, 1.0, 1.0, DEFAULT_THRESHOLD,
        ).unwrap();
        assert!(result.filled);
        assert!(result.fill_ratio > 0.3 && result.fill_ratio < 0.7,
            "expected ~50% fill, got {}", result.fill_ratio);
    }

    #[test]
    fn out_of_bounds_zone() {
        let dir = make_image(50, 50, 255);
        let result = detect_checkmark(
            &dir.path().join("test.png"), "oob",
            2.0, 2.0, 0.5, 0.5, DEFAULT_THRESHOLD,
        ).unwrap();
        assert!(!result.filled);
        assert_eq!(result.fill_ratio, 0.0);
    }

    #[test]
    fn batch_detect() {
        let dir = make_image(100, 100, 0); // all black
        let zones = vec![
            ("a".into(), 0.0, 0.0, 0.5, 0.5),
            ("b".into(), 0.5, 0.5, 0.5, 0.5),
        ];
        let results = detect_checkmarks(&dir.path().join("test.png"), &zones, DEFAULT_THRESHOLD).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].filled);
        assert!(results[1].filled);
    }

    #[test]
    fn nonexistent_image_errors() {
        let result = detect_checkmark(
            Path::new("/nonexistent.png"), "x",
            0.0, 0.0, 1.0, 1.0, DEFAULT_THRESHOLD,
        );
        assert!(result.is_err());
    }

    #[test]
    fn otsu_threshold_pure_white() {
        let img = image::GrayImage::from_fn(10, 10, |_, _| image::Luma([255]));
        let t = otsu_threshold(&img);
        // For a uniform image, threshold should be 0 or the only value
        assert!(t == 0 || t == 255);
    }

    #[test]
    fn otsu_threshold_bimodal() {
        // Gaussian-ish bimodal: half the pixels around 50, half around 200
        let img = image::GrayImage::from_fn(100, 100, |x, _| {
            if x < 50 { image::Luma([50]) } else { image::Luma([200]) }
        });
        let t = otsu_threshold(&img);
        // Otsu should pick a threshold between the two modes
        assert!(t >= 50 && t <= 200, "bimodal Otsu should be between 50 and 200, got {t}");
    }
}
