//! P17.2 — CrispEmbed GGUF OCR (Tier 4).
//!
//! Wraps `crispembed::OcrPipeline` (DBNet text detection + TrOCR line
//! recognition) as the highest-priority OCR tier.  All models
//! are GGUF — no ORT or Tesseract dependency.
//!
//! The pipeline auto-downloads models from HuggingFace on first use via
//! `CrispEmbed::resolve_model`.  Detection and recognition models are
//! specified by registry name; the default pairing is `dbnet-det` + `trocr-printed`.
//!
//! Gated behind `--features crispembed`.

use anyhow::Result;
use crate::operation_log::OperationLog;
use serde_json::{json, Value};
#[cfg(feature = "crispembed")]
use anyhow::Context;
use std::path::Path;
#[cfg(feature = "crispembed")]
use std::sync::Mutex;

use super::ExtractedDocument;

/// Default detector and recognizer are registered, compatible native models.
#[cfg(any(feature = "crispembed", test))]
const DEFAULT_DET_MODEL: &str = "dbnet-det";
/// Printed-text recognizer paired with DBNet.
#[cfg(any(feature = "crispembed", test))]
const DEFAULT_REC_MODEL: &str = "trocr-printed";

/// Process-global lazy-loaded OCR pipeline.  The CrispEmbed OcrPipeline
/// is not `Sync` so we wrap it in a `Mutex`.  This trades concurrency for
/// zero-overhead repeated calls (model stays loaded across documents).
#[cfg(feature = "crispembed")]
static OCR_PIPELINE: std::sync::OnceLock<Mutex<Option<super::context_cache::CachedContext<crispembed::OcrPipeline>>>> =
    std::sync::OnceLock::new();

/// Check if CrispEmbed OCR is available at runtime.
pub fn is_crispembed_ocr_available() -> bool {
    cfg!(feature = "crispembed")
}

// ── Pre-OCR image-restoration engines ──
// Model changes replace the loaded instance. Failed loads remain retryable.
#[cfg(any(feature = "crispembed", test))]
type ModelCache<T> = std::sync::OnceLock<std::sync::Mutex<Option<(String, T)>>>;

#[cfg(any(feature = "crispembed", test))]
fn with_cached_model<T, R>(
    cache: &ModelCache<T>,
    key: &str,
    load: impl FnOnce() -> Option<T>,
    run: impl FnOnce(&mut T) -> Option<R>,
) -> Option<R> {
    let mut slot = cache.get_or_init(|| std::sync::Mutex::new(None)).lock().ok()?;
    if slot.as_ref().map(|(key, _)| key.as_str()) != Some(key) {
        *slot = None;
        *slot = Some((key.to_owned(), load()?));
    }
    run(&mut slot.as_mut()?.1)
}

#[cfg(feature = "crispembed")]
static PAN_SR: ModelCache<crispembed::CrispPanSr> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static ESRGAN_SR: ModelCache<crispembed::CrispEsrganSr> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static SAFMN_SR: ModelCache<crispembed::CrispSafmnSr> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static RESTORMER: ModelCache<crispembed::CrispRestormer> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static SCUNET: ModelCache<crispembed::CrispScunet> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static HAT_SR: ModelCache<crispembed::CrispHatSr> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static TBSRN_SR: ModelCache<crispembed::CrispTbsrnSr> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static INSTRUCTIR: ModelCache<crispembed::CrispInstructIR> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static DAT_SR: ModelCache<crispembed::CrispDatSr> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static SWINIR_SR: ModelCache<crispembed::CrispSwinirSr> =
    std::sync::OnceLock::new();
#[cfg(feature = "crispembed")]
static SCAN_CLEANUP: ModelCache<crispembed::CrispScanCleanup> =
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
    let (out, ow, oh) = with_cached_model(&SCAN_CLEANUP, "classical", || crispembed::CrispScanCleanup::new(None, 0).ok(),
        |engine| engine.process(img.as_raw(), w as i32, h as i32, 3).ok())?;
    save_rgb_stable("cleaned_", ow as u32, oh as u32, out)
}

/// Detect if an image is a two-up book spread and return the gutter column.
///
/// Was a hardcoded `None` "deferred until CrispEmbed tags a release with
/// detect_page_split()" — that release is here: the method is
/// `CrispScanCleanup::detect_page_split` and both workflows now pin v0.16.1.
/// Still soft-fails to `None` on an unreadable image or an engine that will not
/// initialise, because a caller treats that as "not a spread" and carries on.
#[cfg(feature = "crispembed")]
pub fn detect_page_split(path: &Path) -> Option<i32> {
    let img = image::open(path).ok()?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    with_cached_model(&SCAN_CLEANUP, "classical", || crispembed::CrispScanCleanup::new(None, 0).ok(),
        |engine| engine.detect_page_split(img.as_raw(), w as i32, h as i32, 3))
}

#[cfg(not(feature = "crispembed"))]
pub fn detect_page_split(_path: &Path) -> Option<i32> {
    None
}

/// Detect the content bounding box, so blank scan margins can be trimmed.
///
/// Same story as [`detect_page_split`]: the stub outlived its reason. Returns
/// `(x0, y0, x1, y1)` in pixels, or `None` when the page is unreadable or the
/// engine finds no content to bound.
#[cfg(feature = "crispembed")]
pub fn content_bbox(path: &Path) -> Option<(i32, i32, i32, i32)> {
    let img = image::open(path).ok()?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    with_cached_model(&SCAN_CLEANUP, "classical", || crispembed::CrispScanCleanup::new(None, 0).ok(),
        |engine| engine.content_bbox(img.as_raw(), w as i32, h as i32, 3))
}

