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

use anyhow::Result;
#[cfg(feature = "crispembed")]
use anyhow::Context;
use std::path::Path;
#[cfg(feature = "crispembed")]
use std::sync::Mutex;

use super::ExtractedDocument;

/// Default detection model (Surya-OCR-2, EfficientViT, 91 languages).
#[cfg(feature = "crispembed")]
const DEFAULT_DET_MODEL: &str = "dbnet-det";
/// Default recognition model (Qwen2.5-VL, German support).
#[cfg(feature = "crispembed")]
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

// ── Pre-OCR image-restoration engines (cached; `None` once init fails) ──
#[cfg(feature = "crispembed")]
static PAN_SR: std::sync::OnceLock<Option<Mutex<crispembed::CrispPanSr>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static ESRGAN_SR: std::sync::OnceLock<Option<Mutex<crispembed::CrispEsrganSr>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static SAFMN_SR: std::sync::OnceLock<Option<Mutex<crispembed::CrispSafmnSr>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static RESTORMER: std::sync::OnceLock<Option<Mutex<crispembed::CrispRestormer>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static SCUNET: std::sync::OnceLock<Option<Mutex<crispembed::CrispScunet>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static HAT_SR: std::sync::OnceLock<Option<Mutex<crispembed::CrispHatSr>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static TBSRN_SR: std::sync::OnceLock<Option<Mutex<crispembed::CrispTbsrnSr>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static INSTRUCTIR: std::sync::OnceLock<Option<Mutex<crispembed::CrispInstructIR>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static DAT_SR: std::sync::OnceLock<Option<Mutex<crispembed::CrispDatSr>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static SWINIR_SR: std::sync::OnceLock<Option<Mutex<crispembed::CrispSwinirSr>>> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static SCAN_CLEANUP: std::sync::OnceLock<Option<Mutex<crispembed::CrispScanCleanup>>> =
    std::sync::OnceLock::new();

/// Save an RGB buffer to a temp PNG and return it + its path (held alive).
#[cfg(feature = "crispembed")]
fn save_rgb_temp(
    prefix: &str,
    w: u32,
    h: u32,
    rgb: Vec<u8>,
) -> Option<(tempfile::NamedTempFile, std::path::PathBuf)> {
    let img = image::RgbImage::from_raw(w, h, rgb)?;
    let tmp = tempfile::Builder::new().prefix(prefix).suffix(".png").tempfile().ok()?;
    image::DynamicImage::ImageRgb8(img).save(tmp.path()).ok()?;
    let p = tmp.path().to_path_buf();
    Some((tmp, p))
}

/// Write an RGB buffer to a **stable** temp PNG that survives across Tauri
/// calls (unlike [`save_rgb_temp`]'s auto-deleting NamedTempFile). For the OCR
/// workbench, where the frontend loads the path via `convertFileSrc` later.
#[cfg(feature = "crispembed")]
fn save_rgb_stable(prefix: &str, w: u32, h: u32, rgb: Vec<u8>) -> Option<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let img = image::RgbImage::from_raw(w, h, rgb)?;
    let dir = std::env::temp_dir().join("crispsorter_ocr_workbench");
    std::fs::create_dir_all(&dir).ok()?;
    let n = N.fetch_add(1, Ordering::Relaxed);
    let p = dir.join(format!("{prefix}{n}.png"));
    image::DynamicImage::ImageRgb8(img).save(&p).ok()?;
    Some(p)
}

/// Produce the **classical-cleanup** image (deskew / crop / whiten / binarize)
/// for a page, so the OCR workbench can show "what the OCR saw" alongside the
/// original. Returns a stable temp PNG path. `None` when off / unavailable.
#[cfg(feature = "crispembed")]
pub fn cleaned_page_image(path: &Path) -> Option<std::path::PathBuf> {
    let img = image::open(path).ok()?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let eng = SCAN_CLEANUP
        .get_or_init(|| crispembed::CrispScanCleanup::new(None, 0).ok().map(Mutex::new))
        .as_ref()?;
    let (out, ow, oh) = eng.lock().ok()?.process(img.as_raw(), w as i32, h as i32, 3).ok()?;
    save_rgb_stable("cleaned_", ow as u32, oh as u32, out)
}

/// Detect if an image is a two-up book spread and return the gutter column.
#[cfg(feature = "crispembed")]
pub fn detect_page_split(path: &Path) -> Option<i32> {
    let img = image::open(path).ok()?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let eng = SCAN_CLEANUP
        .get_or_init(|| crispembed::CrispScanCleanup::new(None, 0).ok().map(Mutex::new))
        .as_ref()?;
    eng.lock().ok()?.detect_page_split(img.as_raw(), w as i32, h as i32, 3)
}

/// Detect the content bounding box (trim blank margins).
#[cfg(feature = "crispembed")]
pub fn content_bbox(path: &Path) -> Option<(i32, i32, i32, i32)> {
    let img = image::open(path).ok()?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let eng = SCAN_CLEANUP
        .get_or_init(|| crispembed::CrispScanCleanup::new(None, 0).ok().map(Mutex::new))
        .as_ref()?;
    eng.lock().ok()?.content_bbox(img.as_raw(), w as i32, h as i32, 3)
}

#[cfg(not(feature = "crispembed"))]
pub fn detect_page_split(_path: &Path) -> Option<i32> { None }

#[cfg(not(feature = "crispembed"))]
pub fn content_bbox(_path: &Path) -> Option<(i32, i32, i32, i32)> { None }

