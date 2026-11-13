//! PLAN P7.8 Tier 3 — PaddleOCR text detection + recognition via `usls`.
//!
//! Two-stage pipeline:
//!   1. **DB** (text detection) — `ppocr_det_v4_ch` finds text-region polygons.
//!   2. **SVTR** (text recognition) — model selected per `OcrRecLang`:
//!      - `Latin` (EN, DE, …) → `ppocr_rec_v4_en`
//!      - `Cjk` (Chinese, Japanese, Korean) → `ppocr_rec_v4_ch`
//!      - `Auto` → heuristic from path (CJK codepoints in filename/parent → ch model)
//!
//! Models are pinned and installed by the OCR model manager
//! (`ocr_builtin_models`: ~5 MB detection + ~8-11 MB recognition, plus
//! SLANet for tables) into the shared cache, after the user agreed. usls only
//! receives local files and never downloads. Loaded sessions are cached
//! across pages. Gated behind the `paddle-ocr` Cargo feature.

use anyhow::Result;
#[cfg(feature = "paddle-ocr")]
use anyhow::Context;
use std::path::Path;

use super::{ExtractedDocument, OcrRecLang};

#[cfg(any(feature = "paddle-ocr", test))]
#[path = "paddle_resize.rs"]
mod paddle_resize;

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

/// Resolve the automatic script before imports/crops replace the original
/// filename with an opaque temporary path.
pub fn paddle_language_for_name(name: &str) -> OcrRecLang {
    if path_looks_cjk(Path::new(name)) { OcrRecLang::Cjk } else { OcrRecLang::Latin }
}

/// Loaded PaddleOCR sessions, reused across pages and files. Each slot is
/// keyed by the exact model file it holds, so a reinstall reloads it and a
/// script switch only replaces the recognizer.
#[cfg(feature = "paddle-ocr")]
mod sessions {
    use super::super::context_cache::CachedContext;
    use std::sync::{Mutex, OnceLock};
    pub static DET: OnceLock<Mutex<Option<CachedContext<usls::Runtime<usls::models::vision::DB>>>>> = OnceLock::new();
    pub static REC: OnceLock<Mutex<Option<CachedContext<usls::Runtime<usls::models::vision::SVTR>>>>> = OnceLock::new();
    pub static TABLE: OnceLock<Mutex<Option<CachedContext<usls::Runtime<usls::models::vision::SLANet>>>>> = OnceLock::new();
}

/// Read a usls vocabulary (one class per line) from a local file.
#[cfg(feature = "paddle-ocr")]
fn read_vocabulary(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path).context("reading the PaddleOCR vocabulary")?;
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    anyhow::ensure!(!lines.is_empty(), "the PaddleOCR vocabulary is empty");
    Ok(lines)
}

#[cfg(feature = "paddle-ocr")]
fn path_str(path: &Path) -> Result<String> {
    path.to_str().map(str::to_owned).context("the PaddleOCR model path is not valid UTF-8")
}

/// Installed model files, or the `MODEL_DOWNLOAD_REQUIRED:` error the app
/// turns into its download dialog. usls is only ever given local files, so
/// it never reaches its own GitHub/Hugging Face hub.
#[cfg(feature = "paddle-ocr")]
fn installed(name: &str) -> Result<Vec<std::path::PathBuf>> {
    crate::ocr_builtin_models::require(name).map_err(anyhow::Error::msg)
}

/// Run the DB+SVTR PaddleOCR pipeline on a single image file.
///
/// `rec_lang` controls which recognition model is used:
/// - `Latin` → `ppocr_rec_v4_en` (EN/DE/FR/…)
/// - `Cjk`   → `ppocr_rec_v4_ch` (Chinese/Japanese/Korean)
/// - `Auto`  → guess from path heuristic (CJK codepoints → ch, otherwise en)
///
/// Models come from the shared model manager (`ocr_builtin_models`); a
/// missing model is reported, never downloaded here.
#[cfg(feature = "paddle-ocr")]
pub fn ocr_via_paddle(path: &Path, rec_lang: OcrRecLang) -> Result<ExtractedDocument> {
    use crate::operation_log::OperationLog;
    use serde_json::json;
    let use_cjk = match rec_lang {
        OcrRecLang::Cjk   => true,
        OcrRecLang::Latin => false,
        OcrRecLang::Auto  => path_looks_cjk(path),
    };
    let log = OperationLog::start("ocr.paddle", json!({"engine": "paddle", "kind": "image"}));
    let result = recognize_paddle(path, use_cjk, &log);
    match &result {
        Ok((doc, regions, cache_hit)) => log.succeed(json!({"pages": 1, "regions": regions,
            "characters": doc.full_text.chars().count(), "cache_hit": cache_hit})),
        Err(error) => {
            let text = format!("{error:#}");
            log.fail(if text.contains(crate::ocr_models::MODEL_DOWNLOAD_REQUIRED) { "model_missing" }
                else if text.contains("loading image") { "image_unreadable" }
                else if text.contains("initialising") || text.contains("loading PaddleOCR") { "model_load_failed" }
                else { "recognition_failed" });
        }
    }
    result.map(|(doc, _, _)| doc)
}