#[cfg(not(feature = "crispembed"))]
pub fn content_bbox(_path: &Path) -> Option<(i32, i32, i32, i32)> {
    None
}

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
            let m = resolve_checked(model.unwrap_or("esrgan-x4")).ok()?;
            with_cached_model(&ESRGAN_SR, &m, || crispembed::CrispEsrganSr::new(m.clone(), 0),
                |engine| engine.process(rgb, w, h))
        }
        "safmn" => {
            let m = resolve_checked(model.unwrap_or("safmn-x4")).ok()?;
            with_cached_model(&SAFMN_SR, &m, || crispembed::CrispSafmnSr::new(m.clone(), 0),
                |engine| engine.process(rgb, w, h))
        }
        "hat" => {
            // HAT — Hybrid Attention Transformer, SOTA 4× SR (CVPR 2023).
            let m = resolve_checked(model.unwrap_or("hat-sr-x4")).ok()?;
            with_cached_model(&HAT_SR, &m, || crispembed::CrispHatSr::new(&m, 0).ok(),
                |engine| engine.process(rgb, w, h, 0, 0).ok())
        }
        "tbsrn" => {
            // TBSRN — text-line scene-text SR (tiny, PaddleOCR Telescope).
            let m = resolve_checked(model.unwrap_or("tbsrn-telescope")).ok()?;
            with_cached_model(&TBSRN_SR, &m, || crispembed::CrispTbsrnSr::new(&m, 0).ok(),
                |engine| engine.process(rgb, w, h).ok())
        }
        "dat" => {
            // DAT — Dual Aggregation Transformer SR (strong transformer SR).
            let m = resolve_checked(model.unwrap_or("dat-sr-x2")).ok()?;
            with_cached_model(&DAT_SR, &m, || crispembed::CrispDatSr::new(&m, 0).ok(),
                |engine| engine.process(rgb, w, h, 0, 0).ok())
        }
        "swinir" => {
            // SwinIR-light — classic Swin-Transformer SR (tiny, 930K params).
            let m = resolve_checked(model.unwrap_or("swinir-sr-x4")).ok()?;
            with_cached_model(&SWINIR_SR, &m, || crispembed::CrispSwinirSr::new(&m, 0),
                |engine| engine.process(rgb, w, h, 0, 0))
        }
        _ => {
            let m = resolve_checked(model.unwrap_or("pan-x4")).ok()?;
            with_cached_model(&PAN_SR, &m, || crispembed::CrispPanSr::new(&m, 0).ok(),
                |engine| engine.process(rgb, w, h, 0, 0).ok())
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
    let (out, out_width, out_height) = match cfg.restore_engine.as_str() {
        "scunet" => {
            let m = resolve_checked(model.unwrap_or("scunet-color")).ok()?;
            with_cached_model(&SCUNET, &m, || crispembed::CrispScunet::new(m.clone(), 0),
                |engine| engine.process(rgb.as_raw(), w as i32, h as i32))?
        }
        "instructir" => {
            // InstructIR — all-in-one task-driven restoration (same dims out).
            let m = resolve_checked(model.unwrap_or("instructir")).ok()?;
            let task = instructir_task_id(&cfg.restore_task);
            with_cached_model(&INSTRUCTIR, &m, || crispembed::CrispInstructIR::new(&m, 0),
                |engine| engine.process(rgb.as_raw(), w as i32, h as i32, task))?
        }
        _ => {
            let m = resolve_checked(model.unwrap_or("restormer-denoise")).ok()?;
            let output = with_cached_model(&RESTORMER, &m, || crispembed::CrispRestormer::new(&m, 0).ok(),
                |engine| engine.process(rgb.as_raw(), w as i32, h as i32, 0, 0).ok())?;
            (output, w as i32, h as i32)
        }
    };
    if out_width <= 0 || out_height <= 0 { return None; }
    save_rgb_temp("ocr_restore_", out_width as u32, out_height as u32, out)
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
pub fn restore_page_checked(_path: &Path, _cfg: &super::OcrPipelineConfig) -> std::result::Result<(tempfile::NamedTempFile, std::path::PathBuf), String> {
    Err("Restoration requires a build with CrispEmbed.".into())
}

#[cfg(not(feature = "crispembed"))]
pub fn super_resolve_page_checked(_path: &Path, _cfg: &super::OcrPipelineConfig) -> std::result::Result<(tempfile::NamedTempFile, std::path::PathBuf), String> {
    Err("Super-resolution requires a build with CrispEmbed.".into())
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

/// Recogniser the table parser can actually drive.
///
/// `CrispTableParse` loads its cell recogniser as a **Tesseract LSTM** model,
/// and the pinned parser treats a load failure as non-fatal: the grid is still
/// detected, every cell just comes back empty. Passing the pipeline's default
/// recogniser (`qwen2vl-ocr`, a VLM GGUF) therefore degrades table preservation
/// into "parse, get nothing, throw the crop away, fall back to plain OCR".
const TABLE_REC_MODEL: &str = "tesseract-eng";

/// Preserve an explicitly requested model exactly. Custom Tesseract GGUF files
/// need not be named `tesseract-*`; failures are surfaced without replacement.
fn table_rec_model(ocr_model: Option<&str>) -> &str {
    ocr_model.unwrap_or(TABLE_REC_MODEL)
}

/// Parse a table image into an HTML `<table>` (rule-based structure + per-cell
/// OCR). `ocr_model` must be a Tesseract recogniser. Explicit choices are never
/// replaced with another model.
#[cfg(feature = "crispembed")]
pub fn table_to_html(path: &Path, ocr_model: Option<&str>) -> Result<String> {
    let gray = image::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .to_luma8();
    let (w, h) = (gray.width(), gray.height());
    let model = resolve_checked(table_rec_model(ocr_model))?;
    let parser = crispembed::CrispTableParse::new(&model, 0)
        .map_err(|e| anyhow::anyhow!("table parser init: {e}"))?;
    let html = parser
        .to_html(gray.as_raw(), w as i32, h as i32)
        .ok_or_else(|| anyhow::anyhow!("table parse produced no HTML"))?;
    anyhow::ensure!(super::layout_table_text(&html).is_some(),
        "No cell text was recognized by the requested table model; verify that it is a compatible Tesseract GGUF. The model was not replaced");
    Ok(html)
}

/// One prepared image for both the grid receipt and cell recognition.
pub fn table_extract_with_config(
    path: &Path,
    ocr_model: Option<&str>,
    config: &super::OcrPipelineConfig,
) -> Result<(i32, i32, String)> {
    let log = OperationLog::start("ocr.table", json!({"engine":config.engine,"kind":"table"}));
    let result = (|| {
    log.step("validating", Value::Null);
    super::ocr_policy::validate_config(config).map_err(anyhow::Error::msg)?;
    super::ocr_policy::validate_runtime(config).map_err(anyhow::Error::msg)?;
    super::ocr_policy::validate_table_config(config).map_err(anyhow::Error::msg)?;
    let prepared = super::prepare_ocr_page(path, config, true)?;
    let stage = &config.stages[0];
    let model = ocr_model.or(stage.rec_model.as_deref()).unwrap_or_else(|| {
        if stage.engine == "tesseract_fraktur" { "tesseract-fraktur" } else { TABLE_REC_MODEL }
    });
    log.step("detecting_grid", Value::Null);
    let (rows, cols) = table_grid(&prepared.path)?;
    log.step("recognizing_cells", json!({"rows":rows,"cols":cols}));
    let html = table_to_html(&prepared.path, Some(model))?;
    Ok((rows, cols, html))
    })();
    match &result {
        Ok((rows,cols,html)) => log.succeed(json!({"rows":rows,"cols":cols,"bytes":html.len()})),
        Err(_) => log.fail("table_extraction_failed"),
    }
    result
}

/// Detect a table's grid dimensions (rows, cols) without OCR.
/// Apply the current preparation recipe without loading a cell recognizer.
pub fn table_grid_with_config(path: &Path, config: &super::OcrPipelineConfig) -> Result<(i32, i32)> {
    let log = OperationLog::start("ocr.table_grid", json!({"kind":"table","mode":"grid"}));
    let result = (|| {
        let (config, _) = super::ocr_policy::recognition_config(config.clone(), None, None).map_err(anyhow::Error::msg)?;
        let prepared = super::prepare_ocr_page(path, &config, true)?;
        log.step("detecting_grid", Value::Null);
        table_grid(&prepared.path)
    })();
    match &result { Ok((rows,cols)) => log.succeed(json!({"rows":rows,"cols":cols})), Err(_) => log.fail("grid_detection_failed") }
    result
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
pub fn kie_extract_lilt(
    image_path: &Path,
    labels: &[String],
    threshold: f32,
    lilt_model: Option<&str>,
) -> Result<Vec<(String, String, f32)>> {
    let mut config = super::OcrPipelineConfig::default();
    config.cleanup_enabled = false;
    let (config, _) = super::ocr_policy::recognition_config(config, Some("dbnet_trocr"), None).map_err(anyhow::Error::msg)?;
    kie_extract_lilt_with_config(image_path, labels, threshold, lilt_model, &config)
}

#[cfg(feature = "crispembed")]
pub fn kie_extract_lilt_with_config(
    image_path: &Path,
    labels: &[String],
    threshold: f32,
    lilt_model: Option<&str>,
    config: &super::OcrPipelineConfig,
) -> Result<Vec<(String, String, f32)>> {
    let log = OperationLog::start("ocr.lilt", json!({"engine":config.engine,"kind":"fields","count":labels.len()}));
    let result = (|| {
    log.step("validating", Value::Null);
    super::ocr_policy::validate_config(config).map_err(anyhow::Error::msg)?;
    super::ocr_policy::validate_runtime(config).map_err(anyhow::Error::msg)?;
    super::ocr_policy::validate_lilt_config(config).map_err(anyhow::Error::msg)?;
    let prepared = super::prepare_ocr_page(image_path, config, true)?;
    let image_path = &prepared.path;
    let img = image_path.to_str().context("image path not UTF-8")?;
    let stage = &config.stages[0];
    log.step("loading_models", Value::Null);
    let det = resolve_checked(stage.det_model.as_deref().unwrap_or(DEFAULT_DET_MODEL))?;
    let rec = resolve_checked(stage.rec_model.as_deref().unwrap_or(DEFAULT_REC_MODEL))?;
    let lilt = resolve_checked(lilt_model.unwrap_or("lilt-funsd"))?;
    let kie = crispembed::CrispKie::new_lilt(&det, &rec, "", &lilt, 0)
        .map_err(|e| anyhow::anyhow!("LiLT KIE init: {e}"))?;
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    log.step("extracting_fields", Value::Null);
    let fields = kie
        .extract(img, &label_refs, threshold)
        .map_err(|e| anyhow::anyhow!("LiLT KIE extract: {e}"))?;
    Ok(fields.into_iter().map(|f| (f.label, f.value, f.score)).collect::<Vec<_>>())
    })();
    match &result { Ok(fields) => log.succeed(json!({"results":fields.len()})), Err(_) => log.fail("lilt_extraction_failed") }
    result
}

#[cfg(not(feature = "crispembed"))]
pub fn kie_extract_lilt_with_config(
    _image_path: &Path,
    _labels: &[String],
    _threshold: f32,
    _lilt_model: Option<&str>,
    _config: &super::OcrPipelineConfig,
) -> Result<Vec<(String, String, f32)>> {
    let log = OperationLog::start("ocr.lilt", json!({"kind":"fields"}));
    log.fail("unsupported_build");
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

    let key = format!("{DEFAULT_DET_MODEL}:{DEFAULT_REC_MODEL}");
    let mut cache = super::context_cache::acquire(&OCR_PIPELINE, key, || {
        let det = crate::model_provisioning::resolve_model(DEFAULT_DET_MODEL, Some(true))?;
        let rec = crate::model_provisioning::resolve_model(DEFAULT_REC_MODEL, Some(true))?;
        crispembed::OcrPipeline::new(&det, &rec, 0)
    }).map_err(|e| anyhow::anyhow!("CrispEmbed OCR initialization failed: {e}"))?;
    let guard = &mut cache.as_mut().unwrap().context;

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
        ..Default::default()
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

    let det = crate::model_provisioning::resolve_model(det_model, Some(true))
        .unwrap_or_else(|_| det_model.to_string());
    let rec = crate::model_provisioning::resolve_model(rec_model, Some(true))
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
        ..Default::default()
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

/// Keep one model bundle warm, keyed by the complete requested configuration.
/// Failed initialization is not cached: fixing a model path or completing a
/// download must permit a retry without restarting the application.
#[cfg(feature = "crispembed")]
struct CachedOcrPipeline {
    key: String,
    pipeline: crispembed::CrispOcrPipeline,
}

#[cfg(feature = "crispembed")]
static OCR_ORCH: std::sync::OnceLock<Mutex<Option<CachedOcrPipeline>>> =
    std::sync::OnceLock::new();

#[cfg(feature = "crispembed")]
fn with_ocr_pipeline<T>(
    cfg: &super::OcrPipelineConfig,
    run: impl FnOnce(&mut crispembed::CrispOcrPipeline) -> Result<T>,
) -> Result<T> {
    let log = OperationLog::start("ocr.pipeline", json!({"engine":cfg.engine,"stages":cfg.stages.len()}));
    let result = (|| {
    log.step("waiting_for_model", Value::Null);
    let key = serde_json::to_string(cfg).context("serialize OCR configuration")?;
    let mut slot = OCR_ORCH.get_or_init(|| Mutex::new(None)).lock()
        .map_err(|e| anyhow::anyhow!("OCR pipeline lock poisoned: {e}"))?;
    if slot.as_ref().map(|p| p.key.as_str()) != Some(key.as_str()) {
        // Release the previous bundle before loading another large model.
        *slot = None;
        log.step("loading_models", json!({"reused":false}));
        *slot = Some(CachedOcrPipeline { key, pipeline: build_pipeline(cfg)? });
    } else { log.step("models_ready", json!({"reused":true})); }
    log.step("recognizing", Value::Null);
    run(&mut slot.as_mut().context("OCR pipeline was not initialized")?.pipeline)
    })();
    match &result { Ok(_) => log.succeed(Value::Null), Err(_) => log.fail("pipeline_failed") }
    result
}

/// Run the C++ OCR pipeline orchestrator (source-type routing + per-stage
/// cleanup + NAFNet denoise + accept-gate escalation) on an image.
#[cfg(feature = "crispembed")]
pub fn ocr_via_pipeline(
    path: &Path,
    cfg: &super::OcrPipelineConfig,
) -> Result<ExtractedDocument> {
    let path_str = path.to_str().context("image path is not valid UTF-8")?;

    let (res, detected_lang) = with_ocr_pipeline(cfg, |pipeline| {
        let result = pipeline.run(path_str)
            .map_err(|e| anyhow::anyhow!("CrispEmbed OCR pipeline run failed: {e}"))?;
        Ok((result, pipeline.detected_lang()))
    })?;
    if res.mean_confidence > 0.0 {
        crate::app_log("debug", format!(
            "[ocr] pipeline: {} regions, mean confidence {:.2}",
            res.regions.len(),
            res.mean_confidence,
        ));
    }
    // Capture the LID result (ISO 639-1) detected during the pipeline run,
    // if a lid_model was configured. Populates the `language` field so
    // downstream indexing/search can use it without a separate LID pass.
    //
    // This was stubbed to `None` while the release pin was CrispEmbed v0.11.8,
    // which predated `detected_lang()`. The pin is v0.16.1 now — and the API
    // was already present at v0.13.0, so the stub outlived its reason by
    // several releases while silently discarding a language the pipeline had
    // already computed.

    // CrispEmbed v0.17.12 also exposes `markdown`, but it cannot give this
    // extraction path trustworthy extra structure:
    //   * the Rust wrapper copies the native UTF-8 byte buffer with `b as
    //     char`, so every byte >= 0x80 becomes U+0080..U+00FF. German umlauts,
    //     accents and non-Latin scripts arrive mojibaked;
    //   * `build_markdown` only emits `#`/`##` from layout labels (`title` /
    //     `section_header`). `CrispOcrPipeline::new` hardcodes
    //     `layout_model: null`, and `from_stages` goes through
    //     `..._init_stages`, which has no layout-model parameter at all. With
    //     no layout pass the markdown path degenerates to region text with
    //     blank lines — no headings, tables or formulas.
    // Inferring headings from box geometry would fabricate structure for a
    // boosted search field, so `full_text` (already joined upstream in reading
    // order) stays the body and headings stay empty here.
    let headings = Vec::new();

    Ok(ExtractedDocument {
        full_text: res.full_text,
        headings,
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
        ..Default::default()
    })
}

/// Run the OCR pipeline and return the per-region results (box + text +
/// confidence) instead of just the joined text. This is the input the
/// `ocr_render` structured/searchable renderers need (hOCR / ALTO / PDF).
/// Uses the same cached orchestrator as [`ocr_via_pipeline`].
///
/// Regions come back in the **pipeline's reading order** — the same order
/// `full_text` is joined in — because the structured renderers emit text in
/// the order they receive it. Without this a two-column scan interleaves its
/// columns on select-and-copy, even though `full_text` reads correctly.
#[cfg(feature = "crispembed")]
pub fn ocr_regions_via_pipeline(
    path: &Path,
    cfg: &super::OcrPipelineConfig,
) -> Result<Vec<super::ocr_render::OcrRegion>> {
    let path_str = path.to_str().context("image path is not valid UTF-8")?;
    let res = with_ocr_pipeline(cfg, |pipeline| {
        pipeline.run(path_str)
            .map_err(|e| anyhow::anyhow!("CrispEmbed OCR pipeline run failed: {e}"))
    })?;
    Ok(render_regions_from_pipeline(res))
}

/// Reorder regions into the pipeline's reading order.
///
/// CrispEmbed's header defines `reading_order` as "reading-order region indices
/// from the most recent pipeline run" — indices into `regions`. It matters for
/// the structured renderers: hOCR / ALTO / searchable-PDF put text in the order
/// they receive it, so a two-column scan emitted in detector order gives
/// column-interleaved text on select-and-copy. `full_text` was already joined in
/// reading order upstream; the *regions* were not.
///
/// Defensive by construction, because a wrong order is better than lost text:
/// out-of-range and duplicate indices are ignored, and any region the list
/// never mentions is appended in its original position rather than dropped. An
/// empty list (engines that do not compute one) leaves the order untouched.
///
/// Deliberately **not** gated on `feature = "crispembed"`, though its only
/// caller is: CI builds without that feature (the native dylib is a
/// release-only artifact), so gating this would put the ordering logic in the
/// set of code no job compiles. It needs nothing from CrispEmbed — our own
/// `OcrRegion` and a slice of indices — so it compiles and is tested everywhere.
fn apply_reading_order<T>(regions: Vec<T>, order: &[i32]) -> Vec<T> {
    if order.is_empty() {
        return regions;
    }
    let mut taken = vec![false; regions.len()];
    let mut out: Vec<T> = Vec::with_capacity(regions.len());
    let mut slots: Vec<Option<T>> = regions.into_iter().map(Some).collect();

    for &idx in order {
        let Ok(i) = usize::try_from(idx) else { continue };
        if i >= slots.len() || taken[i] {
            continue;
        }
        taken[i] = true;
        if let Some(region) = slots[i].take() {
            out.push(region);
        }
    }
    // Anything the order never named still has to reach the renderer.
    for slot in slots.iter_mut() {
        if let Some(region) = slot.take() {
            out.push(region);
        }
    }
    out
}

/// One pipeline region reduced to the fields the renderer needs.
///
/// Deliberately independent of `crispembed`: the ordering + confidence policy
/// is then compiled and unit-tested in builds without the native library,
/// which is where most CI jobs run.
#[derive(Debug, Clone)]
struct RenderRegionInput {
    text: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    detection_confidence: f32,
    rec_confidence: f32,
    char_conf_len: usize,
}

/// Map pipeline regions onto renderer regions and apply the pipeline's reading
/// order. The index list refers to the pipeline's own region vector, which is
/// exactly the order the [`RenderRegionInput`] values arrive in.
#[cfg_attr(not(feature = "crispembed"), allow(dead_code))]
fn render_regions_in_reading_order(
    raw: Vec<RenderRegionInput>,
    order: &[i32],
) -> Vec<super::ocr_render::OcrRegion> {
    let regions = raw
        .into_iter()
        .map(|r| super::ocr_render::OcrRegion {
            confidence: effective_confidence(
                r.detection_confidence,
                r.rec_confidence,
                r.char_conf_len,
            ),
            text: r.text,
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        })
        .collect();
    apply_reading_order(regions, order)
}

/// Adapt the native result (whose `reading_order` indexes `regions`) for the
/// renderer. The wrapper's `markdown` field is deliberately not used: see
/// [`ocr_via_pipeline`] for why it carries no trustworthy structure here.
#[cfg(feature = "crispembed")]
fn render_regions_from_pipeline(
    res: crispembed::OcrPipelineResult,
) -> Vec<super::ocr_render::OcrRegion> {
    let crispembed::OcrPipelineResult {
        regions,
        reading_order,
        ..
    } = res;
    let raw = regions
        .into_iter()
        .map(|r| RenderRegionInput {
            text: r.text,
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
            detection_confidence: r.confidence,
            rec_confidence: r.rec_confidence,
            char_conf_len: r.char_conf.len(),
        })
        .collect();
    render_regions_in_reading_order(raw, &reading_order)
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegionConf {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub confidence: f32,
    pub char_conf: Vec<f32>,
}

/// Release any cached model and reserve the OCR pipeline while an isolated
/// low-memory worker is running. Environment changes stay in the child process.
#[cfg(feature = "crispembed")]
pub fn with_released_pipeline<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    let mut guard = OCR_ORCH.get_or_init(|| Mutex::new(None)).lock()
        .map_err(|e| anyhow::anyhow!("OCR pipeline lock poisoned: {e}"))?;
    drop(guard.take());
    work()
}

#[cfg(not(feature = "crispembed"))]
pub fn with_released_pipeline<T>(_work: impl FnOnce() -> Result<T>) -> Result<T> {
    anyhow::bail!("CrispEmbed OCR is unavailable in this build")
}

/// Pick the confidence to surface for a region: prefer **recognition**
/// confidence (mean per-char softmax) when the engine exposes per-char data,
/// else fall back to the detection score (usually ~1.0 and useless for
/// proofreading). Pure helper, kept ungated so it's unit-testable.
pub fn effective_confidence(detection: f32, rec: f32, char_conf_len: usize) -> f32 {
    if char_conf_len > 0 && rec.is_finite() && (0.0..=1.0).contains(&rec) {
        rec
    } else if detection.is_finite() && (0.0..=1.0).contains(&detection) {
        detection
    } else {
        -1.0
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
    let res = with_ocr_pipeline(cfg, |pipeline| {
        pipeline.run(path_str)
            .map_err(|e| anyhow::anyhow!("CrispEmbed OCR pipeline run failed: {e}"))
    })?;
    let regions = res.regions.into_iter().map(|r| {
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
        }).collect();
    Ok(apply_reading_order(regions, &res.reading_order))
}

#[cfg(not(feature = "crispembed"))]
pub fn ocr_regions_detailed(
    _path: &Path,
    _cfg: &super::OcrPipelineConfig,
) -> Result<Vec<RegionConf>> {
    anyhow::bail!("OCR region extraction requires the `crispembed` cargo feature")
}

/// Review regions distinguish detector scores from recognition evidence. The
/// native confidence array may contain tokenizer scores; equality with the text
/// length alone is not proof of character alignment.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OcrReviewRegion {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub confidence: Option<f32>,
    pub confidence_basis: String,
    pub char_conf: Vec<f32>,
    pub char_conf_unit: String,
    pub char_conf_aligned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_evidence: Option<crate::ocr_word_evidence::WordEvidenceEnvelope>,
    pub kind: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct OcrReviewResult {
    pub image_path: std::path::PathBuf,
    pub width: u32,
    pub height: u32,
    pub regions: Vec<OcrReviewRegion>,
    pub warnings: Vec<String>,
    pub diagnostics: serde_json::Value,
}

/// Recognition must see exactly the pixels displayed by the reviewer. Cleanup
/// and restoration happen explicitly before this call and produce a new asset;
/// repeating them inside individual engine stages would invalidate the boxes.
pub(crate) fn review_pipeline_config(cfg: &super::OcrPipelineConfig) -> super::OcrPipelineConfig {
    let mut config = cfg.clone();
    config.enabled = true;
    config.cleanup_enabled = false;
    config.denoise = false;
    config.dewarp = false;
    config.restore = false;
    config.sr = false;
    for stage in &mut config.stages {
        stage.cleanup.enabled = false;
        stage.cleanup.denoise = false;
    }
    config
}

#[cfg(any(feature = "ocr-review-diagnostics", test))]
fn parse_review_snapshot(payload: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some("crispembed.ocr.review")
        || value.get("schema_version").and_then(serde_json::Value::as_u64) != Some(1) {
        return None;
    }
    Some(value)
}

#[cfg(feature = "crispembed")]
pub fn ocr_regions_for_review(path: &Path, cfg: &super::OcrPipelineConfig) -> Result<OcrReviewResult> {
    let config = review_pipeline_config(cfg);
    let path_str = path.to_str().context("image path is not valid UTF-8")?;
    let (width, height) = image::image_dimensions(path).context("read review image dimensions")?;
    let (result, language, snapshot) = with_ocr_pipeline(&config, |pipeline| {
        let result = pipeline.run(path_str).map_err(|e| anyhow::anyhow!("OCR recognition failed: {e}"))?;
        #[cfg(feature = "ocr-review-diagnostics")]
        let snapshot = pipeline.review_json().and_then(|s| parse_review_snapshot(&s));
        #[cfg(not(feature = "ocr-review-diagnostics"))]
        let snapshot: Option<serde_json::Value> = None;
        Ok((result, pipeline.detected_lang(), snapshot))
    })?;
    if !result.regions.is_empty()
        && snapshot.as_ref().and_then(|s| s.pointer("/geometry/coordinates_valid_for_input")).and_then(serde_json::Value::as_bool) == Some(false) {
        anyhow::bail!("The recognition engine changed image coordinates. Apply scan repair explicitly and retry on the prepared page.");
    }
    let mut warnings: Vec<String> = snapshot.as_ref().and_then(|s| s.get("warnings")).and_then(serde_json::Value::as_array)
        .map(|list| list.iter().filter_map(serde_json::Value::as_str).map(String::from).collect()).unwrap_or_default();
    if snapshot.is_none() {
        warnings.push("Detailed engine provenance is unavailable or its schema is unsupported in this build. Confidence scores are uncalibrated and character alignment is unverified.".into());
    }
    let diagnostics = serde_json::json!({
        "coordinate_space": "prepared_image_pixels", "internal_cleanup": false,
        "language": language, "reading_order": result.reading_order,
        "full_text": result.full_text, "markdown": result.markdown,
        "native_review": snapshot,
    });
    let snapshot_regions = snapshot.as_ref().and_then(|s| s.get("regions")).and_then(serde_json::Value::as_array);
    let regions = result.regions.into_iter().enumerate().map(|(index, region)| {
        let native = snapshot_regions.and_then(|regions| regions.get(index));
        let score = effective_confidence(region.confidence, region.rec_confidence, region.char_conf.len());
        let (confidence, basis) = if let Some(native) = native {
            let confidence = native.get("confidence").and_then(serde_json::Value::as_f64)
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v)).map(|v| v as f32);
            let basis = match native.get("confidence_source").and_then(serde_json::Value::as_str) {
                Some("recognizer") => "recognition", Some("detector") => "detection", _ => "unknown",
            };
            (confidence, basis)
        } else {
            // Older native versions do not distinguish real scores from engine
            // placeholders. Retain numerical evidence for diagnostics, but the
            // review queue treats its source as unverified.
            (if score >= 0.0 { Some(score) } else { None }, "unknown")
        };
        let token_scores = native.and_then(|s| s.get("sequence_unit")).and_then(serde_json::Value::as_str) == Some("engine_token");
        OcrReviewRegion {
            text: region.text, x: region.x, y: region.y, w: region.w, h: region.h,
            confidence, confidence_basis: basis.into(),
            char_conf: region.char_conf.into_iter().filter(|v| v.is_finite() && (0.0..=1.0).contains(v)).collect(),
            char_conf_unit: if token_scores { "token" } else { "unknown" }.into(), char_conf_aligned: false, kind: "text".into(),
            word_evidence: None,
        }
    }).collect();
    Ok(OcrReviewResult {
        image_path: path.to_path_buf(), width, height,
        regions: apply_reading_order(regions, &result.reading_order),
        warnings, diagnostics,
    })
}

#[cfg(not(feature = "crispembed"))]
pub fn ocr_regions_for_review(_path: &Path, _cfg: &super::OcrPipelineConfig) -> Result<OcrReviewResult> {
    anyhow::bail!("OCR review requires a build with the crispembed feature")
}

/// Map a VLM escalation engine name to the C `vlm_engine` id used by
/// `CrispOcrPipeline::new` (0=GOT 1=GLM 2=Qwen2-VL 3=InternVL2). Note this
/// numbering differs from the per-stage `engine_id`. Unknown is rejected.
#[cfg(feature = "crispembed")]
fn vlm_engine_id(engine: &str) -> i32 {
    match engine {
        "got" => 0,
        "glm" => 1,
        "internvl2" => 3,
        "qwen3vl" => 4,
        "qwen2vl" => 2,
        _ => -1,
    }
}

/// Default single-shot model registry name for a VLM engine string.
#[cfg(feature = "crispembed")]
fn vlm_default_model(engine: &str) -> &'static str {
    match engine {
        "glm" => "glm-ocr",
        "got" => "got-ocr2",
        "internvl2" => "internvl2-2b",
        "deepseek_ocr2" => "deepseek-ocr2",
        "pix2struct" => "pix2struct-base",
        "granite_vision" => "granite-vision",
        "lightonocr" => "lightonocr",
        "qwen3vl" => "qwen3vl-2b",
        "unlimited_ocr" => "unlimited-ocr",
        "unlimited_ocr2" => "unlimited-ocr2", // experimental: CRISPEMBED_UNLIMITED_OCR2=1
        "minicpm_ocr" => "minicpm-ocr",       // experimental: CRISPEMBED_MINICPM_OCR=1 + CRISPEMBED_QWEN35_OCR=1
        "olmocr" => "olmocr-2-7b",
        "lfm2_vl" => "lfm2-vl",
        // `unified` dispatches on GGUF metadata, so it has no registry default:
        // an empty model_a makes the orchestrator report a missing model
        // instead of quietly loading something else.
        "unified" => "",
        // Apache-2.0 Qwen2-VL-2B fine-tune (CrispEmbed's recommended default);
        // `qwen2vl-3b` is Qwen Research (non-commercial) and needs consent.
        _ => "german-ocr-3.1",
    }
}

/// Resolve a model registry name to a cached/downloaded GGUF path (best-effort).
#[cfg(feature = "crispembed")]
fn resolve(name: &str) -> String {
    // Migrate the historical UI alias, which is absent from v0.17.12.
    let name = match name {
        "qwen2vl-ocr" => "qwen2vl-3b",
        "internvl2-ocr" => "internvl2-2b",
        _ => name,
    };
    // Restricted or unreviewed names must not be downloaded here: the load then
    // fails (empty stage) instead of quietly fetching unlicensed weights.
    if !crate::index::license_consent::may_download(name) {
        return name.to_string();
    }
    crate::model_provisioning::resolve_model(name, Some(true)).unwrap_or_else(|_| name.to_string())
}

/// Why the model cache cannot receive downloads, if it cannot: the native
/// downloader only prints to stderr, so a dangling link to a disconnected
/// drive otherwise surfaces as a bare "could not resolve model".
#[cfg(feature = "crispembed")]
pub fn cache_problem() -> Option<String> {
    let dir = std::path::PathBuf::from(crispembed::CrispEmbed::cache_dir());
    if dir.as_os_str().is_empty() { return Some("The model cache folder is not configured (set CRISPEMBED_CACHE_DIR).".into()); }
    if let Ok(target) = std::fs::read_link(&dir) {
        if !dir.exists() {
            return Some(format!("The model cache {} links to {}, which does not exist (a disconnected drive?). Reconnect it, repoint the link, or set CRISPEMBED_CACHE_DIR.", dir.display(), target.display()));
        }
    }
    if dir.exists() && !dir.is_dir() { return Some(format!("The model cache {} is not a folder.", dir.display())); }
    if !dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&dir) { return Some(format!("The model cache {} cannot be created: {e}", dir.display())); }
    }
    let probe = dir.join(".crispsorter-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => { let _ = std::fs::remove_file(&probe); None }
        Err(e) => Some(format!("The model cache {} is not writable: {e}", dir.display())),
    }
}