#[cfg(not(feature = "crispembed"))]
pub fn cleaned_page_image(_path: &Path) -> Option<std::path::PathBuf> {
    None
}

/// Map an InstructIR task name to its model task index (mirrors
/// `instructir.h`'s `INSTRUCTIR_*` enum). Unknown → denoise (0).
#[cfg(feature = "crispembed")]
fn instructir_task_id(task: &str) -> i32 {
    match task {
        "deblur" => 1,
        "dehaze" => 2,
        "derain" => 3,
        "super_resolution" => 4,
        "low_light" => 5,
        "enhance" => 6,
        _ => 0, // denoise
    }
}

/// Run the selected SR engine (`pan` / `esrgan` / `safmn` / `hat` / `tbsrn` /
/// `dat` / `swinir`) on an RGB buffer.
#[cfg(feature = "crispembed")]
fn run_sr(
    engine: &str,
    model: Option<&str>,
    rgb: &[u8],
    w: i32,
    h: i32,
) -> Option<(Vec<u8>, i32, i32)> {
    match engine {
        "esrgan" => {
            let m = resolve(model.unwrap_or("esrgan-x4"));
            let e = ESRGAN_SR
                .get_or_init(|| crispembed::CrispEsrganSr::new(m, 0).map(Mutex::new))
                .as_ref()?;
            e.lock().ok()?.process(rgb, w, h)
        }
        "safmn" => {
            let m = resolve(model.unwrap_or("safmn-x4"));
            let e = SAFMN_SR
                .get_or_init(|| crispembed::CrispSafmnSr::new(m, 0).map(Mutex::new))
                .as_ref()?;
            e.lock().ok()?.process(rgb, w, h)
        }
        "hat" => {
            // HAT — Hybrid Attention Transformer, SOTA 4× SR (CVPR 2023).
            let m = resolve(model.unwrap_or("hat-sr-x4"));
            let e = HAT_SR
                .get_or_init(|| crispembed::CrispHatSr::new(&m, 0).ok().map(Mutex::new))
                .as_ref()?;
            e.lock().ok()?.process(rgb, w, h, 0, 0).ok()
        }
        "tbsrn" => {
            // TBSRN — text-line scene-text SR (tiny, PaddleOCR Telescope).
            let m = resolve(model.unwrap_or("tbsrn-telescope"));
            let e = TBSRN_SR
                .get_or_init(|| crispembed::CrispTbsrnSr::new(&m, 0).ok().map(Mutex::new))
                .as_ref()?;
            e.lock().ok()?.process(rgb, w, h).ok()
        }
        "dat" => {
            // DAT — Dual Aggregation Transformer SR (strong transformer SR).
            let m = resolve(model.unwrap_or("dat-sr-x2"));
            let e = DAT_SR
                .get_or_init(|| crispembed::CrispDatSr::new(&m, 0).ok().map(Mutex::new))
                .as_ref()?;
            e.lock().ok()?.process(rgb, w, h, 0, 0).ok()
        }
        "swinir" => {
            // SwinIR-light — classic Swin-Transformer SR (tiny, 930K params).
            let m = resolve(model.unwrap_or("swinir-sr-x4"));
            let e = SWINIR_SR
                .get_or_init(|| crispembed::CrispSwinirSr::new(&m, 0).map(Mutex::new))
                .as_ref()?;
            e.lock().ok()?.process(rgb, w, h, 0, 0)
        }
        _ => {
            let m = resolve(model.unwrap_or("pan-x4"));
            let e = PAN_SR
                .get_or_init(|| crispembed::CrispPanSr::new(&m, 0).ok().map(Mutex::new))
                .as_ref()?;
            e.lock().ok()?.process(rgb, w, h, 0, 0).ok()
        }
    }
}

/// Super-resolve a low-resolution page before OCR. Engine is configurable
/// (`cfg.sr_engine`: pan / esrgan / safmn). Returns an upscaled temp PNG (keep
/// the tuple in scope for the OCR call) or `None` when off / already large /
/// unavailable. The SR compute runs in C++.
#[cfg(feature = "crispembed")]
pub fn super_resolve_page(
    path: &Path,
    cfg: &super::OcrPipelineConfig,
) -> Option<(tempfile::NamedTempFile, std::path::PathBuf)> {
    if !cfg.sr {
        return None;
    }
    let img = image::open(path).ok()?;
    let (w, h) = (img.width(), img.height());
    // Only upscale genuinely low-res pages; above the threshold a 4× blow-up
    // just wastes memory.
    if w.min(h) as i32 > cfg.sr_max_short_side {
        return None;
    }
    let rgb = img.to_rgb8();
    let model = cfg.sr_model.as_deref().filter(|s| !s.is_empty());
    let (out, ow, oh) = run_sr(&cfg.sr_engine, model, rgb.as_raw(), w as i32, h as i32)?;
    save_rgb_temp("ocr_sr_", ow as u32, oh as u32, out)
}

