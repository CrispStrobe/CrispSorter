//! Bounded photo inspection and explicit, non-destructive native geometry.
//! Measurements are capture hints, never calibrated recognition confidence.

use image::{DynamicImage, GrayImage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct PhotoPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Serialize)]
pub struct PhotoAssessment {
    pub width: u32,
    pub height: u32,
    pub sample_width: u32,
    pub sample_height: u32,
    pub mean_luminance: f64,
    pub contrast: f64,
    pub sharpness: f64,
    pub ink_fraction: f64,
    /// Clockwise correction in degrees; absent for aligned/indeterminate pages.
    pub correction_degrees: Option<f64>,
    /// Zero means the detector has no calibrated confidence estimate.
    pub skew_confidence: f64,
    pub warnings: Vec<String>,
}

pub fn assess(image: &DynamicImage) -> Result<PhotoAssessment, String> {
    let sample = image.thumbnail(1000, 1000).to_luma8();
    let (mean_luminance, contrast, sharpness, ink_fraction) = measurements(&sample);
    let mut warnings = Vec::new();
    if image.width().min(image.height()) < 1000 {
        warnings.push("Small text may lack detail. Move closer while keeping every page edge visible.".into());
    }
    if mean_luminance < 100.0 {
        warnings.push("The photo is dark. Use even lighting and avoid casting a shadow across the page.".into());
    }
    if contrast < 35.0 {
        warnings.push("The capture has little tonal separation. Check the text at full size before recognizing.".into());
    }
    if sharpness < 45.0 {
        warnings.push("Fine edges are weak. Check focus and motion blur at full size; a blank page can also produce this hint.".into());
    }
    let correction_degrees = correction_angle(image)?;
    if correction_degrees.is_some() {
        warnings.push("Tilt is a geometric estimate. Inspect the corrected preview, and flatten the background if shadows remain.".into());
    } else {
        warnings.push("No clear tilt was measured. The page may be aligned, or contain too little text; inspect its baselines before using manual tilt.".into());
    }
    Ok(PhotoAssessment {
        width: image.width(), height: image.height(),
        sample_width: sample.width(), sample_height: sample.height(),
        mean_luminance, contrast, sharpness, ink_fraction,
        correction_degrees, skew_confidence: 0.0, warnings,
    })
}

fn measurements(sample: &GrayImage) -> (f64, f64, f64, f64) {
    let count = sample.as_raw().len().max(1) as f64;
    let mean = sample.as_raw().iter().map(|v| f64::from(*v)).sum::<f64>() / count;
    let mut histogram = [0usize; 256];
    for &value in sample.as_raw() { histogram[value as usize] += 1; }
    let quantile = |fraction: f64| {
        let target = (count * fraction).ceil() as usize;
        let mut cumulative = 0;
        for (value, frequency) in histogram.iter().enumerate() {
            cumulative += frequency;
            if cumulative >= target { return value as f64; }
        }
        255.0
    };
    // Text often occupies much less than 10% of a page; use the 1st percentile.
    let contrast = quantile(0.95) - quantile(0.01);
    let threshold = (mean - 45.0).clamp(30.0, 180.0);
    let ink = sample.as_raw().iter().filter(|v| f64::from(**v) < threshold).count() as f64 / count;
    let mut sum = 0.0;
    let mut squared = 0.0;
    let mut edges = 0usize;
    if sample.width() > 2 && sample.height() > 2 {
        for y in 1..sample.height() - 1 {
            for x in 1..sample.width() - 1 {
                let p = |x, y| f64::from(sample.get_pixel(x, y)[0]);
                let laplacian = p(x - 1, y) + p(x + 1, y) + p(x, y - 1) + p(x, y + 1) - 4.0 * p(x, y);
                sum += laplacian;
                squared += laplacian * laplacian;
                edges += 1;
            }
        }
    }
    let sharpness = if edges == 0 { 0.0 } else {
        (squared / edges as f64 - (sum / edges as f64).powi(2)).max(0.0)
    };
    (mean, contrast, sharpness, ink)
}

