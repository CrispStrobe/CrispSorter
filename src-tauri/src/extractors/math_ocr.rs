//! P17.3 — Math formula OCR via CrispEmbed.
//!
//! Recognizes printed and handwritten mathematical formulas in images,
//! producing LaTeX strings.  Works standalone or integrated with layout
//! detection (P17.1): detect formula regions → crop → recognize each.
//!
//! Supported engines (auto-detected from GGUF metadata):
//! - PP-FormulaNet-L (printed, 181M params, BLEU 0.90)
//! - PosFormer (handwritten, DenseNet + Transformer + ARM)
//! - BTTR (handwritten, DenseNet + Transformer)
//! - HMER (handwritten, DenseNet + GRU attention)
//! - DeiT+TrOCR (printed, lightweight 17 MB Q4_K)
//! - MixTex (Chinese+English LaTeX)
//! - Qwen2.5-VL (German support)
//!
//! Gated behind `--features crispembed`.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Mutex;

/// Default math OCR model — PP-FormulaNet-L for printed formulas.
const DEFAULT_MATH_MODEL: &str = "ppformulanet-l";

/// Process-global lazy-loaded math OCR engine.
#[cfg(feature = "crispembed")]
static MATH_OCR: std::sync::OnceLock<Mutex<crispembed::MathOcr>> = std::sync::OnceLock::new();

/// Check if math OCR is available at runtime.
pub fn is_math_ocr_available() -> bool {
    cfg!(feature = "crispembed")
}

/// Recognize a math formula from an image file, returning LaTeX.
///
/// Loads the image, converts to RGB, and passes to the MathOcr engine.
/// Returns `None` if the engine fails to recognize anything (not an error
/// — some images just don't contain math).
#[cfg(feature = "crispembed")]
pub fn recognize_formula(image_path: &Path) -> Result<Option<String>> {
    let img = image::open(image_path)
        .with_context(|| format!("failed to open image: {}", image_path.display()))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    let pixels = img.into_raw();

    let engine = MATH_OCR.get_or_init(|| {
        let resolved = crispembed::CrispEmbed::resolve_model(DEFAULT_MATH_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_MATH_MODEL.to_string());
        let ocr = crispembed::MathOcr::new(&resolved, 0)
            .expect("MathOcr model init failed");
        Mutex::new(ocr)
    });

    let mut guard = engine
        .lock()
        .map_err(|e| anyhow::anyhow!("MathOcr lock poisoned: {e}"))?;

    Ok(guard.recognize(&pixels, w as i32, h as i32))
}

/// Recognize a math formula from a raw RGB pixel buffer.
#[cfg(feature = "crispembed")]
pub fn recognize_formula_from_pixels(
    pixels: &[u8],
    width: i32,
    height: i32,
) -> Result<Option<String>> {
    let engine = MATH_OCR.get_or_init(|| {
        let resolved = crispembed::CrispEmbed::resolve_model(DEFAULT_MATH_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_MATH_MODEL.to_string());
        let ocr = crispembed::MathOcr::new(&resolved, 0)
            .expect("MathOcr model init failed");
        Mutex::new(ocr)
    });

    let mut guard = engine
        .lock()
        .map_err(|e| anyhow::anyhow!("MathOcr lock poisoned: {e}"))?;

    Ok(guard.recognize(pixels, width, height))
}

/// Recognize math formulas with a custom model path.
#[cfg(feature = "crispembed")]
pub fn recognize_formula_with_model(
    image_path: &Path,
    model_path: &str,
) -> Result<Option<String>> {
    let img = image::open(image_path)
        .with_context(|| format!("failed to open image: {}", image_path.display()))?
        .to_rgb8();
    let (w, h) = img.dimensions();
    let pixels = img.into_raw();

    let resolved = crispembed::CrispEmbed::resolve_model(model_path, Some(true))
        .unwrap_or_else(|_| model_path.to_string());
    let mut ocr = crispembed::MathOcr::new(&resolved, 0)
        .map_err(|e| anyhow::anyhow!("MathOcr init failed: {e}"))?;

    Ok(ocr.recognize(&pixels, w as i32, h as i32))
}