/// Restore a page before OCR. Engine is configurable (`cfg.restore_engine`:
/// `restormer` denoise+deblur, `scunet` denoise, or `instructir` all-in-one
/// task-driven via `cfg.restore_task`). Same dimensions out; helps noisy /
/// blurred scans the classical+NAFNet tiers can't. `None` when off / unavailable.
#[cfg(feature = "crispembed")]
pub fn restore_page(
    path: &Path,
    cfg: &super::OcrPipelineConfig,
) -> Option<(tempfile::NamedTempFile, std::path::PathBuf)> {
    if !cfg.restore {
        return None;
    }
    let rgb = image::open(path).ok()?.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    let model = cfg.restore_model.as_deref().filter(|s| !s.is_empty());
    let out = match cfg.restore_engine.as_str() {
        "scunet" => {
            let m = resolve(model.unwrap_or("scunet-color"));
            let eng = SCUNET
                .get_or_init(|| crispembed::CrispScunet::new(m, 0).map(Mutex::new))
                .as_ref()?;
            let (o, _ow, _oh) = eng.lock().ok()?.process(rgb.as_raw(), w as i32, h as i32)?;
            o
        }
        "instructir" => {
            // InstructIR — all-in-one task-driven restoration (same dims out).
            let m = resolve(model.unwrap_or("instructir"));
            let task = instructir_task_id(&cfg.restore_task);
            let eng = INSTRUCTIR
                .get_or_init(|| crispembed::CrispInstructIR::new(&m, 0).map(Mutex::new))
                .as_ref()?;
            let (o, _ow, _oh) =
                eng.lock().ok()?.process(rgb.as_raw(), w as i32, h as i32, task)?;
            o
        }
        _ => {
            let m = resolve(model.unwrap_or("restormer-denoise"));
            let eng = RESTORMER
                .get_or_init(|| crispembed::CrispRestormer::new(&m, 0).ok().map(Mutex::new))
                .as_ref()?;
            let guard = eng.lock().ok()?;
            guard.process(rgb.as_raw(), w as i32, h as i32, 0, 0).ok()?
        }
    };
    save_rgb_temp("ocr_restore_", w, h, out)
}

/// Dewarp (straighten curved/warped text lines) a page before OCR. Grayscale
/// in/out; `None` when off / unavailable / too few text lines to fit a baseline.
#[cfg(feature = "crispembed")]
pub fn dewarp_page(
    path: &Path,
    cfg: &super::OcrPipelineConfig,
) -> Option<(tempfile::NamedTempFile, std::path::PathBuf)> {
    if !cfg.dewarp {
        return None;
    }
    let gray = image::open(path).ok()?.to_luma8();
    let (w, h) = (gray.width(), gray.height());
    let (out, ow, oh) = if cfg.dewarp_engine == "tps" {
        // TPS spatial-transformer dewarp (learned localizer); same dims out.
        let m = resolve("tps-loc");
        let o = crispembed::tps_auto_dewarp(gray.as_raw(), w as i32, h as i32, &m).ok()?;
        (o, w as i32, h as i32)
    } else {
        crispembed::dewarp(gray.as_raw(), w as i32, h as i32).ok()?
    };
    let img = image::GrayImage::from_raw(ow as u32, oh as u32, out)?;
    let tmp = tempfile::Builder::new().prefix("ocr_dewarp_").suffix(".png").tempfile().ok()?;
    image::DynamicImage::ImageLuma8(img).save(tmp.path()).ok()?;
    let p = tmp.path().to_path_buf();
    Some((tmp, p))
}

#[cfg(not(feature = "crispembed"))]
pub fn super_resolve_page(
    _path: &Path,
    _cfg: &super::OcrPipelineConfig,
) -> Option<(tempfile::NamedTempFile, std::path::PathBuf)> {
    None
}

#[cfg(not(feature = "crispembed"))]
pub fn restore_page(
    _path: &Path,
    _cfg: &super::OcrPipelineConfig,
) -> Option<(tempfile::NamedTempFile, std::path::PathBuf)> {
    None
}

#[cfg(not(feature = "crispembed"))]
pub fn dewarp_page(
    _path: &Path,
    _cfg: &super::OcrPipelineConfig,
) -> Option<(tempfile::NamedTempFile, std::path::PathBuf)> {
    None
}

/// Detect text-line regions with the model-free connected-components detector
/// (`crispembed::cc_detect`) — zero-download, GPU-free. Returns boxes as
/// `Text` layout regions (cc_detect has no semantic typing) in raw order; the
/// caller sorts into reading order.
#[cfg(feature = "crispembed")]
pub fn cc_detect_regions(path: &Path) -> Vec<super::layout::LayoutRegion> {
    use super::layout::{LayoutRegion, RegionKind};
    let gray = match image::open(path) {
        Ok(i) => i.to_luma8(),
        Err(_) => return vec![],
    };
    let (w, h) = (gray.width(), gray.height());
    crispembed::cc_detect(gray.as_raw(), w as i32, h as i32)
        .into_iter()
        .map(|r| LayoutRegion {
            x1: r.x,
            y1: r.y,
            x2: r.x + r.w,
            y2: r.y + r.h,
            score: r.confidence,
            kind: RegionKind::Text,
            label_name: "text".to_string(),
        })
        .collect()
}

#[cfg(not(feature = "crispembed"))]
pub fn cc_detect_regions(_path: &Path) -> Vec<super::layout::LayoutRegion> {
    vec![]
}

/// Parse a table image into an HTML `<table>` (rule-based structure + per-cell
/// OCR via `ocr_model`, default the pipeline recogniser).
#[cfg(feature = "crispembed")]
pub fn table_to_html(path: &Path, ocr_model: Option<&str>) -> Result<String> {
    let gray = image::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .to_luma8();
    let (w, h) = (gray.width(), gray.height());
    let model = resolve(ocr_model.unwrap_or(DEFAULT_REC_MODEL));
    let parser = crispembed::CrispTableParse::new(&model, 0)
        .map_err(|e| anyhow::anyhow!("table parser init: {e}"))?;
    parser
        .to_html(gray.as_raw(), w as i32, h as i32)
        .ok_or_else(|| anyhow::anyhow!("table parse produced no HTML"))
}