/// True when `name` is a file or a registry model already in the local cache.
#[cfg(feature = "crispembed")]
pub fn model_is_local(name: &str) -> bool {
    Path::new(name).is_file() || crate::model_provisioning::resolve_model(name, Some(false)).is_ok()
}

#[cfg(feature = "crispembed")]
pub fn restore_model_name(cfg: &super::OcrPipelineConfig) -> String {
    cfg.restore_model.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| match cfg.restore_engine.as_str() {
        "scunet" => "scunet-color", "instructir" => "instructir", _ => "restormer-denoise",
    }.to_string())
}

#[cfg(feature = "crispembed")]
pub fn sr_model_name(cfg: &super::OcrPipelineConfig) -> String {
    cfg.sr_model.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| match cfg.sr_engine.as_str() {
        "esrgan" => "esrgan-x4", "safmn" => "safmn-x4", "hat" => "hat-sr-x4", "tbsrn" => "tbsrn-telescope",
        "dat" => "dat-sr-x2", "swinir" => "swinir-sr-x4", _ => "pan-x4",
    }.to_string())
}

/// Restoration with the actual failure reason (model lookup/download, image
/// read, or the native engine returning no image).
#[cfg(feature = "crispembed")]
pub fn restore_page_checked(path: &Path, cfg: &super::OcrPipelineConfig) -> std::result::Result<(tempfile::NamedTempFile, std::path::PathBuf), String> {
    let engine = if cfg.restore_engine.is_empty() { "restormer" } else { cfg.restore_engine.as_str() };
    let name = restore_model_name(cfg);
    resolve_checked(&name).map_err(|e| format!("Restoration ({engine}): {e:#}"))?;
    let (w, h) = image::image_dimensions(path).map_err(|e| format!("Restoration ({engine}): could not read the page image: {e}"))?;
    restore_page(path, cfg).ok_or_else(|| format!(
        "Restoration ({engine}, model {name}) returned no image for this {w}×{h} page: the native engine could not be initialised or failed while processing, and reports no further detail. Try a smaller page, another restoration engine, or disable restoration."))
}

