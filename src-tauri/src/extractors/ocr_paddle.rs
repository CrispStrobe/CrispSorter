//! PLAN P7.8 Tier 3 — PaddleOCR text detection + recognition via `usls`.
//!
//! Two-stage pipeline:
//!   1. **DB** (text detection) — `ppocr_det_v4_ch` finds text-region polygons.
//!   2. **SVTR** (text recognition) — model selected per `OcrRecLang`:
//!      - `Latin` (EN, DE, …) → `ppocr_rec_v4_en`
//!      - `Cjk` (Chinese, Japanese, Korean) → `ppocr_rec_v4_ch`
//!      - `Auto` → heuristic from path (CJK codepoints in filename/parent → ch model)
//!
//! Models auto-download from HuggingFace on first use (~50 MB det + ~10 MB rec).
//! Gated behind the `paddle-ocr` Cargo feature.

use anyhow::{Context, Result};
use std::path::Path;

use super::{ExtractedDocument, OcrRecLang};

/// True when the `paddle-ocr` feature is compiled in.
pub fn is_paddle_ocr_available() -> bool {
    #[cfg(feature = "paddle-ocr")]
    return true;
    #[cfg(not(feature = "paddle-ocr"))]
    return false;
}

/// Heuristic: does the path contain CJK Unicode characters?
/// Used when `OcrRecLang::Auto` — if the filename or parent directory
/// has a significant proportion of CJK codepoints we prefer the CH model.
fn path_looks_cjk(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let total = s.chars().count().max(1);
    let cjk = s.chars().filter(|c| is_cjk(*c)).count();
    cjk * 5 > total // >20% CJK codepoints
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x4E00..=0x9FFF |  // CJK Unified Ideographs
        0x3400..=0x4DBF |  // CJK Extension A
        0x20000..=0x2A6DF| // CJK Extension B
        0x3000..=0x303F |  // CJK Symbols
        0x3040..=0x309F |  // Hiragana
        0x30A0..=0x30FF |  // Katakana
        0xAC00..=0xD7AF    // Hangul
    )
}

/// Run the DB+SVTR PaddleOCR pipeline on a single image file.
///
/// `rec_lang` controls which recognition model is used:
/// - `Latin` → `ppocr_rec_v4_en` (EN/DE/FR/…)
/// - `Cjk`   → `ppocr_rec_v4_ch` (Chinese/Japanese/Korean)
/// - `Auto`  → guess from path heuristic (CJK codepoints → ch, otherwise en)
#[cfg(feature = "paddle-ocr")]
pub fn ocr_via_paddle(path: &Path, rec_lang: OcrRecLang) -> Result<ExtractedDocument> {
    use usls::{models::Model, Config, Device, Image};
    use usls::models::vision::{DB, SVTR};

    // Resolve effective language.
    let use_cjk = match rec_lang {
        OcrRecLang::Cjk   => true,
        OcrRecLang::Latin => false,
        OcrRecLang::Auto  => path_looks_cjk(path),
    };

    // ── Text detection ─────────────────────────────────────────────────────
    let det_config = Config::ppocr_det_v4_ch()
        .with_device_all(Device::Cpu(0))
        .with_num_dry_run_all(0)
        .commit()
        .context("loading PaddleOCR detection model")?;
    let mut detector = DB::new(det_config)
        .context("initialising DB text detector")?;

    // ── Text recognition (model depends on script) ─────────────────────────
    let rec_config = if use_cjk {
        Config::ppocr_rec_v4_ch()
    } else {
        Config::ppocr_rec_v4_en()
    };
    let rec_config = rec_config
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

    let mut region_pairs: Vec<(f32, Image)> = Vec::new();
    for (src_img, det_y) in images.iter().zip(det_results.iter()) {
        if det_y.polygons.is_empty() { continue; }
        let hbbs: Vec<_> = det_y.polygons.iter().filter_map(|p| p.hbb()).collect();
        if hbbs.is_empty() { continue; }
        let y_centres: Vec<f32> = hbbs.iter().map(|h| (h.ymin() + h.ymax()) / 2.0).collect();
        let crops = src_img.crop(&hbbs).context("cropping text regions")?;
        for (yc, crop) in y_centres.into_iter().zip(crops.into_iter()) {
            region_pairs.push((yc, crop));
        }
    }

    if region_pairs.is_empty() {
        return Ok(ExtractedDocument { full_text: String::new(), headings: vec![], ext: extension_of(path), language: None, translated_text: None, translated_to_lang: None });
    }

    region_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let sorted_crops: Vec<Image> = region_pairs.into_iter().map(|(_, img)| img).collect();

    // ── Recognition ────────────────────────────────────────────────────────
    let rec_results = recogniser.forward(&sorted_crops).context("SVTR text recognition failed")?;

    let mut lines: Vec<String> = Vec::new();
    for rec_y in &rec_results {
        for text in &rec_y.texts {
            let s = text.text().trim().to_owned();
            if !s.is_empty() { lines.push(s); }
        }
    }

    Ok(ExtractedDocument { full_text: lines.join("\n"), headings: vec![], ext: extension_of(path), language: None, translated_text: None, translated_to_lang: None })
}

#[cfg(feature = "paddle-ocr")]
fn extension_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).unwrap_or_default()
}

/// Stub for non-paddle-ocr builds.
#[cfg(not(feature = "paddle-ocr"))]
pub fn ocr_via_paddle(_path: &Path, _rec_lang: OcrRecLang) -> Result<ExtractedDocument> {
    anyhow::bail!("PaddleOCR Tier 3 is not compiled in (build with --features paddle-ocr)");
}