/// Detect a table's grid dimensions (rows, cols) without OCR.
#[cfg(feature = "crispembed")]
pub fn table_grid(path: &Path) -> Result<(i32, i32)> {
    let gray = image::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .to_luma8();
    let (w, h) = (gray.width(), gray.height());
    crispembed::CrispTableParse::detect_grid(gray.as_raw(), w as i32, h as i32)
        .ok_or_else(|| anyhow::anyhow!("no table grid detected"))
}

/// Layout-aware KIE via LiLT: OCR the image + run LiLT token classification to
/// extract `labels` → `(label, value, score)`. Uses the high-level CrispKie
/// pipeline (OCR det+rec done internally).
#[cfg(feature = "crispembed")]
pub fn kie_extract_lilt(
    image_path: &Path,
    labels: &[String],
    threshold: f32,
    lilt_model: Option<&str>,
) -> Result<Vec<(String, String, f32)>> {
    let img = image_path.to_str().context("image path not UTF-8")?;
    let det = resolve(DEFAULT_DET_MODEL);
    let rec = resolve(DEFAULT_REC_MODEL);
    let lilt = resolve(lilt_model.unwrap_or("lilt-funsd"));
    let kie = crispembed::CrispKie::new_lilt(&det, &rec, "", &lilt, 0)
        .map_err(|e| anyhow::anyhow!("LiLT KIE init: {e}"))?;
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let fields = kie
        .extract(img, &label_refs, threshold)
        .map_err(|e| anyhow::anyhow!("LiLT KIE extract: {e}"))?;
    Ok(fields.into_iter().map(|f| (f.label, f.value, f.score)).collect())
}

#[cfg(not(feature = "crispembed"))]
pub fn kie_extract_lilt(
    _image_path: &Path,
    _labels: &[String],
    _threshold: f32,
    _lilt_model: Option<&str>,
) -> Result<Vec<(String, String, f32)>> {
    anyhow::bail!("LiLT KIE requires the `crispembed` cargo feature")
}

#[cfg(not(feature = "crispembed"))]
pub fn table_to_html(_path: &Path, _ocr_model: Option<&str>) -> Result<String> {
    anyhow::bail!("table parsing requires the `crispembed` cargo feature")
}

#[cfg(not(feature = "crispembed"))]
pub fn table_grid(_path: &Path) -> Result<(i32, i32)> {
    anyhow::bail!("table grid detection requires the `crispembed` cargo feature")
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
        audio_pcm: None,
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
        audio_pcm: None,
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

    let orch = OCR_ORCH.get_or_init(|| Mutex::new(build_pipeline(cfg)));

    let mut guard = orch
        .lock()
        .map_err(|e| anyhow::anyhow!("OCR pipeline lock poisoned: {e}"))?;
    let res = guard
        .run(path_str)
        .map_err(|e| anyhow::anyhow!("CrispEmbed OCR pipeline run failed: {e}"))?;
    if res.mean_confidence > 0.0 {
        println!(
            "[ocr] pipeline: {} regions, mean confidence {:.2}",
            res.regions.len(),
            res.mean_confidence,
        );
    }
    // Capture the LID result (ISO 639-1) detected during the pipeline run,
    // if a lid_model was configured. Populates the `language` field so
    // downstream indexing/search can use it without a separate LID pass.
    // NOTE: `detected_lang()` landed after CrispEmbed v0.11.8 — gated so
    // the release build against the pinned tag compiles.  Un-gate once
    // CrispEmbed cuts a release with this API.
    let detected_lang: Option<String> = None;
    // let detected_lang = guard.detected_lang();

    Ok(ExtractedDocument {
        full_text: res.full_text,
        headings: Vec::new(),
        ext: path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase(),
        language: detected_lang,
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url: None,
        tags: vec![],
        audio_pcm: None,
    })
}

/// Run the OCR pipeline and return the per-region results (box + text +
/// confidence) instead of just the joined text. This is the input the
/// `ocr_render` structured/searchable renderers need (hOCR / ALTO / PDF).
/// Uses the same cached orchestrator as [`ocr_via_pipeline`].
#[cfg(feature = "crispembed")]
pub fn ocr_regions_via_pipeline(
    path: &Path,
    cfg: &super::OcrPipelineConfig,
) -> Result<Vec<super::ocr_render::OcrRegion>> {
    let path_str = path.to_str().context("image path is not valid UTF-8")?;
    let orch = OCR_ORCH.get_or_init(|| Mutex::new(build_pipeline(cfg)));
    let mut guard = orch
        .lock()
        .map_err(|e| anyhow::anyhow!("OCR pipeline lock poisoned: {e}"))?;
    let res = guard
        .run(path_str)
        .map_err(|e| anyhow::anyhow!("CrispEmbed OCR pipeline run failed: {e}"))?;
    Ok(res
        .regions
        .into_iter()
        .map(|r| {
            let conf = effective_confidence(r.confidence, r.rec_confidence, r.char_conf.len());
            super::ocr_render::OcrRegion {
                text: r.text,
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                confidence: conf,
            }
        })
        .collect())
}