/// Super-resolution with the actual failure reason.
#[cfg(feature = "crispembed")]
pub fn super_resolve_page_checked(path: &Path, cfg: &super::OcrPipelineConfig) -> std::result::Result<(tempfile::NamedTempFile, std::path::PathBuf), String> {
    let engine = if cfg.sr_engine.is_empty() { "pan" } else { cfg.sr_engine.as_str() };
    let name = sr_model_name(cfg);
    resolve_checked(&name).map_err(|e| format!("Super-resolution ({engine}): {e:#}"))?;
    let (w, h) = image::image_dimensions(path).map_err(|e| format!("Super-resolution ({engine}): could not read the page image: {e}"))?;
    super_resolve_page(path, cfg).ok_or_else(|| format!(
        "Super-resolution ({engine}, model {name}) returned no image for this {w}×{h} page: the native engine could not be initialised or failed while processing, and reports no further detail. Try another engine or disable super-resolution."))
}

/// Interactive OCR must report missing models instead of treating the native
/// engine's empty fallback as a successfully recognized blank page. Registry
/// names pass the same licence gate as [`resolve`] before any download.
#[cfg(feature = "crispembed")]
fn resolve_checked(name: &str) -> Result<String> {
    anyhow::ensure!(!name.contains('\0'), "OCR model path contains a null character");
    let resolved = if Path::new(name).is_file() {
        name.to_string()
    } else {
        crate::index::license_consent::ensure_registry_download_allowed(name)?;
        crate::model_provisioning::resolve_model(name, Some(true))
            .map_err(|e| anyhow::anyhow!("Could not load OCR model '{name}': {e}{}", cache_problem().map(|p| format!(". {p}")).unwrap_or_default()))?
    };
    let mut file = std::fs::File::open(&resolved)
        .with_context(|| format!("OCR model '{name}' is unavailable at {resolved}"))?;
    let mut magic = [0u8; 4];
    std::io::Read::read_exact(&mut file, &mut magic)
        .with_context(|| format!("OCR model '{name}' is incomplete"))?;
    anyhow::ensure!(&magic == b"GGUF", "OCR model '{name}' is not a GGUF model");
    Ok(resolved)
}