#[cfg(feature = "paddle-ocr")]
fn recognize_paddle(path: &Path, use_cjk: bool, log: &crate::operation_log::OperationLog) -> Result<(ExtractedDocument, usize, bool)> {
    use usls::{models::Model, Config, Device, Image};
    use usls::models::vision::{DB, SVTR};
    use serde_json::json;

    log.step("checking", json!({}));
    let det_files = installed(crate::ocr_builtin_models::PADDLE_DET)?;
    let rec_files = installed(crate::ocr_builtin_models::paddle_recognizer(use_cjk))?;
    let det_path = path_str(&det_files[0])?;
    let rec_path = path_str(&rec_files[0])?;

    let mut det_built = false;
    let mut rec_built = false;
    log.step("loading", json!({}));
    let mut detector = super::context_cache::acquire(&sessions::DET, det_path.clone(), || {
        det_built = true;
        let config = Config::db().with_model_file(det_path.as_str())
            .with_device_all(Device::Cpu(0)).with_num_dry_run_all(0)
            .commit().map_err(|e| format!("loading PaddleOCR detection model: {e:#}"))?;
        DB::new(config).map_err(|e| format!("initialising DB text detector: {e:#}"))
    }).map_err(anyhow::Error::msg)?;
    let mut recogniser = super::context_cache::acquire(&sessions::REC, rec_path.clone(), || {
        rec_built = true;
        let vocabulary = read_vocabulary(&rec_files[1]).map_err(|e| format!("loading PaddleOCR recognition model: {e:#}"))?;
        let config = Config::svtr().with_class_names_owned(vocabulary).with_model_file(rec_path.as_str())
            .with_device_all(Device::Cpu(0)).with_num_dry_run_all(0)
            .commit().map_err(|e| format!("loading PaddleOCR recognition model: {e:#}"))?;
        SVTR::new(config).map_err(|e| format!("initialising SVTR text recogniser: {e:#}"))
    }).map_err(anyhow::Error::msg)?;
    let cache_hit = !det_built && !rec_built;
    log.step("loaded", json!({"cache_hit": cache_hit}));
    let detector = &mut detector.as_mut().context("PaddleOCR detector was not initialised")?.context;
    let recogniser = &mut recogniser.as_mut().context("PaddleOCR recogniser was not initialised")?.context;

    // ── Load image ─────────────────────────────────────────────────────────
    let img = Image::try_read(path).context("loading image for OCR")?;
    let images = vec![img];

    // ── Detection ──────────────────────────────────────────────────────────
    log.step("recognizing", json!({}));
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
    let regions = region_pairs.len();

    if region_pairs.is_empty() {
        return Ok((ExtractedDocument { full_text: String::new(), headings: vec![], ext: extension_of(path), language: None, translated_text: None, translated_to_lang: None, audio: None, image_exif: None, source_url: None, tags: vec![], audio_pcm: None, ..Default::default() }, 0, cache_hit));
    }

    region_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // usls FitHeight does not cap the resized width to the SVTR model canvas.
    // Paddle clamps over-wide lines to that canvas. Rescale the full region
    // to the actual model dimensions before usls performs its height fit.
    let canvas_width = u32::try_from(recogniser.width).context("SVTR model width exceeds image dimensions")?;
    let canvas_height = u32::try_from(recogniser.height).context("SVTR model height exceeds image dimensions")?;
    let sorted_crops: Vec<Image> = region_pairs.into_iter().map(|(_, img)| {
        let (width, height) = img.dimensions();
        match paddle_resize::svtr_pre_resize(width, height, canvas_width, canvas_height)
            .map_err(anyhow::Error::msg)? {
            None => Ok(img),
            Some((width, height)) => {
                let alg = usls::ResizeAlg::default();
                let mode = usls::ResizeMode::FitExact { width, height, alg };
                img.resize_with_info(width, height, alg, &mode, 0)
                    .map(|(image, _)| image).context("fitting text region to SVTR model canvas")
            }
        }
    }).collect::<Result<_>>()?;

    // ── Recognition ────────────────────────────────────────────────────────
    let rec_results = recogniser.forward(&sorted_crops).context("SVTR text recognition failed")?;

    let mut lines: Vec<String> = Vec::new();
    for rec_y in &rec_results {
        for text in &rec_y.texts {
            let s = text.text().trim().to_owned();
            if !s.is_empty() { lines.push(s); }
        }
    }

    Ok((ExtractedDocument { full_text: lines.join("\n"), headings: vec![], ext: extension_of(path), language: None, translated_text: None, translated_to_lang: None, audio: None, image_exif: None, source_url: None, tags: vec![], audio_pcm: None, ..Default::default() }, regions, cache_hit))
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
    use crate::operation_log::OperationLog;
    use serde_json::json;

    let log = OperationLog::start("ocr.paddle", json!({"engine": "paddle", "kind": "table"}));
    let result = (|| -> Result<(Option<String>, bool)> {
        let files = installed(crate::ocr_builtin_models::PADDLE_SLANET)?;
        let model_path = path_str(&files[0])?;
        let mut built = false;
        let mut slanet = super::context_cache::acquire(&sessions::TABLE, model_path.clone(), || {
            built = true;
            let vocabulary = read_vocabulary(&files[1]).map_err(|e| format!("loading SLANet table detection model: {e:#}"))?;
            let config = Config::slanet().with_class_names_owned(vocabulary).with_model_file(model_path.as_str())
                .with_device_all(Device::Cpu(0)).with_num_dry_run_all(0)
                .commit().map_err(|e| format!("loading SLANet table detection model: {e:#}"))?;
            SLANet::new(config).map_err(|e| format!("initialising SLANet table detector: {e:#}"))
        }).map_err(anyhow::Error::msg)?;
        let slanet = &mut slanet.as_mut().context("SLANet was not initialised")?.context;
        let img = Image::try_read(path).context("loading image for table detection")?;
        let results = slanet.forward(&[img]).context("SLANet table detection failed")?;
        // SLANet returns HTML table tokens (<table>, <tr>, <td>, etc.)
        // and keypoints for cell bounding boxes.
        for y in &results {
            if y.texts.is_empty() { continue; }
            let html: String = y.texts.iter().map(|t| t.text().to_string()).collect::<Vec<_>>().join("");
            if html.contains("<table>") { return Ok((Some(html), !built)); }
        }
        Ok((None, !built))
    })();
    match &result {
        Ok((table, cache_hit)) => log.succeed(json!({"pages": 1, "results": usize::from(table.is_some()), "cache_hit": cache_hit})),
        Err(error) => log.fail(if format!("{error:#}").contains(crate::ocr_models::MODEL_DOWNLOAD_REQUIRED) { "model_missing" } else { "table_detection_failed" }),
    }
    result.map(|(table, _)| table)
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
            log::warn!("SLANet table detection failed (non-fatal): {e}");
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

    #[test]
    fn original_import_name_resolves_script_before_temporary_crop_paths() {
        assert_eq!(paddle_language_for_name("中文发票.png"), OcrRecLang::Cjk);
        assert_eq!(paddle_language_for_name("invoice.png"), OcrRecLang::Latin);
    }

    /// Live recognition: downloads the PP-OCRv4 detection + English recognition
    /// models (~60 MB) on first run. CI runs it explicitly with `--ignored`.
    #[cfg(feature = "paddle-ocr")]
    #[test]
    #[ignore = "live: downloads PaddleOCR models"]
    fn live_paddle_recognises_the_fixture_sentence() {
        use crate::ocr_builtin_models as builtin;
        // Through the model manager (pinned SHA-256), never usls' own hub.
        let names = [builtin::PADDLE_DET.to_string(), builtin::PADDLE_REC_EN.to_string(), builtin::PADDLE_SLANET.to_string()];
        crate::ocr_models::install_models(&names, None, &|_| {}).expect("install pinned PaddleOCR models");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/quality/ocr-task.png");
        let (doc, events) = crate::operation_log::capture(|| ocr_via_paddle(&fixture, OcrRecLang::Latin));
        let doc = doc.expect("paddle OCR runs");
        let text = doc.full_text.to_lowercase();
        for word in ["cobalt", "warranty", "november", "2031"] {
            assert!(text.contains(word), "missing {word:?} in {text:?}");
        }
        let done = events.iter().find(|e| e.operation == "ocr.paddle" && e.phase == "completed").expect("receipt");
        assert!(done.context.get("characters").and_then(|v| v.as_u64()).unwrap_or(0) > 20);
        // The second page reuses the loaded sessions.
        let (again, events) = crate::operation_log::capture(|| ocr_via_paddle(&fixture, OcrRecLang::Latin));
        assert_eq!(again.unwrap().full_text, doc.full_text);
        let done = events.iter().find(|e| e.operation == "ocr.paddle" && e.phase == "completed").unwrap();
        assert_eq!(done.context.get("cache_hit"), Some(&serde_json::Value::Bool(true)));
        // Table structure loads SLANet from the same cache.
        detect_table_structure(&fixture).expect("SLANet runs on a page without a table");
    }

    #[cfg(feature = "paddle-ocr")]
    #[test]
    fn missing_paddle_models_ask_instead_of_downloading() {
        use crate::ocr_builtin_models as builtin;
        if builtin::locate(builtin::PADDLE_REC_CH).is_some() { return; }
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/quality/ocr-task.png");
        let (result, events) = crate::operation_log::capture(|| ocr_via_paddle(&fixture, OcrRecLang::Cjk));
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains(crate::ocr_models::MODEL_DOWNLOAD_REQUIRED) || error.contains("not installed"), "{error}");
        let last = events.last().unwrap();
        assert_eq!((last.operation.as_str(), last.code.as_deref()), ("ocr.paddle", Some("model_missing")));
    }

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