#[cfg(not(feature = "crispembed"))]
pub fn ocr_regions_via_pipeline(
    _path: &Path,
    _cfg: &super::OcrPipelineConfig,
) -> Result<Vec<super::ocr_render::OcrRegion>> {
    anyhow::bail!("OCR region extraction requires the `crispembed` cargo feature")
}

/// A region plus per-character confidence, for the OCR workbench. `confidence`
/// is the **recognition** confidence (mean per-char softmax) when the engine
/// exposes it (the signal that flags OCR errors), else the detection score.
#[derive(Debug, Clone)]
pub struct RegionConf {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub confidence: f32,
    pub char_conf: Vec<f32>,
}

/// Pick the confidence to surface for a region: prefer **recognition**
/// confidence (mean per-char softmax) when the engine exposes per-char data,
/// else fall back to the detection score (usually ~1.0 and useless for
/// proofreading). Pure helper, kept ungated so it's unit-testable.
pub fn effective_confidence(detection: f32, rec: f32, char_conf_len: usize) -> f32 {
    if char_conf_len > 0 && rec > 0.0 {
        rec
    } else {
        detection
    }
}

/// Like [`ocr_regions_via_pipeline`] but also returns per-character confidence
/// (PARSeq / Tesseract-LSTM expose it; VLM engines don't → empty).
#[cfg(feature = "crispembed")]
pub fn ocr_regions_detailed(
    path: &Path,
    cfg: &super::OcrPipelineConfig,
) -> Result<Vec<RegionConf>> {
    let path_str = path.to_str().context("image path is not valid UTF-8")?;
    let orch = OCR_ORCH.get_or_init(|| Mutex::new(build_pipeline(cfg)));
    let mut guard = orch
        .lock()
        .map_err(|e| anyhow::anyhow!("OCR pipeline lock poisoned: {e}"))?;
    let res = guard
        .run(path_str)
        .map_err(|e| anyhow::anyhow!("CrispEmbed OCR pipeline run failed: {e}"))?;
    Ok(res
        .regions
        .into_iter()
        .map(|r| {
            let confidence = effective_confidence(r.confidence, r.rec_confidence, r.char_conf.len());
            RegionConf {
                text: r.text,
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                confidence,
                char_conf: r.char_conf,
            }
        })
        .collect())
}

#[cfg(not(feature = "crispembed"))]
pub fn ocr_regions_detailed(
    _path: &Path,
    _cfg: &super::OcrPipelineConfig,
) -> Result<Vec<RegionConf>> {
    anyhow::bail!("OCR region extraction requires the `crispembed` cargo feature")
}

/// Map a VLM escalation engine name to the C `vlm_engine` id used by
/// `CrispOcrPipeline::new` (0=GOT 1=GLM 2=Qwen2-VL 3=InternVL2). Note this
/// numbering differs from the per-stage `engine_id`. Unknown → Qwen2-VL.
#[cfg(feature = "crispembed")]
fn vlm_engine_id(engine: &str) -> i32 {
    match engine {
        "got" => 0,
        "glm" => 1,
        "internvl2" => 3,
        "qwen3vl" => 4,
        _ => 2, // qwen2vl (german-ocr-3.1 family)
    }
}

/// Default single-shot model registry name for a VLM engine string.
#[cfg(feature = "crispembed")]
fn vlm_default_model(engine: &str) -> &'static str {
    match engine {
        "glm" => "glm-ocr",
        "got" => "got-ocr2",
        "internvl2" => "internvl2-ocr",
        "deepseek_ocr2" => "deepseek-ocr2",
        "pix2struct" => "pix2struct-base",
        "granite_vision" => "granite-vision",
        "lightonocr" => "lightonocr",
        "qwen3vl" => "qwen3vl-2b",
        "unlimited_ocr" => "unlimited-ocr",
        _ => "qwen2vl-ocr",
    }
}

/// Resolve a model registry name to a cached/downloaded GGUF path (best-effort).
#[cfg(feature = "crispembed")]
fn resolve(name: &str) -> String {
    crispembed::CrispEmbed::resolve_model(name, Some(true)).unwrap_or_else(|_| name.to_string())
}