/// A requested dewarp operation reports failures instead of silently using the
/// curved source image. Basic dewarp may refuse a page with too few baselines.
pub fn dewarp_page_checked(path: &Path, cfg: &super::OcrPipelineConfig) -> std::result::Result<(tempfile::NamedTempFile, std::path::PathBuf), String> {
    #[cfg(feature = "crispembed")]
    {
        if cfg.dewarp_engine == "tps" { resolve_checked("tps-loc").map_err(|e| format!("Dewarp: {e:#}"))?; }
        dewarp_page(path, cfg).ok_or_else(|| format!("Dewarp ({}) could not process this page. Disable dewarp or choose another engine.", cfg.dewarp_engine))
    }
    #[cfg(not(feature = "crispembed"))]
    {
        let _ = (path, cfg);
        Err("Dewarp requires a build with CrispEmbed.".into())
    }
}

/// Apply the simple cleanup controls before a legacy engine that has no native
/// pipeline stages. The advanced embedded path retains its per-stage recipes.
pub fn cleanup_page_checked(path: &Path, cfg: &super::OcrPipelineConfig) -> std::result::Result<(tempfile::NamedTempFile, std::path::PathBuf), String> {
    #[cfg(feature = "crispembed")]
    {
        let legacy = super::ocr_policy::LEGACY_ENGINES.contains(&cfg.engine.as_deref().unwrap_or("auto"));
        let recipe = if legacy { super::OcrCleanupSpec::default() }
            else { cfg.stages.first().map(|stage| stage.cleanup.clone()).unwrap_or_default() };
        let classical = if !legacy && !cfg.stages.is_empty() { recipe.enabled } else { cfg.cleanup_enabled };
        let denoise = if !legacy && !cfg.stages.is_empty() { recipe.denoise } else { cfg.denoise };
        let nafnet = if denoise {
            Some(resolve_checked(cfg.nafnet_model.as_deref().unwrap_or("nafnet-denoise")).map_err(|e| format!("Denoising: {e:#}"))?)
        } else { None };
        let cleanup = crispembed::CrispScanCleanup::new(nafnet.as_deref(), 0)
            .map_err(|e| format!("Initialize scan cleanup: {e}"))?;
        let mut params = unsafe { crispembed_sys::crispembed_scan_cleanup_defaults() };
        params.deskew = i32::from(classical && recipe.deskew);
        params.crop_borders = i32::from(classical && recipe.crop_borders);
        params.whiten_background = i32::from(classical && recipe.whiten_background);
        params.binarize = i32::from(classical && recipe.binarize);
        params.binarize_method = recipe.binarize_method;
        params.sauvola_k = recipe.sauvola_k;
        params.sauvola_window = recipe.sauvola_window;
        params.morph_kernel = recipe.morph_kernel;
        params.border_threshold = recipe.border_threshold;
        params.deskew_max_angle = recipe.deskew_max_angle;
        params.despeckle = i32::from(classical && recipe.despeckle);
        params.blackfilter = i32::from(classical && recipe.blackfilter);
        let rgb = image::open(path).map_err(|e| format!("Read scan for cleanup: {e}"))?.to_rgb8();
        let (pixels, width, height) = cleanup.process_with_params(rgb.as_raw(), rgb.width() as i32, rgb.height() as i32, 3, params)
            .map_err(|e| format!("Clean scan: {e}"))?;
        if width <= 0 || height <= 0 { return Err("Cleanup returned invalid image dimensions.".into()); }
        let (file, output) = save_rgb_temp("ocr_cleaned_", width as u32, height as u32, pixels)
            .ok_or_else(|| "Could not save the cleaned OCR image.".to_string())?;
        if classical && recipe.auto_crop {
            if let Some((x0,y0,x1,y1)) = content_bbox(&output) {
                if x0 >= 0 && y0 >= 0 && x1 > x0 && y1 > y0 && x1 <= width && y1 <= height {
                    let image = image::open(&output).map_err(|e| e.to_string())?;
                    image.crop_imm(x0 as u32, y0 as u32, (x1-x0) as u32, (y1-y0) as u32)
                        .save(&output).map_err(|e| format!("Save cropped OCR page: {e}"))?;
                }
            }
        }
        Ok((file, output))
    }
    #[cfg(not(feature = "crispembed"))]
    {
        let _ = (path, cfg);
        Err("Scan cleanup requires a build with CrispEmbed.".into())
    }
}