#[cfg(feature = "ocr-photo-tools")]
fn correction_angle(image: &DynamicImage) -> Result<Option<f64>, String> {
    let rgb = image.as_rgb8().map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| std::borrow::Cow::Owned(image.to_rgb8()));
    let angle = crispembed::CrispScanPhoto::correction_angle(
        rgb.as_raw(), rgb.width() as i32, rgb.height() as i32, 3, 20.0,
    ).map_err(|e| format!("Measure photo tilt: {e}"))?;
    if !angle.is_finite() { return Err("Photo tilt detector returned an invalid angle.".into()); }
    Ok((angle.abs() >= 0.05).then_some(f64::from(angle)))
}

#[cfg(not(feature = "ocr-photo-tools"))]
fn correction_angle(_image: &DynamicImage) -> Result<Option<f64>, String> { Ok(None) }

#[cfg(feature = "ocr-photo-tools")]
pub fn transform(image: &DynamicImage, operation: &str, quad: Option<Vec<PhotoPoint>>, angle: Option<f64>) -> Result<DynamicImage, String> {
    let rgb = image.as_rgb8().map(std::borrow::Cow::Borrowed)
        .unwrap_or_else(|| std::borrow::Cow::Owned(image.to_rgb8()));
    let output = match operation {
        "deskew" => {
            let angle = angle.ok_or("Choose a correction angle.")?;
            if !angle.is_finite() || !(-20.0..=20.0).contains(&angle) {
                return Err("Photo tilt must be a finite angle between -20 and 20 degrees.".into());
            }
            crispembed::CrispScanPhoto::rotate(rgb.as_raw(), rgb.width() as i32, rgb.height() as i32, 3, angle as f32)
        }
        "perspective" => {
            let points = quad.ok_or("Choose four page corners.")?;
            if points.len() != 4 || points.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()
                || p.x < 0.0 || p.y < 0.0 || p.x > f64::from(rgb.width() - 1) || p.y > f64::from(rgb.height() - 1)) {
                return Err("Choose four finite corners inside the page, clockwise from the top left.".into());
            }
            let corners: [[f32; 2]; 4] = std::array::from_fn(|i| [points[i].x as f32, points[i].y as f32]);
            crispembed::CrispScanPhoto::perspective(rgb.as_raw(), rgb.width() as i32, rgb.height() as i32, 3, &corners)
        }
        _ => return Err("Choose perspective correction or photo tilt.".into()),
    }.map_err(|e| format!("Correct photo: {e}"))?;
    let (pixels, width, height) = output;
    if width <= 0 || height <= 0 { return Err("Photo correction returned invalid dimensions.".into()); }
    image::RgbImage::from_raw(width as u32, height as u32, pixels).map(DynamicImage::ImageRgb8)
        .ok_or_else(|| "Photo correction returned an invalid pixel buffer.".into())
}

#[cfg(not(feature = "ocr-photo-tools"))]
pub fn transform(_image: &DynamicImage, _operation: &str, _quad: Option<Vec<PhotoPoint>>, _angle: Option<f64>) -> Result<DynamicImage, String> {
    Err("Photo geometry requires an OCR workspace build with its matching CrispEmbed photo library.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_and_tiny_measurements_are_finite_without_inventing_detail() {
        for size in [1, 2, 80] {
            let gray = GrayImage::from_pixel(size, size, image::Luma([230]));
            let (mean, contrast, sharpness, ink) = measurements(&gray);
            assert_eq!(mean, 230.0);
            assert_eq!((contrast, sharpness, ink), (0.0, 0.0, 0.0));
        }
    }

    #[test]
    fn focus_hint_distinguishes_edges_from_a_flat_capture() {
        let sharp = GrayImage::from_fn(80, 100, |x, y| image::Luma([if y % 15 < 3 && x > 10 && x < 70 { 30 } else { 240 }]));
        let blurred = image::imageops::blur(&sharp, 3.0);
        let a = measurements(&sharp);
        let b = measurements(&blurred);
        assert!(a.2 > b.2 * 10.0);
        assert!(a.1 > 100.0);
        assert!(a.3 > 0.05 && a.3 < 0.4);
    }
}