/// Build the orchestrator: the explicit per-stage builder when `cfg.stages` is
/// non-empty (full tweakability), else the flat simple-mode pipeline.
#[cfg(feature = "crispembed")]
fn build_pipeline(cfg: &super::OcrPipelineConfig) -> crispembed::CrispOcrPipeline {
    use super::{engine_id, source_type_id};
    // Optional post-OCR punctuation/spacing restorer (resolved once).
    let punct: Option<String> = cfg
        .punct_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(resolve);
    // Optional post-OCR truecaser + text-LID models (registry names → paths).
    let truecase: Option<String> = cfg
        .truecase_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(resolve);
    let lid: Option<String> = cfg
        .lid_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(resolve);
    // Tesseract LID-auto-select dir is a filesystem path, not a registry model.
    let tess_dir: Option<&str> = cfg.tess_model_dir.as_deref().filter(|s| !s.is_empty());
    if cfg.stages.is_empty() {
        // Simple mode (slice-A flat config).
        let det = resolve(cfg.det_model.as_deref().unwrap_or(DEFAULT_DET_MODEL));
        let rec = resolve(cfg.rec_model.as_deref().unwrap_or(DEFAULT_REC_MODEL));
        let nafnet = if cfg.denoise {
            crispembed::CrispEmbed::resolve_model(
                cfg.nafnet_model.as_deref().unwrap_or(NAFNET_MODEL),
                Some(true),
            )
            .ok()
        } else {
            None
        };
        // Optional VLM escalation (e.g. german-ocr-3.1): when set, the chain
        // tries DBNet+TrOCR first and falls back to the VLM if the gate fails.
        let vlm: Option<String> = cfg
            .vlm_ocr_model
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(resolve);
        return crispembed::CrispOcrPipeline::new(
            &det,
            &rec,
            nafnet.as_deref(),
            cfg.router,
            cfg.cleanup_enabled,
            cfg.min_chars,
            cfg.min_confidence,
            vlm.as_deref(),
            vlm_engine_id(&cfg.vlm_ocr_engine),
            punct.as_deref(),
            lid.as_deref(),
            truecase.as_deref(),
            tess_dir,
            0,
        )
        .expect("CrispEmbed OCR pipeline init failed");
    }

    // Advanced mode: explicit per-stage chains.
    let nafnet = crispembed::CrispEmbed::resolve_model(
        cfg.nafnet_model.as_deref().unwrap_or(NAFNET_MODEL),
        Some(true),
    )
    .ok();
    let specs: Vec<crispembed::OcrStageSpec> = cfg
        .stages
        .iter()
        .map(|s| {
            let eid = engine_id(&s.engine);
            // dbnet_trocr(0) / surya(1) / tesseract(6) / parseq(7) need det+rec;
            // VLMs use a single model. Tesseract recogniser defaults to a
            // tesseract GGUF, PARSeq to the parseq scene-text GGUF.
            let (model_a, model_b) = if eid == 0 || eid == 1 || eid == 6 || eid == 7 {
                let rec_default = match eid {
                    6 => "tesseract-eng",
                    7 => "parseq",
                    _ => DEFAULT_REC_MODEL,
                };
                (
                    resolve(s.det_model.as_deref().unwrap_or(DEFAULT_DET_MODEL)),
                    resolve(s.rec_model.as_deref().unwrap_or(rec_default)),
                )
            } else {
                let m = s
                    .det_model
                    .clone()
                    .unwrap_or_else(|| vlm_default_model(&s.engine).to_string());
                (resolve(&m), String::new())
            };
            crispembed::OcrStageSpec {
                source_type: source_type_id(&s.source_type),
                engine: eid,
                model_a,
                model_b,
                cleanup: crispembed::OcrCleanupSpec {
                    enabled: s.cleanup.enabled,
                    deskew: s.cleanup.deskew,
                    crop_borders: s.cleanup.crop_borders,
                    whiten_background: s.cleanup.whiten_background,
                    binarize: s.cleanup.binarize,
                    binarize_method: s.cleanup.binarize_method,
                    sauvola_k: s.cleanup.sauvola_k,
                    sauvola_window: s.cleanup.sauvola_window,
                    morph_kernel: s.cleanup.morph_kernel,
                    border_threshold: s.cleanup.border_threshold,
                    deskew_max_angle: s.cleanup.deskew_max_angle,
                    denoise: s.cleanup.denoise,
                },
                det_prob_threshold: s.det_prob_threshold,
                det_box_threshold: s.det_box_threshold,
                det_target_short: s.det_target_short,
                vlm_max_tokens: s.vlm_max_tokens,
                vlm_prompt: s.vlm_prompt.clone(),
                min_chars: s.min_chars,
                min_confidence: s.min_confidence,
            }
        })
        .collect();
    // sr_model = None for now (text-SR pre-processor is wired in P20 #2).
    crispembed::CrispOcrPipeline::from_stages(
        cfg.router,
        nafnet.as_deref(),
        None,
        punct.as_deref(),
        lid.as_deref(),
        truecase.as_deref(),
        tess_dir,
        &specs,
        0,
    )
    .expect("CrispEmbed OCR per-stage pipeline init failed")
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

    /// Live E2E for the C++ orchestrator path (`ocr_via_pipeline`): exercises
    /// the full FFI — config marshalling, build_pipeline (simple mode), the
    /// orchestrator run, and result mapping — end to end. Downloads dbnet-det +
    /// the recognizer on first run. Text accuracy is validated separately via
    /// the CrispEmbed CLI (`--ocr-pipeline`); this guards the Rust↔C++ wiring.
    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal ocr_pipeline_live -- --ignored
    fn ocr_pipeline_live_simple() {
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("page.png");
        // High-contrast synthetic page (not real glyphs — accuracy is the CLI's
        // job; here we assert the pipeline runs end-to-end without crashing).
        let mut img = image::RgbImage::from_pixel(400, 120, image::Rgb([255, 255, 255]));
        for y in 40..70 {
            for x in 20..380 {
                if (x / 14) % 2 == 0 {
                    img.put_pixel(x, y, image::Rgb([0, 0, 0]));
                }
            }
        }
        img.save(&img_path).unwrap();

        let cfg = super::super::OcrPipelineConfig {
            enabled: true,
            ..Default::default()
        };
        let doc = ocr_via_pipeline(&img_path, &cfg)
            .expect("orchestrator pipeline should run end-to-end without crashing");
        println!("pipeline full_text len: {}", doc.full_text.len());
    }

    /// Live E2E for the per-stage builder (`from_stages`) with a Tesseract
    /// stage: validates the advanced config path + the tesseract engine wiring.
    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore]
    fn ocr_pipeline_live_tesseract_stage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let img_path = tmp.path().join("page.png");
        let img = image::RgbImage::from_pixel(400, 120, image::Rgb([255, 255, 255]));
        img.save(&img_path).unwrap();

        let mut cfg = super::super::OcrPipelineConfig { enabled: true, ..Default::default() };
        cfg.stages.push(super::super::OcrStageSpec {
            source_type: "auto".into(),
            engine: "tesseract".into(),
            det_model: Some("dbnet-det".into()),
            rec_model: Some("tesseract-eng".into()),
            cleanup: Default::default(),
            det_prob_threshold: 0.3,
            det_box_threshold: 0.5,
            det_target_short: 736,
            vlm_max_tokens: 0,
            vlm_prompt: String::new(),
            min_chars: 1,
            min_confidence: 0.0,
        });
        let doc = ocr_via_pipeline(&img_path, &cfg)
            .expect("tesseract-stage pipeline should run without crashing");
        println!("tesseract pipeline full_text len: {}", doc.full_text.len());
    }

    // ── Restoration pre-processors (#9 restore / #10 SR engine / #11 dewarp) ──

    /// Write a small high-contrast synthetic page (text-ish stripes).
    fn synth_page(dir: &std::path::Path, name: &str, w: u32, h: u32) -> std::path::PathBuf {
        let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([255, 255, 255]));
        for y in 0..h {
            for x in 0..w {
                if (y / 4) % 2 == 0 && (x / 3) % 2 == 0 {
                    img.put_pixel(x, y, image::Rgb([0, 0, 0]));
                }
            }
        }
        let p = dir.join(name);
        img.save(&p).unwrap();
        p
    }

    #[test]
    fn restoration_helpers_off_by_default_return_none() {
        // With a default (all-off) config the pre-processors are no-ops, in both
        // crispembed and stub builds.
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "x.png", 64, 64);
        let cfg = super::super::OcrPipelineConfig::default();
        assert!(restore_page(&p, &cfg).is_none());
        assert!(super_resolve_page(&p, &cfg).is_none());
        assert!(dewarp_page(&p, &cfg).is_none());
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal restore_live -- --ignored
    fn restore_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "noisy.png", 96, 64);
        let cfg = super::super::OcrPipelineConfig { restore: true, ..Default::default() };
        let (_g, out) = restore_page(&p, &cfg).expect("Restormer should restore");
        let img = image::open(&out).expect("restored image decodes");
        assert_eq!((img.width(), img.height()), (96, 64), "Restormer keeps dimensions");
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal restore_engines_live -- --ignored
    fn restore_engines_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "noisy.png", 96, 64);
        // SCUNet denoise + InstructIR (each of its 7 tasks) — all same-dims out.
        for (engine, task) in [
            ("scunet", "denoise"),
            ("instructir", "denoise"),
            ("instructir", "deblur"),
            ("instructir", "low_light"),
            ("instructir", "enhance"),
        ] {
            let cfg = super::super::OcrPipelineConfig {
                restore: true,
                restore_engine: engine.into(),
                restore_task: task.into(),
                ..Default::default()
            };
            let (_g, out) = restore_page(&p, &cfg)
                .unwrap_or_else(|| panic!("{engine}/{task} should restore"));
            let img = image::open(&out).expect("restored image decodes");
            assert_eq!((img.width(), img.height()), (96, 64), "{engine}: keeps dimensions");
        }
    }

    #[test]
    fn instructir_task_ids_match_enum() {
        // Mirrors instructir.h INSTRUCTIR_* ordering.
        #[cfg(feature = "crispembed")]
        {
            assert_eq!(instructir_task_id("denoise"), 0);
            assert_eq!(instructir_task_id("deblur"), 1);
            assert_eq!(instructir_task_id("dehaze"), 2);
            assert_eq!(instructir_task_id("derain"), 3);
            assert_eq!(instructir_task_id("super_resolution"), 4);
            assert_eq!(instructir_task_id("low_light"), 5);
            assert_eq!(instructir_task_id("enhance"), 6);
            assert_eq!(instructir_task_id("bogus"), 0, "unknown falls back to denoise");
        }
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal sr_engines_live -- --ignored
    fn sr_engines_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "low.png", 64, 48);
        for engine in ["pan", "esrgan", "safmn", "hat", "tbsrn", "dat", "swinir"] {
            let cfg = super::super::OcrPipelineConfig {
                sr: true,
                sr_engine: engine.into(),
                sr_max_short_side: 10_000, // force SR regardless of size
                ..Default::default()
            };
            let (_g, out) = super_resolve_page(&p, &cfg)
                .unwrap_or_else(|| panic!("{engine} SR should upscale"));
            let img = image::open(&out).expect("SR image decodes");
            assert!(img.width() > 64, "{engine}: upscaled wider ({})", img.width());
        }
    }

    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal dewarp_live -- --ignored
    fn dewarp_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "warp.png", 256, 128);
        let cfg = super::super::OcrPipelineConfig { dewarp: true, ..Default::default() };
        // Dewarp may return None on a synthetic page (too few real text lines);
        // the contract is "runs without panicking, output decodes if produced".
        if let Some((_g, out)) = dewarp_page(&p, &cfg) {
            let img = image::open(&out).expect("dewarped image decodes");
            assert!(img.width() > 0 && img.height() > 0);
        }
    }

    // ── Per-character confidence selection (workbench) ──

    #[test]
    fn effective_confidence_prefers_recognition_when_charconf_present() {
        // char_conf present + rec > 0 → use recognition confidence.
        assert_eq!(super::effective_confidence(0.99, 0.42, 5), 0.42);
        // no char_conf → fall back to detection score.
        assert_eq!(super::effective_confidence(0.99, 0.42, 0), 0.99);
        // char_conf present but rec is 0 (engine reported none) → detection.
        assert_eq!(super::effective_confidence(0.88, 0.0, 3), 0.88);
    }

    #[test]
    fn cleaned_page_image_none_on_missing_file() {
        // Graceful in both crispembed + stub builds (image::open fails → None).
        let missing = std::path::Path::new("/no/such/workbench/page.png");
        assert!(super::cleaned_page_image(missing).is_none());
    }

    /// Classical scan cleanup needs NO model download — runs whenever CrispEmbed
    /// is linked. Produces a stable cleaned PNG for the workbench compare.
    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal cleaned_page_image_live -- --ignored
    fn cleaned_page_image_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "scan.png", 200, 120);
        let out = super::cleaned_page_image(&p).expect("cleanup should produce an image");
        let img = image::open(&out).expect("cleaned image decodes");
        assert!(img.width() > 0 && img.height() > 0);
    }

    /// Detailed regions: box + text + per-character confidence. Needs det+rec
    /// models (downloads on first run). char_conf is engine-dependent (PARSeq /
    /// Tesseract expose it; the default DBNet+TrOCR may not) → only shape-checked.
    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal ocr_regions_detailed_live -- --ignored
    fn ocr_regions_detailed_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "text.png", 320, 96);
        let cfg = super::super::OcrPipelineConfig { enabled: true, ..Default::default() };
        let regions = super::ocr_regions_detailed(&p, &cfg).expect("detailed OCR runs");
        for r in &regions {
            assert!(r.confidence >= 0.0 && r.confidence <= 1.0, "confidence in [0,1]");
            // char_conf is either empty or roughly aligned to the text length.
            if !r.char_conf.is_empty() {
                assert!(r.char_conf.iter().all(|&c| (0.0..=1.0).contains(&c)));
            }
        }
    }

    /// Run one OCR engine through a **fresh** pipeline (not the cached
    /// `OCR_ORCH`, so engines can be tested back-to-back in one process) and
    /// return its regions with per-char confidence. Models auto-download.
    #[cfg(feature = "crispembed")]
    fn run_engine_fresh(p: &std::path::Path, engine: &str) -> Vec<super::RegionConf> {
        use super::super::{OcrCleanupSpec, OcrPipelineConfig, OcrStageSpec};
        let cfg = OcrPipelineConfig {
            enabled: true,
            stages: vec![OcrStageSpec {
                source_type: "auto".into(),
                engine: engine.into(),
                det_model: None,
                rec_model: None,
                cleanup: OcrCleanupSpec::default(),
                det_prob_threshold: 0.3,
                det_box_threshold: 0.5,
                det_target_short: 736,
                vlm_max_tokens: 64,
                vlm_prompt: String::new(),
                min_chars: 0,
                min_confidence: 0.0,
            }],
            ..Default::default()
        };
        let mut pipe = super::build_pipeline(&cfg);
        let res = pipe.run(p.to_str().unwrap()).expect("pipeline run");
        res.regions
            .into_iter()
            .map(|r| super::RegionConf {
                text: r.text,
                x: r.x,
                y: r.y,
                w: r.w,
                h: r.h,
                confidence: super::effective_confidence(r.confidence, r.rec_confidence, r.char_conf.len()),
                char_conf: r.char_conf,
            })
            .collect()
    }

    /// Test the per-char/recognition-confidence path for every pipeline engine.
    /// Asserts each engine runs and every confidence is a valid probability;
    /// char_conf is engine-dependent (tesseract → per-char; VLMs → per-token;
    /// TrOCR-based dbnet/surya → none) so only validity-when-present is checked
    /// (synthetic pages aren't guaranteed to recognize text).

    /// Small document engines — modest model downloads, run locally.
    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal ocr_engines_charconf_small_live -- --ignored
    fn ocr_engines_charconf_small_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "small.png", 320, 96);
        // parseq (24 MB) is small enough to run locally alongside the others;
        // it's the one engine here that yields per-character (1:1) confidence.
        for engine in ["dbnet_trocr", "surya", "tesseract", "parseq"] {
            let regions = run_engine_fresh(&p, engine);
            for r in &regions {
                assert!((0.0..=1.0).contains(&r.confidence), "{engine}: confidence range");
                assert!(
                    r.char_conf.iter().all(|&c| (0.0..=1.0).contains(&c)),
                    "{engine}: char_conf values are valid probabilities"
                );
            }
        }
    }

    /// VLM engines — multi-GB downloads; run on a GPU host (e.g. Kaggle), not in
    /// the local fast suite. Per-token confidences (counts differ from chars).
    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore] // cargo test --features crispembed-metal ocr_engines_charconf_vlm_live -- --ignored
    fn ocr_engines_charconf_vlm_live() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = synth_page(tmp.path(), "vlm.png", 320, 96);
        for engine in ["got", "glm", "qwen2vl", "internvl2",
                       "deepseek_ocr2", "pix2struct", "granite_vision", "lightonocr"] {
            let regions = run_engine_fresh(&p, engine);
            for r in &regions {
                assert!((0.0..=1.0).contains(&r.confidence), "{engine}: confidence range");
                assert!(
                    r.char_conf.iter().all(|&c| (0.0..=1.0).contains(&c)),
                    "{engine}: token confidences are valid probabilities"
                );
            }
        }
    }
}