/// Engine ids whose C stage takes a detector in `model_a` and a recogniser in
/// `model_b`; every other engine is a single-model VLM/document model.
#[cfg(any(feature = "crispembed", test))]
fn needs_det_and_rec(eid: i32) -> bool {
    matches!(eid, 0 | 1 | 6 | 7 | 15 | 16 | 17)
}

/// Default `(model_a, model_b)` registry names for a det+rec engine.
///
/// Names come from `k_registry` in the pinned CrispEmbed `model_mgr.cpp`.
#[cfg(any(feature = "crispembed", test))]
fn stage_model_defaults(eid: i32) -> (&'static str, &'static str) {
    match eid {
        6 => (DEFAULT_DET_MODEL, "tesseract-eng"),
        7 => (DEFAULT_DET_MODEL, "parseq"),
        // `auto` lets the orchestrator resolve `tesseract-{lang}` from the LID
        // result + tess_model_dir, instead of forcing a Latin LSTM onto
        // Fraktur pages.
        15 => (DEFAULT_DET_MODEL, "auto"),
        16 => ("ppocrv6-tiny-det", "ppocrv6-tiny-rec"),
        17 => (DEFAULT_DET_MODEL, "easyocr-english-g2"),
        _ => (DEFAULT_DET_MODEL, DEFAULT_REC_MODEL),
    }
}

#[cfg(any(feature = "crispembed", test))]
fn automatic_tesseract_recognizer(eid: i32, name: &str) -> bool {
    matches!(eid, 6 | 15) && name == "auto"
}

#[cfg(feature = "crispembed")]
fn resolve_rec_model(eid: i32, name: &str) -> Result<String> {
    if automatic_tesseract_recognizer(eid, name) {
        Ok(name.into()) // Native LID sentinel, not a registry download.
    } else {
        resolve_checked(name)
    }
}

/// The flat native constructor supports only five vision engine IDs. Stage
/// expansion lets the remaining supported vision engines use the requested
/// weights without silently being remapped to Qwen2-VL.
#[cfg(any(feature = "crispembed", test))]
fn expand_flat_escalation(cfg: &super::OcrPipelineConfig) -> Result<Option<super::OcrPipelineConfig>> {
    if !cfg.stages.is_empty() || cfg.vlm_ocr_model.as_deref().is_none_or(|m| m.trim().is_empty())
        || ["got", "glm", "qwen2vl", "internvl2", "qwen3vl"].contains(&cfg.vlm_ocr_engine.as_str()) {
        return Ok(None);
    }
    anyhow::ensure!(super::ocr_policy::VISION_ENGINES.contains(&cfg.vlm_ocr_engine.as_str()),
        "Unsupported OCR escalation engine: {}", cfg.vlm_ocr_engine);
    let stage = |engine: &str| -> Result<super::OcrStageSpec> {
        let mut stage: super::OcrStageSpec = serde_json::from_value(serde_json::json!({"engine":engine}))?;
        stage.min_chars = cfg.min_chars;
        stage.min_confidence = cfg.min_confidence;
        stage.cleanup.enabled = cfg.cleanup_enabled;
        stage.cleanup.denoise = cfg.denoise;
        Ok(stage)
    };
    let mut fast = stage("dbnet_trocr")?;
    fast.det_model = cfg.det_model.clone();
    fast.rec_model = cfg.rec_model.clone();
    let mut vision = stage(&cfg.vlm_ocr_engine)?;
    vision.det_model = cfg.vlm_ocr_model.clone();
    let mut expanded = cfg.clone();
    expanded.stages = vec![fast, vision];
    Ok(Some(expanded))
}