/// Given layout regions from P17.1, crop formula regions from the page
/// image and recognize each, returning `Vec<(region_index, latex)>`.
#[cfg(feature = "crispembed")]
pub fn recognize_formulas_in_layout(
    image_path: &Path,
    regions: &[super::layout::LayoutRegion],
) -> Result<Vec<(usize, String)>> {
    use super::layout::RegionKind;

    let img = image::open(image_path)
        .with_context(|| format!("failed to open image: {}", image_path.display()))?
        .to_rgb8();
    let (img_w, img_h) = img.dimensions();

    let engine = MATH_OCR.get_or_init(|| {
        let resolved = crispembed::CrispEmbed::resolve_model(DEFAULT_MATH_MODEL, Some(true))
            .unwrap_or_else(|_| DEFAULT_MATH_MODEL.to_string());
        let ocr = crispembed::MathOcr::new(&resolved, 0)
            .expect("MathOcr model init failed");
        Mutex::new(ocr)
    });

    let mut guard = engine
        .lock()
        .map_err(|e| anyhow::anyhow!("MathOcr lock poisoned: {e}"))?;

    let mut results = Vec::new();
    for (i, region) in regions.iter().enumerate() {
        if !region.kind.is_formula() {
            continue;
        }
        // Clamp bounding box to image dimensions.
        let x1 = (region.x1.max(0.0) as u32).min(img_w.saturating_sub(1));
        let y1 = (region.y1.max(0.0) as u32).min(img_h.saturating_sub(1));
        let x2 = (region.x2.max(0.0) as u32).min(img_w);
        let y2 = (region.y2.max(0.0) as u32).min(img_h);
        let w = x2.saturating_sub(x1);
        let h = y2.saturating_sub(y1);
        if w < 4 || h < 4 {
            continue; // Too small to be a formula.
        }
        let crop = image::imageops::crop_imm(&img, x1, y1, w, h).to_image();
        let pixels = crop.into_raw();
        if let Some(latex) = guard.recognize(&pixels, w as i32, h as i32) {
            if !latex.trim().is_empty() {
                results.push((i, latex));
            }
        }
    }
    Ok(results)
}

// ── Stubs when crispembed is not compiled ───────────────────────────

#[cfg(not(feature = "crispembed"))]
pub fn recognize_formula(_image_path: &Path) -> Result<Option<String>> {
    Err(anyhow::anyhow!(
        "math OCR requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn recognize_formula_from_pixels(
    _pixels: &[u8],
    _width: i32,
    _height: i32,
) -> Result<Option<String>> {
    Err(anyhow::anyhow!(
        "math OCR requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn recognize_formula_with_model(
    _image_path: &Path,
    _model_path: &str,
) -> Result<Option<String>> {
    Err(anyhow::anyhow!(
        "math OCR requires --features crispembed"
    ))
}

#[cfg(not(feature = "crispembed"))]
pub fn recognize_formulas_in_layout(
    _image_path: &Path,
    _regions: &[super::layout::LayoutRegion],
) -> Result<Vec<(usize, String)>> {
    Err(anyhow::anyhow!(
        "math OCR requires --features crispembed"
    ))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_available_matches_feature() {
        let available = is_math_ocr_available();
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
        let p = tmp.path().join("formula.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        assert!(recognize_formula(&p).is_err());
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_from_pixels_returns_error_without_feature() {
        assert!(recognize_formula_from_pixels(&[0u8; 12], 2, 2).is_err());
    }

    #[cfg(not(feature = "crispembed"))]
    #[test]
    fn stub_layout_returns_error_without_feature() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("page.png");
        std::fs::write(&p, b"\x89PNG").unwrap();
        assert!(recognize_formulas_in_layout(&p, &[]).is_err());
    }

    // ── Live tests ──────────────────────────────────────────────────

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed math_ocr_live -- --ignored
    fn math_ocr_live() {
        // Create a white test image.
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("formula.png");
        let img = image::RgbImage::new(100, 50);
        img.save(&img_path).unwrap();
        let result = recognize_formula(&img_path);
        match result {
            Ok(maybe_latex) => {
                println!("Math OCR result: {:?}", maybe_latex);
            }
            Err(e) => {
                // Model may not be downloaded yet — that's OK for a live test.
                println!("Math OCR error (may be expected): {e}");
            }
        }
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn math_ocr_from_pixels_live() {
        // 2×2 white RGB image.
        let pixels = vec![255u8; 2 * 2 * 3];
        let result = recognize_formula_from_pixels(&pixels, 2, 2);
        match result {
            Ok(maybe_latex) => println!("Pixel-based OCR: {:?}", maybe_latex),
            Err(e) => println!("Pixel OCR error (may be expected): {e}"),
        }
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn math_ocr_layout_integration_live() {
        use super::super::layout::{LayoutRegion, RegionKind};

        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("page.png");
        // Create a 400×400 white image.
        let img = image::RgbImage::new(400, 400);
        img.save(&img_path).unwrap();

        let regions = vec![LayoutRegion {
            x1: 50.0,
            y1: 50.0,
            x2: 350.0,
            y2: 150.0,
            score: 0.9,
            kind: RegionKind::Formula,
            label_name: "formula".into(),
        }];
        let result = recognize_formulas_in_layout(&img_path, &regions);
        match result {
            Ok(formulas) => println!("Found {} formulas in layout", formulas.len()),
            Err(e) => println!("Layout math OCR error (may be expected): {e}"),
        }
    }
}
