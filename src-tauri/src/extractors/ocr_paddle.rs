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

use anyhow::Result;
#[cfg(feature = "paddle-ocr")]
use anyhow::Context;
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
#[cfg(any(feature = "paddle-ocr", test))]
fn path_looks_cjk(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let total = s.chars().count().max(1);
    let cjk = s.chars().filter(|c| is_cjk(*c)).count();
    cjk * 5 > total // >20% CJK codepoints
}

#[cfg(any(feature = "paddle-ocr", test))]
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
        return Ok(ExtractedDocument { full_text: String::new(), headings: vec![], ext: extension_of(path), language: None, translated_text: None, translated_to_lang: None, audio: None, image_exif: None, source_url: None, tags: vec![] });
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

    Ok(ExtractedDocument { full_text: lines.join("\n"), headings: vec![], ext: extension_of(path), language: None, translated_text: None, translated_to_lang: None, audio: None, image_exif: None, source_url: None, tags: vec![] })
}

#[cfg(feature = "paddle-ocr")]
fn extension_of(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).unwrap_or_default()
}

/// Run SLANet table structure detection on a single image file.
///
/// Returns an HTML table string with cell bounding boxes.  The caller
/// can combine this with SVTR recognition results to populate cell
/// text.  Returns `None` if no table is detected.
#[cfg(feature = "paddle-ocr")]
pub fn detect_table_structure(path: &Path) -> Result<Option<String>> {
    use usls::{models::Model, Config, Device, Image};
    use usls::models::vision::SLANet;

    let config = Config::slanet_lcnet_v2_mobile_ch()
        .with_device_all(Device::Cpu(0))
        .with_num_dry_run_all(0)
        .commit()
        .context("loading SLANet table detection model")?;
    let mut slanet = SLANet::new(config)
        .context("initialising SLANet table detector")?;

    let img = Image::try_read(path)
        .with_context(|| format!("loading image for table detection: {}", path.display()))?;
    let images = vec![img];

    let results = slanet
        .forward(&images)
        .context("SLANet table detection failed")?;

    // SLANet returns HTML table tokens (<table>, <tr>, <td>, etc.)
    // and keypoints for cell bounding boxes.
    for y in &results {
        if y.texts.is_empty() {
            continue;
        }
        let html: String = y.texts.iter().map(|t| t.text().to_string()).collect::<Vec<_>>().join("");
        if html.contains("<table>") {
            return Ok(Some(html));
        }
    }

    Ok(None)
}

/// Run full OCR with table structure detection.
///
/// First runs DB+SVTR for text extraction, then SLANet for table
/// structure.  If a table is detected, the output includes the HTML
/// table structure appended after the plain text.
#[cfg(feature = "paddle-ocr")]
pub fn ocr_with_tables(path: &Path, rec_lang: OcrRecLang) -> Result<ExtractedDocument> {
    let mut doc = ocr_via_paddle(path, rec_lang)?;

    // Try table detection — non-fatal if it fails (some images
    // have text but no tables).
    match detect_table_structure(path) {
        Ok(Some(html_table)) => {
            if !doc.full_text.is_empty() {
                doc.full_text.push_str("\n\n");
            }
            doc.full_text.push_str("<!-- table structure -->\n");
            doc.full_text.push_str(&html_table);
        }
        Ok(None) => {} // No table detected
        Err(e) => {
            tracing::warn!("SLANet table detection failed (non-fatal): {e}");
        }
    }

    Ok(doc)
}

/// True when SLANet table detection is available.
pub fn is_slanet_available() -> bool {
    #[cfg(feature = "paddle-ocr")]
    return true;
    #[cfg(not(feature = "paddle-ocr"))]
    return false;
}

/// Stub for non-paddle-ocr builds.
#[cfg(not(feature = "paddle-ocr"))]
pub fn ocr_via_paddle(_path: &Path, _rec_lang: OcrRecLang) -> Result<ExtractedDocument> {
    anyhow::bail!("PaddleOCR Tier 3 is not compiled in (build with --features paddle-ocr)");
}

/// Stub for non-paddle-ocr builds.
#[cfg(not(feature = "paddle-ocr"))]
pub fn ocr_with_tables(_path: &Path, _rec_lang: OcrRecLang) -> Result<ExtractedDocument> {
    anyhow::bail!("PaddleOCR Tier 3 is not compiled in (build with --features paddle-ocr)");
}

/// Stub for non-paddle-ocr builds.
#[cfg(not(feature = "paddle-ocr"))]
pub fn detect_table_structure(_path: &Path) -> Result<Option<String>> {
    anyhow::bail!("PaddleOCR Tier 3 is not compiled in (build with --features paddle-ocr)");
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_cjk ────────────────────────────────────────────────────────────────

    #[test]
    fn is_cjk_unified_ideograph_accepted() {
        // U+4E00 is the first CJK Unified Ideograph ("一").
        assert!(is_cjk('\u{4E00}'));
    }

    #[test]
    fn is_cjk_hangul_accepted() {
        // U+AC00 is the first Hangul syllable block ("가").
        assert!(is_cjk('\u{AC00}'));
    }

    #[test]
    fn is_cjk_latin_rejected() {
        assert!(!is_cjk('a'));
    }

    // ── path_looks_cjk ────────────────────────────────────────────────────────

    #[test]
    fn path_looks_cjk_ascii_is_false() {
        assert!(!path_looks_cjk(Path::new("/tmp/hello.jpg")));
    }

    #[test]
    fn path_looks_cjk_majority_cjk_is_true() {
        // Filename is four CJK chars + ".jpg" — well over 20 % CJK.
        assert!(path_looks_cjk(Path::new("/tmp/\u{4E00}\u{4E01}\u{4E02}\u{4E03}.jpg")));
    }

    #[test]
    fn path_looks_cjk_boundary_exclusive() {
        // Exactly 1 CJK char in 5 total chars: 1 * 5 == 5, which is NOT > 5,
        // so the function must return false (strict greater-than boundary).
        // "\u{4E00}abcd" — 5 chars, 1 CJK.
        assert!(!path_looks_cjk(Path::new("\u{4E00}abcd")));
    }
}