/// Build the orchestrator: the explicit per-stage builder when `cfg.stages` is
/// non-empty (full tweakability), else the flat simple-mode pipeline.
#[cfg(feature = "crispembed")]
fn build_pipeline(cfg: &super::OcrPipelineConfig) -> Result<crispembed::CrispOcrPipeline> {
    use super::{engine_id, source_type_id};
    super::ocr_policy::validate_config(cfg).map_err(anyhow::Error::msg)?;
    super::ocr_policy::validate_runtime(cfg).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(!super::ocr_policy::LEGACY_ENGINES.contains(&cfg.engine.as_deref().unwrap_or("auto")),
        "The selected legacy OCR engine must run through the shared image dispatcher");
    let expanded = expand_flat_escalation(cfg)?;
    let cfg = expanded.as_ref().unwrap_or(cfg);
    super::ocr_policy::validate_runtime(cfg).map_err(anyhow::Error::msg)?;
    for stage in &cfg.stages {
        anyhow::ensure!(!stage.cleanup.enabled || !stage.cleanup.page_split,
            "This OCR stage requests spread splitting. Split the pages in OCR Workbench before recognition; no page was silently omitted");
        anyhow::ensure!(!stage.cleanup.enabled || !(stage.cleanup.despeckle || stage.cleanup.blackfilter || stage.cleanup.auto_crop),
            "This OCR stage requires shared despeckle/blackfilter/auto-crop preparation. Use the shared image OCR dispatcher or prepare the page in OCR Workbench first");
    }
    // Optional post-OCR punctuation/spacing restorer (resolved once).
    let punct: Option<String> = cfg
        .punct_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(resolve_checked).transpose()?;
    // Optional post-OCR truecaser + text-LID models (registry names → paths).
    let truecase: Option<String> = cfg
        .truecase_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(resolve_checked).transpose()?;
    let lid: Option<String> = cfg
        .lid_model
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(resolve_checked).transpose()?;
    // Tesseract LID-auto-select dir is a filesystem path, not a registry model.
    let tess_dir: Option<&str> = cfg.tess_model_dir.as_deref().filter(|s| !s.is_empty());
    if cfg.stages.is_empty() {
        // Simple mode (slice-A flat config).
        let det = resolve_checked(cfg.det_model.as_deref().unwrap_or(DEFAULT_DET_MODEL))?;
        let rec = resolve_checked(cfg.rec_model.as_deref().unwrap_or(DEFAULT_REC_MODEL))?;
        let nafnet = if cfg.denoise {
            Some(resolve_checked(cfg.nafnet_model.as_deref().unwrap_or(NAFNET_MODEL))?)
        } else {
            None
        };
        // Optional VLM escalation (e.g. german-ocr-3.1): when set, the chain
        // tries DBNet+TrOCR first and falls back to the VLM if the gate fails.
        let vlm: Option<String> = cfg
            .vlm_ocr_model
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(resolve_checked).transpose()?;
        let escalation_engine = vlm_engine_id(&cfg.vlm_ocr_engine);
        anyhow::ensure!(vlm.is_none() || escalation_engine >= 0, "Unsupported OCR escalation engine: {}", cfg.vlm_ocr_engine);
        return crispembed::CrispOcrPipeline::new(
            &det,
            &rec,
            nafnet.as_deref(),
            cfg.router,
            cfg.cleanup_enabled,
            cfg.min_chars,
            cfg.min_confidence,
            vlm.as_deref(),
            escalation_engine.max(0),
            punct.as_deref(),
            lid.as_deref(),
            truecase.as_deref(),
            tess_dir,
            0,
        )
        .map_err(|e| anyhow::anyhow!("CrispEmbed OCR pipeline initialization failed: {e}"));
    }

    // Advanced mode: explicit per-stage chains.
    let nafnet = if cfg.stages.iter().any(|s| s.cleanup.enabled && s.cleanup.denoise) {
        Some(resolve_checked(cfg.nafnet_model.as_deref().unwrap_or(NAFNET_MODEL))?)
    } else { None };
    let specs: Vec<crispembed::OcrStageSpec> = cfg
        .stages
        .iter()
        .map(|s| -> Result<crispembed::OcrStageSpec> {
            let eid = engine_id(&s.engine);
            anyhow::ensure!(eid >= 0, "Unsupported OCR stage engine: {}", s.engine);
            let (model_a, model_b) = if needs_det_and_rec(eid) {
                let (det_default, rec_default) = stage_model_defaults(eid);
                (
                    resolve_checked(s.det_model.as_deref().unwrap_or(det_default))?,
                    resolve_rec_model(eid, s.rec_model.as_deref().unwrap_or(rec_default))?,
                )
            } else {
                let m = s
                    .det_model
                    .clone()
                    .unwrap_or_else(|| vlm_default_model(&s.engine).to_string());
                (resolve_checked(&m)?, String::new())
            };
            Ok(crispembed::OcrStageSpec {
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
                    // Required field added in CrispEmbed v0.16.1 (the tag both
                    // workflows pin). Upstream's own `impl Default` sets it true
                    // — cross-check both skew detectors before rotating — so
                    // match that rather than invent a value. Not surfaced in our
                    // settings yet; a follow-up if anyone needs it off.
                    deskew_consensus: true,
                    denoise: s.cleanup.denoise,
                },
                det_prob_threshold: s.det_prob_threshold,
                det_box_threshold: s.det_box_threshold,
                det_target_short: s.det_target_short,
                vlm_max_tokens: s.vlm_max_tokens,
                vlm_prompt: s.vlm_prompt.clone(),
                min_chars: s.min_chars,
                min_confidence: s.min_confidence,
            })
        })
        .collect::<Result<Vec<_>>>()?;
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
    .map_err(|e| anyhow::anyhow!("CrispEmbed OCR per-stage pipeline initialization failed: {e}"))
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
    fn stage_only_vision_escalation_keeps_its_engine_and_custom_weights() {
        let config = super::super::OcrPipelineConfig {
            enabled:true, det_model:Some("custom-det.gguf".into()), rec_model:Some("custom-rec.gguf".into()),
            vlm_ocr_engine:"lfm2_vl".into(), vlm_ocr_model:Some("custom-lfm.gguf".into()),
            min_chars:0, min_confidence:0.0, cleanup_enabled:false, ..Default::default()
        };
        let expanded = expand_flat_escalation(&config).unwrap().unwrap();
        assert_eq!(expanded.stages.len(), 2);
        assert_eq!(expanded.stages[0].engine, "dbnet_trocr");
        assert_eq!(expanded.stages[0].rec_model.as_deref(), Some("custom-rec.gguf"));
        assert_eq!(expanded.stages[1].engine, "lfm2_vl");
        assert_eq!(expanded.stages[1].det_model.as_deref(), Some("custom-lfm.gguf"));
        assert_eq!(expanded.stages[1].min_chars, 0);
        assert_eq!(expanded.stages[1].min_confidence, 0.0);
        assert!(!expanded.stages[1].cleanup.enabled);
    }

    #[test]
    fn tesseract_auto_recognition_is_a_native_sentinel_only_for_tesseract() {
        assert!(automatic_tesseract_recognizer(15, "auto"));
        assert!(automatic_tesseract_recognizer(6, "auto"));
        assert!(!automatic_tesseract_recognizer(0, "auto"));
        assert!(!automatic_tesseract_recognizer(6, "custom-tesseract.gguf"));
    }

    #[test]
    fn review_diagnostics_accept_only_the_versioned_contract() {
        assert!(parse_review_snapshot(r#"{"schema":"crispembed.ocr.review","schema_version":1}"#).is_some());
        assert!(parse_review_snapshot(r#"{"schema":"crispembed.ocr.review","schema_version":2}"#).is_none());
        assert!(parse_review_snapshot(r#"{"schema":"unknown","schema_version":1}"#).is_none());
        assert!(parse_review_snapshot(r#"{"geometry":{}}"#).is_none());
        assert!(parse_review_snapshot("{interrupted").is_none());
    }

    #[test]
    fn model_cache_reuses_only_matching_models_and_retries_failed_loads() {
        let cache: ModelCache<u32> = std::sync::OnceLock::new();
        assert_eq!(with_cached_model(&cache, "first", || None, |model| Some(*model)), None);
        assert_eq!(with_cached_model(&cache, "first", || Some(7), |model| Some(*model)), Some(7));
        assert_eq!(with_cached_model(&cache, "first", || panic!("same model should stay loaded"), |model| Some(*model)), Some(7));
        assert_eq!(with_cached_model(&cache, "second", || Some(11), |model| Some(*model)), Some(11));
        assert_eq!(with_cached_model(&cache, "third", || None, |model| Some(*model)), None);
        assert_eq!(with_cached_model(&cache, "third", || Some(13), |model| Some(*model)), Some(13));
    }

    fn region(text: &str) -> super::super::ocr_render::OcrRegion {
        super::super::ocr_render::OcrRegion {
            text: text.to_owned(),
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            confidence: 1.0,
        }
    }

    fn texts(regions: &[super::super::ocr_render::OcrRegion]) -> Vec<&str> {
        regions.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn reading_order_reorders_regions() {
        // A two-column scan: the detector emits left-top, right-top, left-bottom,
        // and the pipeline says the reading order is left column then right.
        let regions = vec![region("a"), region("b"), region("c")];
        let out = apply_reading_order(regions, &[0, 2, 1]);
        assert_eq!(texts(&out), vec!["a", "c", "b"]);
    }

    #[test]
    fn empty_reading_order_leaves_the_detector_order_alone() {
        let regions = vec![region("a"), region("b")];
        let out = apply_reading_order(regions, &[]);
        assert_eq!(texts(&out), vec!["a", "b"]);
    }

    #[test]
    fn reading_order_never_drops_or_duplicates_a_region() {
        // Out-of-range, negative, and repeated indices are all ignored, and the
        // region the order forgot is still emitted. Losing OCR text to a bad
        // index would be far worse than emitting it in the wrong place.
        let regions = vec![region("a"), region("b"), region("c")];
        let out = apply_reading_order(regions, &[2, 99, -1, 2, 0]);
        assert_eq!(texts(&out), vec!["c", "a", "b"]);
        assert_eq!(out.len(), 3, "every region must survive");
    }

    // ── Pipeline → renderer mapping (model-free) ──

    // ── Table cell recogniser selection (model-free) ──

    // ── Stage engine wiring (model-free) ──

    #[test]
    fn stage_engines_split_into_det_rec_and_single_model() {
        // det + rec stages: DBNet+TrOCR, Surya, Tesseract, PARSeq, Fraktur,
        // PP-OCRv6, EasyOCR. Everything else is one GGUF.
        for eid in [0, 1, 6, 7, 15, 16, 17] {
            assert!(super::needs_det_and_rec(eid), "engine {eid} needs det+rec");
        }
        for eid in [2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 14, 18, 19] {
            assert!(!super::needs_det_and_rec(eid), "engine {eid} is single-model");
        }
    }

    #[test]
    fn stage_defaults_match_the_pinned_registry_names() {
        // A recogniser that does not exist in the pinned registry silently
        // fails to load, so the defaults are pinned here as literals.
        assert_eq!(super::stage_model_defaults(6).1, "tesseract-eng");
        assert_eq!(super::stage_model_defaults(7).1, "parseq");
        // Fraktur resolves tesseract-{lang} from LID instead of a Latin LSTM.
        assert_eq!(super::stage_model_defaults(15).1, "auto");
        assert_eq!(
            super::stage_model_defaults(16),
            ("ppocrv6-tiny-det", "ppocrv6-tiny-rec")
        );
        assert_eq!(super::stage_model_defaults(17).1, "easyocr-english-g2");
        // dbnet_trocr / surya keep the pipeline defaults.
        assert_eq!(
            super::stage_model_defaults(0),
            (DEFAULT_DET_MODEL, DEFAULT_REC_MODEL)
        );
    }

    #[cfg(feature = "crispembed")]
    #[test]
    fn vlm_defaults_cover_the_new_document_models() {
        assert_eq!(super::vlm_default_model("olmocr"), "olmocr-2-7b");
        assert_eq!(super::vlm_default_model("lfm2_vl"), "lfm2-vl");
        // `unified` dispatches on metadata, so it must not inherit a default.
        assert_eq!(super::vlm_default_model("unified"), "");
        // A permissive default; the non-commercial `qwen2vl-3b` stays selectable.
        assert_eq!(super::vlm_default_model("qwen2vl"), "german-ocr-3.1");
    }

    #[test]
    fn table_parser_never_replaces_an_explicit_model_request() {
        assert_eq!(super::table_rec_model(None), "tesseract-eng");
        assert_eq!(super::table_rec_model(Some("/models/custom-deu.gguf")), "/models/custom-deu.gguf");
        assert_eq!(super::table_rec_model(Some("qwen2vl-ocr")), "qwen2vl-ocr");
    }

    #[test]
    fn table_parser_keeps_an_explicit_tesseract_recogniser() {
        assert_eq!(super::table_rec_model(Some("tesseract-eng")), "tesseract-eng");
        assert_eq!(
            super::table_rec_model(Some("tesseract-fraktur")),
            "tesseract-fraktur"
        );
        // Resolved paths: the leaf is what identifies the model.
        assert_eq!(
            super::table_rec_model(Some("/models/ocr/tesseract-eng.gguf")),
            "/models/ocr/tesseract-eng.gguf"
        );
        assert_eq!(
            super::table_rec_model(Some("C:\\models\\tesseract-deu.gguf")),
            "C:\\models\\tesseract-deu.gguf"
        );
    }

    fn raw_region(text: &str, det: f32, rec: f32, char_conf_len: usize) -> RenderRegionInput {
        RenderRegionInput {
            text: text.to_owned(),
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            detection_confidence: det,
            rec_confidence: rec,
            char_conf_len,
        }
    }

    #[test]
    fn render_regions_follow_the_pipelines_reading_order() {
        // The detector emits both columns row by row; the pipeline's order is
        // left column then right. hOCR/ALTO/PDF receive the regions in this
        // order, so a select-and-copy of a two-column page stays readable.
        let raw = vec![
            raw_region("left-top", 0.99, 0.80, 4),
            raw_region("right-top", 0.99, 0.70, 5),
            raw_region("left-bottom", 0.99, 0.90, 6),
        ];
        let out = render_regions_in_reading_order(raw, &[0, 2, 1]);
        assert_eq!(texts(&out), vec!["left-top", "left-bottom", "right-top"]);
        // Recognition confidence wins over the (near-useless) detection score.
        assert_eq!(out[0].confidence, 0.80);
        assert_eq!(out[1].confidence, 0.90);
    }

    #[test]
    fn render_regions_without_an_order_keep_detector_order() {
        // Engines that do not compute a reading order hand back an empty list;
        // the renderer must still get every region, unchanged.
        let raw = vec![raw_region("a", 0.88, 0.0, 0), raw_region("b", 0.77, 0.0, 0)];
        let out = render_regions_in_reading_order(raw, &[]);
        assert_eq!(texts(&out), vec!["a", "b"]);
        // No per-char confidence → fall back to the detection score.
        assert_eq!(out[0].confidence, 0.88);
    }

    #[test]
    fn render_regions_survive_a_malformed_order() {
        // Same defensive contract as apply_reading_order: a bad index can
        // reorder text, but it must never drop or duplicate it.
        let raw = vec![
            raw_region("a", 1.0, 0.0, 0),
            raw_region("b", 1.0, 0.0, 0),
            raw_region("c", 1.0, 0.0, 0),
        ];
        let out = render_regions_in_reading_order(raw, &[2, 99, -1, 2, 0]);
        assert_eq!(texts(&out), vec!["c", "a", "b"]);
        assert_eq!(out.len(), 3);
    }

    #[cfg(feature = "crispembed")]
    #[test]
    fn pipeline_result_glue_reads_reading_order_and_confidence() {
        // Constructs the native result type directly: no models, no OCR run.
        // Guards the field mapping that the live tests can only shape-check.
        let res = crispembed::OcrPipelineResult {
            regions: vec![
                crispembed::OcrPipelineRegion {
                    text: "first".into(),
                    x: 10.0,
                    y: 20.0,
                    w: 30.0,
                    h: 40.0,
                    confidence: 0.99,
                    rec_confidence: 0.42,
                    char_conf: vec![0.4, 0.5],
                },
                crispembed::OcrPipelineRegion {
                    text: "second".into(),
                    x: 50.0,
                    y: 60.0,
                    w: 70.0,
                    h: 80.0,
                    confidence: 0.99,
                    rec_confidence: 0.0,
                    char_conf: Vec::new(),
                },
            ],
            full_text: "second\nfirst".into(),
            mean_confidence: 0.7,
            reading_order: vec![1, 0],
            markdown: String::new(),
        };
        let out = render_regions_from_pipeline(res);
        assert_eq!(texts(&out), vec!["second", "first"]);
        assert_eq!((out[0].x, out[0].y, out[0].w, out[0].h), (50.0, 60.0, 70.0, 80.0));
        assert_eq!(out[0].confidence, 0.99, "no char_conf → detection score");
        assert_eq!(out[1].confidence, 0.42, "char_conf present → recognition score");
    }

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
        let result = ocr_via_crispembed_custom(&img_path, DEFAULT_DET_MODEL, DEFAULT_REC_MODEL);
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

    /// Actual released-model inference through the application's production
    /// workbench path. Kept separate from model-free regressions because it
    /// downloads weights and exercises four model configurations on a CPU.
    #[cfg(feature = "crispembed")]
    #[test]
    #[ignore = "downloads OCR weights; run on an admitted inference worker"]
    fn ocr_task_presets_live() {
        let image = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/quality/ocr-task.png");
        let reference: serde_json::Value = serde_json::from_slice(&std::fs::read(image.with_extension("json")).unwrap()).unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(format!("{:x}", Sha256::digest(std::fs::read(&image).unwrap())), reference["sha256"].as_str().unwrap());
        let mut models = Vec::new();
        for name in ["dbnet-det", "trocr-printed", "qwen2vl-3b"] {
            use std::io::Read;
            let path = crate::model_provisioning::resolve_model(name, Some(true)).expect("released OCR model must resolve");
            let mut file = std::fs::File::open(&path).unwrap();
            let bytes = file.metadata().unwrap().len();
            let mut digest = Sha256::new();
            let mut block = [0u8; 65536];
            loop {
                let count = file.read(&mut block).unwrap();
                if count == 0 { break; }
                digest.update(&block[..count]);
            }
            models.push(serde_json::json!({"registry_name":name,"bytes":bytes,"sha256":format!("{:x}",digest.finalize())}));
        }
        let mut cases = Vec::new();
        for preset in ["quick", "multilingual", "tables", "difficult"] {
            let config = super::super::ocr_presets::apply(&Default::default(), preset).unwrap();
            let started = std::time::Instant::now();
            let regions = ocr_regions_detailed(&image, &config).expect("released OCR task should run through the app binding");
            let text = regions.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(" ");
            let normalized = text.to_lowercase();
            let matched = ["cobalt", "warranty", "2031"].iter().all(|word| normalized.contains(word));
            cases.push(serde_json::json!({"preset":preset,"text":text,"matched":matched,"elapsed_seconds":started.elapsed().as_secs_f64(),"config":config}));
        }
        let receipt = serde_json::json!({"scope":"Synthetic native app wiring smoke, not a corpus accuracy claim; timings exclude initial downloads", "models":models,"cases":cases});
        println!("{receipt}");
        if let Ok(path) = std::env::var("CRISPSORTER_OCR_RECEIPT") {
            std::fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
        }
        assert!(cases.iter().all(|case| case["matched"] == true), "OCR task failed to retain the known fixture evidence: {receipt}");
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
        // A real zero must not turn into a confident detector score.
        assert_eq!(super::effective_confidence(0.88, 0.0, 3), 0.0);
        assert_eq!(super::effective_confidence(f32::NAN, f32::NAN, 0), -1.0);
        assert_eq!(super::effective_confidence(0.75, f32::INFINITY, 2), 0.75);
    }

    #[test]
    fn review_recognition_preserves_the_prepared_coordinate_space() {
        let mut cfg = super::super::OcrPipelineConfig {
            cleanup_enabled: true, denoise: true, dewarp: true, restore: true, sr: true,
            ..Default::default()
        };
        cfg.stages.push(serde_json::from_value(serde_json::json!({
            "engine":"tesseract","cleanup":{"enabled":true,"denoise":true}
        })).unwrap());
        let review = super::review_pipeline_config(&cfg);
        assert!(!review.cleanup_enabled && !review.denoise && !review.dewarp && !review.restore && !review.sr);
        assert!(!review.stages[0].cleanup.enabled && !review.stages[0].cleanup.denoise);
        assert_eq!(review.stages[0].engine, "tesseract");
        assert!(cfg.cleanup_enabled && cfg.stages[0].cleanup.enabled, "caller profile must remain intact");
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
        let mut pipe = super::build_pipeline(&cfg).expect("pipeline should initialize");
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
