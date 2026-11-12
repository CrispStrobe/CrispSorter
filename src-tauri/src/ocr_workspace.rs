//! Durable, non-destructive OCR review projects.
//!
//! Source files are never edited. Imports and page transformations create
//! immutable, content-addressed PNG assets under the application data directory.
//! Project saves use revision checks and same-directory atomic replacement;
//! exports use the reviewed pixels and text, never silently rerun recognition.

use crate::extractors::ocr_render::{OcrOutputFormat, OcrRegion, RenderPage};
use crate::index::tauri_commands::KieFieldDto;
use crate::AppState;
use crate::operation_log::OperationLog;
use crate::ocr_photo::{PhotoAssessment, PhotoPoint};
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager, State};

#[path = "ocr_document_import.rs"]
mod document_import;
use crate::ocr_import_payload::{self as import_payload, ImportMetadata};
use crate::ocr_atomic_output as atomic_output;
use atomic_output::{atomic_write, atomic_write_with, sync_directory};
#[path = "ocr_export_control.rs"]
mod export_control;
use export_control::{ControlledWriter, ExportControl, ExportJob};
#[path = "ocr_asset_store.rs"]
mod asset_store;

const PROJECT_VERSION: u64 = 1;
const MAX_PROJECT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PAGE_PIXELS: u64 = 100_000_000;
const MAX_PAGES: usize = 10_000;
const MAX_REGIONS: usize = 100_000;
const MAX_CAPTURE_BYTES: usize = 32 * 1024 * 1024;
static PROJECT_WRITER: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageAsset {
    pub image_path: String,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_width_points: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_height_points: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

fn checked_physical_size(width: Option<f64>, height: Option<f64>) -> Result<Option<(f64, f64)>, String> {
    match (width, height) {
        (None, None) => Ok(None),
        (Some(w), Some(h)) if [w, h].iter().all(|n| n.is_finite() && *n > 0.0 && *n <= 14_400.0) => Ok(Some((w, h))),
        _ => Err("Physical page dimensions must both be positive and no larger than 14400 PDF points (200 inches).".into()),
    }
}

fn set_physical_size(page: &mut PageAsset, size: Option<(f64, f64)>) {
    page.page_width_points = size.map(|(w, _)| w);
    page.page_height_points = size.map(|(_, h)| h);
}

#[derive(Debug, Serialize)]
pub struct ImportedDocument {
    pub source_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_uri: Option<String>,
    pub name: String,
    pub pages: Vec<PageAsset>,
}

#[derive(Debug, Serialize)]
pub struct TransformedPages {
    pub pages: Vec<PageAsset>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PixelRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedProject {
    pub schema_version: u64,
    pub project: Value,
    pub revision: u64,
    pub saved_at: u64,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub updated_at: u64,
    pub page_count: usize,
}

fn workspace_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let root = app.path().app_data_dir().map_err(|e| e.to_string())?.join("ocr-workspace");
    fs::create_dir_all(root.join("assets")).map_err(|e| format!("Create OCR asset directory: {e}"))?;
    fs::create_dir_all(root.join("projects")).map_err(|e| format!("Create OCR project directory: {e}"))?;
    fs::canonicalize(root).map_err(|e| format!("Open OCR workspace: {e}"))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn valid_project_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn local_file(uri: &str) -> Result<PathBuf, String> {
    let path = crate::images::tauri_commands::location_uri_to_local_path(uri)
        .ok_or_else(|| "Select a local file or download the cloud document first.".to_string())?;
    let path = fs::canonicalize(path).map_err(|e| format!("Open source file: {e}"))?;
    if !path.is_file() {
        return Err("The selected source is not a file.".into());
    }
    Ok(path)
}

fn managed_page(root: &Path, path: &str) -> Result<PathBuf, String> {
    let assets = fs::canonicalize(root.join("assets")).map_err(|e| format!("Open page cache: {e}"))?;
    let path = fs::canonicalize(path).map_err(|e| format!("Open page snapshot: {e}. Reimport the source to restore missing pixels."))?;
    if !path.starts_with(assets) || !path.is_file() {
        return Err("Import the image into this OCR workspace before processing or exporting it.".into());
    }
    Ok(path)
}

fn checked_dimensions(path: &Path) -> Result<(u32, u32), String> {
    let (width, height) = image::image_dimensions(path)
        .map_err(|e| format!("Read page dimensions ({}): {e}", path.display()))?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PAGE_PIXELS {
        return Err(format!("Page dimensions {width} × {height} exceed the supported 100 megapixel limit."));
    }
    Ok((width, height))
}

fn decode_page(path: &Path) -> Result<DynamicImage, String> {
    crate::extractors::image_input::validate_file(path)
        .map_err(|e| format!("Validate page source ({}): {e}", path.display()))?;
    checked_dimensions(path)?;
    image::open(path).map_err(|e| format!("Decode page ({}): {e}", path.display()))
}

/// Bake EXIF orientation into the imported pixels. Browsers otherwise rotate
/// JPEG previews independently of the recognizer's pixel coordinates.
fn orient_image(image: DynamicImage, orientation: u32) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate90().flipv(),
        8 => image.rotate270(),
        _ => image,
    }
}

/// Commit a fully encoded page before returning its path. Reusing the same
/// pixels never replaces an existing asset that another project references.
fn persist_image(root: &Path, image: &DynamicImage) -> Result<PageAsset, String> {
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PAGE_PIXELS {
        return Err("Transformed page exceeds the 100 megapixel limit.".into());
    }
    let assets = asset_store::directory(root).map_err(|e| format!("Open private page cache: {e}"))?;
    let mut staging = tempfile::NamedTempFile::new_in(&assets)
        .map_err(|e| format!("Create page snapshot: {e}"))?;
    // A recognizer that asks for RGB drops alpha, whereas the preview browser
    // composites it. Flatten onto white paper once so both see identical pixels.
    let canonical = if image.color().has_alpha() {
        std::borrow::Cow::Owned(opaque_page(image))
    } else { std::borrow::Cow::Borrowed(image) };
    canonical.write_to(staging.as_file_mut(), image::ImageFormat::Png)
        .map_err(|e| format!("Encode page snapshot: {e}"))?;
    staging.as_file_mut().flush().map_err(|e| e.to_string())?;
    staging.as_file().sync_all().map_err(|e| e.to_string())?;
    staging.as_file_mut().seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = staging.read(&mut buffer).map_err(|e| format!("Hash page snapshot: {e}"))?;
        if n == 0 { break; }
        hash.update(&buffer[..n]);
    }
    let path = assets.join(format!("{:x}.png", hash.finalize()));
    match staging.persist_noclobber(&path) {
        Ok(_) => sync_directory(&assets)?,
        Err(mut e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            asset_store::verify_existing(&path, e.file.as_file_mut())
                .map_err(|error| format!("Verify existing page snapshot: {error} The existing asset was retained; restore the page cache before retrying."))?;
        },
        Err(e) => return Err(format!("Save page snapshot: {}", e.error)),
    }
    Ok(PageAsset { image_path: path.to_string_lossy().into_owned(), width, height,
        page_width_points: None, page_height_points: None, warnings: Vec::new() })
}

fn opaque_page(image: &DynamicImage) -> DynamicImage {
    if !image.color().has_alpha() { return image.clone(); }
    let rgba = image.to_rgba8();
    DynamicImage::ImageRgb8(image::RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let source = rgba.get_pixel(x, y);
        let alpha = u16::from(source[3]);
        image::Rgb([0, 1, 2].map(|channel| {
            ((u16::from(source[channel]) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
        }))
    }))
}

/// Workbench and extraction share the same embedded catalog. Custom-model
/// engines remain configurable, and experiment gates control availability.
const WORKSPACE_ENGINES: &[&str] = crate::extractors::ocr_policy::EMBEDDED_ENGINES;

fn offered_engines(native: bool, system: bool, ocrs: bool, paddle: bool) -> Vec<&'static str> {
    let mut engines = Vec::new();
    if native || system || ocrs || paddle { engines.push("auto"); }
    if native {
        engines.extend(WORKSPACE_ENGINES.iter().copied().filter(|engine| match *engine {
            "unlimited_ocr2" => std::env::var("CRISPEMBED_UNLIMITED_OCR2").ok().as_deref() == Some("1"),
            "minicpm_ocr" => ["CRISPEMBED_MINICPM_OCR", "CRISPEMBED_QWEN35_OCR"].iter()
                .all(|gate| std::env::var(gate).ok().as_deref() == Some("1")),
            _ => true,
        }));
    }
    if system { engines.push("system_tesseract"); }
    if ocrs { engines.push("ocrs"); }
    if paddle { engines.push("paddle"); }
    engines
}

#[tauri::command]
pub fn ocr_workspace_capabilities() -> Value {
    let native = cfg!(feature = "crispembed");
    let system = crate::ocr_word_evidence::capabilities();
    // Offered when compiled: the model manager installs missing weights.
    let ocrs = crate::extractors::ocr_ocrs::is_ocrs_offered();
    let paddle = crate::extractors::ocr_paddle::is_paddle_ocr_available();
    let engines = offered_engines(native, system.available, ocrs, paddle);
    let available = native || system.available || ocrs || paddle;
    let mut formats = vec!["text", "markdown", "html", "json"];
    if native { formats.extend(["hocr", "alto"]); }
    if reviewed_pdf_available() { formats.push("pdf"); }
    if crate::extractors::ocr_render::reviewed_archival_pdf_available() { formats.push("pdfa"); }
    // Structured Office export supersedes the text-only Word writer when built.
    if cfg!(feature = "ocr-office") { formats.extend(["docx", "xlsx"]); }
    else if cfg!(feature = "ocr-text-export") { formats.push("docx"); }
    json!({
        "schema_version": PROJECT_VERSION,
        "ocr": available, "crispembed": native,
        "system_tesseract": system,
        "camera_import": true, "photo_tools": cfg!(feature = "ocr-photo-tools"),
        "mobile_model_store": crate::model_provisioning::enabled(),
        "default_engine": if crate::model_provisioning::enabled() { "tesseract" } else { "auto" },
        "default_language": if crate::model_provisioning::enabled() { "eng" } else { "auto" },
        "pdf_import": cfg!(any(feature = "pdf-render", target_os = "android", target_os = "ios")),
        "native_document_import": cfg!(any(target_os = "android", target_os = "ios")),
        "pdf_export_reason": if reviewed_pdf_available() { None } else { Some("Searchable PDF requires the matching updated CrispEmbed renderer and an OCR workspace build. Export hOCR, ALTO, text, or JSON with this build.") },
        "archival_pdf_export": crate::extractors::ocr_render::reviewed_archival_pdf_available(),
        "pdf_bidi_export": crate::extractors::ocr_render::reviewed_bidi_pdf_available(),
        "pdf_bidi_export_reason": if crate::extractors::ocr_render::reviewed_bidi_pdf_available() { None } else { Some(crate::ocr_text_export::PDF_BIDI_UNAVAILABLE) },
        "docx_export": cfg!(feature = "ocr-text-export"),
        "docx_export_reason": if cfg!(feature = "ocr-text-export") { None } else { Some("Word export requires the OCR text export feature in this build. Plain text and HTML document remain available.") },
        "pdfa_conformance": if crate::extractors::ocr_render::reviewed_archival_pdf_available() { Some("PDF/A-2b") } else { None },
        "pdfa_export_reason": if crate::extractors::ocr_render::reviewed_archival_pdf_available() { None } else { Some("PDF/A-2b export requires a matching native renderer with archival resources. Choose another available output format with this build.") },
        "cleanup": native, "layout": native, "kie": native, "table": native, "math": native,
        "structured_export": native, "formats": formats,
        "structured_docx": cfg!(feature = "ocr-office"),
        "structured_xlsx": cfg!(feature = "ocr-office"),
        "review_diagnostics": cfg!(feature = "ocr-review-diagnostics"),
        // Isolated low-memory Qwen2-VL worker (see `ocr_worker::run_cpu_budget`).
        "cpu_budget": native && crate::extractors::ocr_worker::CPU_BUDGET_AVAILABLE,
        "cpu_budget_pixels": crate::extractors::ocr_worker::CPU_PIXEL_BUDGET,
        "engines": engines,
        "reason": if available { None } else { Some("Recognition requires an available embedded or installed OCR engine. Project editing, image/TIFF import, and text exports remain available.") },
        "pdf_reason": if cfg!(any(feature = "pdf-render", target_os = "android", target_os = "ios")) { None } else { Some("PDF import requires a build with PDF rendering enabled. Import page images or a TIFF instead.") },
        "max_page_pixels": MAX_PAGE_PIXELS,
    })
}

fn reviewed_pdf_available() -> bool {
    crate::extractors::ocr_render::reviewed_pdf_render_available()
}

#[tauri::command]
pub async fn ocr_workspace_import(app: tauri::AppHandle, location_uri: String, request_id: Option<String>) -> Result<ImportedDocument, String> {
    let log=OperationLog::start_with_id("ocr.import",json!({"surface":"gui"}),request_id.as_deref());
    let result: Result<ImportedDocument,String> = async {
    log.step("validating",Value::Null);
    let root = workspace_root(&app)?;
    let path = local_file(&location_uri)?;
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let bytes = tokio::task::spawn_blocking(move || {
            let file = File::open(path).map_err(|e| format!("Open selected document: {e}"))?;
            if file.metadata().map_err(|e| e.to_string())?.len() > MAX_CAPTURE_BYTES as u64 { return Err("This document exceeds the 32 MiB mobile import limit.".to_string()); }
            let mut bytes = Vec::new();
            file.take(MAX_CAPTURE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            Ok(bytes)
        }).await.map_err(|e| format!("Read import worker: {e}"))??;
        let metadata = ImportMetadata { name, source_uri: None, request_id };
        let mut document = document_import::from_bytes(app, root, metadata, bytes, true).await?;
        document.source_uri = location_uri;
        Ok(document)
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let job = document_import::ImportJob::begin(app, request_id)?;
        tokio::task::spawn_blocking(move || import_document_with_progress(&root, &path, Some(&job))).await
            .map_err(|e| format!("Import worker: {e}"))?
    }
    }.await;
    finish_import_log(&log,&result);
    result
}

/// Camera and provider bytes never become paths supplied by the webview. Keep
/// their originals by content hash before creating the oriented page assets.
#[tauri::command]
pub async fn ocr_workspace_import_image(app: tauri::AppHandle, request: tauri::ipc::Request<'_>) -> Result<ImportedDocument, String> {
    let log=OperationLog::start("ocr.import_image",json!({"surface":"gui","kind":"image"}));
    let result: Result<ImportedDocument,String> = async {
    let (mut metadata, bytes) = import_request(&request)?;
    let root = workspace_root(&app)?;
    metadata.source_uri = None;
    document_import::from_bytes(app, root, metadata, bytes, false).await
    }.await;
    finish_import_log(&log,&result);
    result
}

#[tauri::command]
pub async fn ocr_workspace_import_bytes(app: tauri::AppHandle, request: tauri::ipc::Request<'_>) -> Result<ImportedDocument, String> {
    let log=OperationLog::start("ocr.import_bytes",json!({"surface":"gui"}));
    let result: Result<ImportedDocument,String> = async {
    let (metadata, bytes) = import_request(&request)?;
    let root = workspace_root(&app)?;
    document_import::from_bytes(app, root, metadata, bytes, true).await
    }.await;
    finish_import_log(&log,&result);
    result
}

fn finish_import_log(log:&OperationLog,result:&Result<ImportedDocument,String>) {
    match result {
        Ok(document)=>log.succeed(json!({"pages":document.pages.len()})),
        Err(error) if error == "OCR_IMPORT_CANCELLED"=>log.cancel(),
        Err(_)=>log.fail("import_failed"),
    }
}

#[tauri::command]
pub fn ocr_workspace_cancel_import(request_id: String) -> Result<(), String> {
    let log=OperationLog::start("ocr.import_cancel",json!({"surface":"gui"}));
    let result=document_import::cancel(&request_id);
    match &result {Ok(())=>log.succeed(json!({"accepted":true})),Err(_)=>log.fail("cancel_request_failed")}
    result
}

fn import_request(request: &tauri::ipc::Request<'_>) -> Result<(ImportMetadata, Vec<u8>), String> {
    // Reject malformed headers before copying or decoding a large document.
    let metadata = import_payload::decode_metadata(
        request.headers().get("x-ocr-metadata").map(|header| header.as_bytes()),
    )?;
    // Android (and desktop custom-protocol fallback) uses postMessage, which
    // requires JSON. New Android clients send base64; legacy typed bytes arrive
    // as arrays. Tauri cannot deliver InvokeBody::Raw there.
    let bytes = match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => import_payload::copy_raw(bytes, MAX_CAPTURE_BYTES)?,
        tauri::ipc::InvokeBody::Json(value) => import_payload::decode_json(value, MAX_CAPTURE_BYTES)?,
    };
    Ok((metadata, bytes))
}

fn import_bytes(root: &Path, name: &str, bytes: &[u8], allow_pdf: bool, source_uri: Option<&str>) -> Result<ImportedDocument, String> {
    let archived = document_import::archive(root, name, bytes, allow_pdf, source_uri)?;
    import_document(root, &archived.path).map(|document| archived.attach(document))
}

#[tauri::command]
pub async fn ocr_workspace_assess_photo(app: tauri::AppHandle, page_path: String) -> Result<PhotoAssessment, String> {
    let root = workspace_root(&app)?;
    let path = managed_page(&root, &page_path)?;
    tokio::task::spawn_blocking(move || {
        let image = decode_page(&path)?;
        if image.color().has_alpha() { crate::ocr_photo::assess(&opaque_page(&image)) }
        else { crate::ocr_photo::assess(&image) }
    }).await
        .map_err(|e| format!("Photo assessment worker: {e}"))?
}

fn import_document(root: &Path, path: &Path) -> Result<ImportedDocument, String> {
    import_document_with_progress(root, path, None)
}

fn import_document_with_progress(root: &Path, path: &Path, job: Option<&document_import::ImportJob>) -> Result<ImportedDocument, String> {
    let log=OperationLog::start("ocr.import_pages",Value::Null);
    let result=(|| {
    log.step("rendering_pages",Value::Null);
    if let Some(job) = job { job.check()?; job.progress("rendering", 0, None); }
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    if !["pdf", "png", "jpg", "jpeg", "tif", "tiff", "bmp", "webp"].contains(&ext.as_str()) {
        return Err(format!("Unsupported OCR document type: .{ext}"));
    }
    let images = if ext == "pdf" {
        crate::extractors::page_source::rasterize_pdf(path)
    } else {
        crate::extractors::page_source::rasterize_pages(path, &ext)
    }.map_err(|e| format!("Import document: {e:#}"))?;
    if images.is_empty() || images.len() > MAX_PAGES {
        return Err(format!("A project import must contain between 1 and {MAX_PAGES} pages."));
    }
    let mut pages = Vec::with_capacity(images.len());
    for (index, page) in images.paths().iter().enumerate() {
        log.step("verifying_page",json!({"page":index+1,"pages":images.len()}));
        if let Some(job) = job { job.check()?; job.progress("verifying", index, Some(images.len())); }
        let img = decode_page(page).map_err(|e| format!("Page {}: {e}", index + 1))?;
        let orientation = images.page_orientation(index);
        let mut page = persist_image(root, &orient_image(img, orientation))?;
        // TIFF/PDF assets are already oriented; single images still carry their
        // source orientation, so swap their physical axes with their pixels.
        let (width_points, height_points) = images.page_size_points(index)
            .map(|(w, h)| if (5..=8).contains(&orientation) { (h, w) } else { (w, h) })
            // Only sources without reliable resolution use the 300 DPI policy.
            .unwrap_or((f64::from(page.width) * 72.0 / 300.0, f64::from(page.height) * 72.0 / 300.0));
        let size = checked_physical_size(Some(width_points), Some(height_points))?;
        set_physical_size(&mut page, size);
        pages.push(page);
    }
    if let Some(job) = job { job.check()?; job.progress("complete", pages.len(), Some(pages.len())); }
    Ok(ImportedDocument {
        source_uri: path.to_string_lossy().into_owned(),
        archive_uri: None,
        name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        pages,
    })
    })();
    finish_import_log(&log,&result);
    result
}

fn checked_rect(rect: PixelRect, width: u32, height: u32) -> Result<(u32, u32, u32, u32), String> {
    if ![rect.x, rect.y, rect.w, rect.h].iter().all(|n| n.is_finite())
        || rect.x < 0.0 || rect.y < 0.0 || rect.w <= 0.0 || rect.h <= 0.0
        || rect.x + rect.w > f64::from(width) + 0.01
        || rect.y + rect.h > f64::from(height) + 0.01 {
        return Err("The selected area must be a nonempty rectangle inside the page.".into());
    }
    let x = rect.x.floor() as u32;
    let y = rect.y.floor() as u32;
    let right = (rect.x + rect.w).ceil().min(f64::from(width)) as u32;
    let bottom = (rect.y + rect.h).ceil().min(f64::from(height)) as u32;
    if right <= x || bottom <= y { return Err("The selected area is smaller than one pixel.".into()); }
    Ok((x, y, right - x, bottom - y))
}

#[tauri::command]
pub async fn ocr_workspace_transform(
    app: tauri::AppHandle,
    page_path: String,
    operation: String,
    degrees: Option<i32>,
    crop: Option<PixelRect>,
    split_x: Option<u32>,
    quad: Option<Vec<PhotoPoint>>,
    angle: Option<f64>,
    page_width_points: Option<f64>,
    page_height_points: Option<f64>,
) -> Result<TransformedPages, String> {
    let log=OperationLog::start("ocr.transform",json!({"surface":"gui"}));
    let result: Result<TransformedPages,String> = async {
    let root = workspace_root(&app)?;
    let path = managed_page(&root, &page_path)?;
    let size = checked_physical_size(page_width_points, page_height_points)?;
    tokio::task::spawn_blocking(move || transform_page_with_photo(&root, &path, &operation, degrees, crop, split_x, size, quad, angle)).await
        .map_err(|e| format!("Page transform worker: {e}"))?
    }.await;
    match &result {Ok(pages)=>log.succeed(json!({"pages":pages.pages.len()})),Err(_)=>log.fail("transform_failed")}
    result
}

fn transform_page(root: &Path, path: &Path, operation: &str, degrees: Option<i32>, crop: Option<PixelRect>, split_x: Option<u32>, physical_size: Option<(f64, f64)>) -> Result<TransformedPages, String> {
    transform_page_with_photo(root, path, operation, degrees, crop, split_x, physical_size, None, None)
}

fn transform_page_with_photo(root: &Path, path: &Path, operation: &str, degrees: Option<i32>, crop: Option<PixelRect>, split_x: Option<u32>, physical_size: Option<(f64, f64)>, quad: Option<Vec<PhotoPoint>>, angle: Option<f64>) -> Result<TransformedPages, String> {
    let image = decode_page(path)?;
    let output = match operation {
        "rotate" => vec![match degrees.unwrap_or(90).rem_euclid(360) {
            90 => image.rotate90(), 180 => image.rotate180(), 270 => image.rotate270(),
            _ => return Err("Choose a rotation of 90, 180, or 270 degrees.".into()),
        }],
        "crop" => {
            let (x, y, w, h) = checked_rect(crop.ok_or("Choose a crop area.")?, image.width(), image.height())?;
            vec![image.crop_imm(x, y, w, h)]
        },
        "split" => {
            let x = split_x.unwrap_or(image.width() / 2);
            if x == 0 || x >= image.width() { return Err("The split must be inside the page.".into()); }
            vec![image.crop_imm(0, 0, x, image.height()), image.crop_imm(x, 0, image.width() - x, image.height())]
        },
        "perspective" | "deskew" => vec![crate::ocr_photo::transform(&image, operation, quad, angle)?],
        _ => return Err(format!("Unknown page operation: {operation}")),
    };
    let mut pages = output.iter().map(|img| persist_image(root, img)).collect::<Result<Vec<_>, _>>()?;
    for page in &mut pages {
        let size = physical_size.map(|(width, height)| {
            if operation == "rotate" {
                if degrees.unwrap_or(90).rem_euclid(180) == 90 { (height, width) } else { (width, height) }
            } else {
                (width * f64::from(page.width) / f64::from(image.width()), height * f64::from(page.height) / f64::from(image.height()))
            }
        });
        set_physical_size(page, size);
    }
    Ok(TransformedPages { pages })
}

#[derive(Serialize)]
pub struct PreparedPage {
    #[serde(flatten)]
    pub page: PageAsset,
    pub warnings: Vec<String>,
}

fn active_preparation_config(mut config: crate::extractors::OcrPipelineConfig) -> crate::extractors::OcrPipelineConfig {
    if let Some(engine) = config.engine.as_deref().filter(|engine| !engine.is_empty() && *engine != "auto") {
        config.stages = config.stages.iter().find(|stage| stage.engine == engine).cloned().into_iter().collect();
    }
    config
}

#[tauri::command]
pub async fn ocr_workspace_prepare(
    app: tauri::AppHandle,
    page_path: String,
    config: crate::extractors::OcrPipelineConfig,
    page_width_points: Option<f64>,
    page_height_points: Option<f64>,
    allow_downloads: Option<bool>,
) -> Result<PreparedPage, String> {
    let log=std::sync::Arc::new(OperationLog::start("ocr.workspace_prepare",json!({"surface":"gui","engine":config.engine})));
    let result: Result<PreparedPage,String> = async {
    log.step("validating",Value::Null);
    let root = workspace_root(&app)?;
    let path = managed_page(&root, &page_path)?;
    let config = active_preparation_config(config);
    validate_config(&config)?;
    let size = checked_physical_size(page_width_points, page_height_points)?;
    let worker_log=log.clone();
    tokio::task::spawn_blocking(move || {
        // Models are downloaded only after the user agreed for this request,
        // through the model manager; preparation itself never downloads.
        let plan = crate::ocr_models::plan(&config, &[]);
        let preparation: Vec<_> = plan.models.into_iter().filter(|m| m.engine == "preparation").collect();
        crate::ocr_models::ensure_or_gate(&preparation, allow_downloads.unwrap_or(false), &worker_log)?;
        let progress = |stage: &str| {
            let code = if stage.starts_with("Straightening") {"dewarp"}
                else if stage.starts_with("Image restoration") {"restore"}
                else if stage.starts_with("Upscaling") {"upscale"}
                else if stage.starts_with("Learned denoise") {"denoise"}
                else if stage.starts_with("Deskew") {"cleanup"} else {"saving_page"};
            worker_log.step(code,Value::Null);
            let _ = app.emit("ocr-workspace-prepare-stage", json!({ "page": page_path, "stage": stage }));
        };
        let result = prepare_page_with_progress(&root, &path, &config, size, &progress);
        result
    }).await
        .map_err(|e| format!("Page preparation worker: {e}"))?
    }.await;
    match &result {
        Ok(prepared) => log.succeed(json!({"warnings":prepared.warnings.len(),"width":prepared.page.width,"height":prepared.page.height})),
        Err(_) => log.fail("preparation_failed"),
    }
    result
}

/// Error prefix the UI turns into a download confirmation; JSON follows.
pub const MODEL_DOWNLOAD_REQUIRED: &str = "MODEL_DOWNLOAD_REQUIRED:";

fn prepare_page(root: &Path, source: &Path, config: &crate::extractors::OcrPipelineConfig, physical_size: Option<(f64, f64)>) -> Result<PreparedPage, String> {
    prepare_page_with_progress(root, source, config, physical_size, &|_| {})
}

fn prepare_page_with_progress(root: &Path, source: &Path, config: &crate::extractors::OcrPipelineConfig, mut physical_size: Option<(f64, f64)>, progress: &dyn Fn(&str)) -> Result<PreparedPage, String> {
    use crate::extractors::ocr_crispembed;
    let mut path = source.to_path_buf();
    let mut guards = Vec::new();
    let mut warnings = Vec::new();
    checked_dimensions(&path)?;
    if config.dewarp {
        progress("Straightening curved lines");
        let (guard, output) = ocr_crispembed::dewarp_page_checked(&path, config)?;
        path = output; guards.push(guard);
    }
    if config.restore {
        progress(&format!("Image restoration ({})", if config.restore_engine.is_empty() { "restormer" } else { config.restore_engine.as_str() }));
        let (guard, output) = ocr_crispembed::restore_page_checked(&path, config)?;
        path = output; guards.push(guard);
    }
    if config.sr {
        let (w, h) = checked_dimensions(&path)?;
        if w.min(h) as i32 <= config.sr_max_short_side {
            if u64::from(w) * u64::from(h) * 16 > MAX_PAGE_PIXELS {
                return Err("A 4× upscale would exceed the 100 megapixel page limit. Crop the page or disable super-resolution.".into());
            }
            progress(&format!("Upscaling ({})", if config.sr_engine.is_empty() { "pan" } else { config.sr_engine.as_str() }));
            let (guard, output) = ocr_crispembed::super_resolve_page_checked(&path, config)?;
            path = output; guards.push(guard);
        }
    }
    let image = decode_page(&path)?;
    // Restoration and super-resolution change the sampling density, not the
    // physical sheet size. Only the subsequent pixel crop changes the paper.
    let before_cleanup = (image.width(), image.height());
    let recipe = config.stages.first().map(|s| s.cleanup.clone()).unwrap_or_default();
    if config.stages.iter().any(|s| s.cleanup.enabled && s.cleanup.page_split) {
        warnings.push("This profile requests spread splitting. Use Split page to create and review both halves; recognition retains the complete current page.".into());
    }
    let use_cleanup = config.stages.first().map(|s| s.cleanup.enabled).unwrap_or(config.cleanup_enabled);
    let denoise = config.stages.first().map(|s| s.cleanup.denoise).unwrap_or(config.denoise);
    if denoise { progress("Learned denoise (NAFNet) — can take a while on large pages"); }
    else if use_cleanup { progress("Deskew and clean"); }
    let image = if use_cleanup || denoise {
        if config.stages.len() > 1 && config.stages.iter().any(|s| s.cleanup.enabled) {
            warnings.push("Review uses the first stage's cleanup recipe for one shared page image. Internal stage cleanup is disabled so every recognition box aligns with that image.".into());
        }
        cleanup_image(image, config, &recipe, use_cleanup, denoise)?
    } else { image };
    physical_size = physical_size.map(|(w, h)| (w * f64::from(image.width()) / f64::from(before_cleanup.0), h * f64::from(image.height()) / f64::from(before_cleanup.1)));
    progress("Saving the prepared page");
    let page = persist_image(root, &image)?;
    let page = if use_cleanup && recipe.auto_crop {
        if let Some((left, top, right, bottom)) = ocr_crispembed::content_bbox(Path::new(&page.image_path)) {
            let rect = PixelRect { x: left as f64, y: top as f64, w: (right - left) as f64, h: (bottom - top) as f64 };
            let (x, y, w, h) = checked_rect(rect, page.width, page.height)
                .map_err(|e| format!("Automatic content crop: {e}"))?;
            physical_size = physical_size.map(|(wp, hp)| (wp * f64::from(w) / f64::from(page.width), hp * f64::from(h) / f64::from(page.height)));
            persist_image(root, &image.crop_imm(x, y, w, h))?
        } else {
            warnings.push("Automatic content crop found no bounding area. The complete prepared page was retained.".into());
            page
        }
    } else { page };
    let mut page = page;
    let size = checked_physical_size(physical_size.map(|(w, _)| w), physical_size.map(|(_, h)| h))?;
    set_physical_size(&mut page, size);
    drop(guards);
    Ok(PreparedPage { page, warnings })
}

#[cfg(feature = "crispembed")]
fn cleanup_image(image: DynamicImage, config: &crate::extractors::OcrPipelineConfig, recipe: &crate::extractors::OcrCleanupSpec, classical: bool, denoise: bool) -> Result<DynamicImage, String> {
    let model = if denoise {
        Some(crate::model_provisioning::resolve_model(config.nafnet_model.as_deref().unwrap_or("nafnet-denoise"), Some(false))
            .map_err(|e| format!("Load denoising model: {e}"))?)
    } else { None };
    let engine = crispembed::CrispScanCleanup::new(model.as_deref(), 0).map_err(|e| format!("Initialize scan cleanup: {e}"))?;
    // Defaults come from the linked library, so newer parameter fields remain
    // initialized when the native ABI is extended.
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
    let rgb = image.to_rgb8();
    let (pixels, width, height) = engine.process_with_params(rgb.as_raw(), rgb.width() as i32, rgb.height() as i32, 3, params)
        .map_err(|e| format!("Clean scan: {e}"))?;
    if width <= 0 || height <= 0 { return Err("Scan cleanup returned invalid image dimensions.".into()); }
    image::RgbImage::from_raw(width as u32, height as u32, pixels).map(DynamicImage::ImageRgb8)
        .ok_or_else(|| "Scan cleanup returned an invalid pixel buffer.".into())
}

#[cfg(not(feature = "crispembed"))]
fn cleanup_image(_image: DynamicImage, _config: &crate::extractors::OcrPipelineConfig, _recipe: &crate::extractors::OcrCleanupSpec, _classical: bool, _denoise: bool) -> Result<DynamicImage, String> {
    Err("Scan cleanup requires a build with CrispEmbed.".into())
}

fn validate_config(config: &crate::extractors::OcrPipelineConfig) -> Result<(), String> {
    crate::extractors::ocr_policy::validate_config(config)
}

pub(crate) fn recognition_config(config: crate::extractors::OcrPipelineConfig, engine: Option<&str>, language: Option<&str>) -> Result<(crate::extractors::OcrPipelineConfig, Vec<String>), String> {
    crate::extractors::ocr_policy::recognition_config(config, engine, language)
}

#[derive(Serialize)]
pub struct RecognizedPage {
    #[serde(flatten)]
    pub page: PageAsset,
    pub regions: Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>,
    pub elapsed_ms: u64,
    pub warnings: Vec<String>,
    pub diagnostics: Value,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn ocr_workspace_recognize(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    page_path: String,
    config: Option<crate::extractors::OcrPipelineConfig>,
    engine: Option<String>,
    language: Option<String>,
    zone: Option<PixelRect>,
    page_width_points: Option<f64>,
    page_height_points: Option<f64>,
    cpu_pixel_budget: Option<bool>,
    allow_downloads: Option<bool>,
) -> Result<RecognizedPage, String> {
    let log=OperationLog::start("ocr.workspace_recognition",json!({"surface":"gui","engine":engine,"language":language}));
    let result = ocr_workspace_recognize_inner(app, state, page_path, config, engine, language, zone,
        page_width_points, page_height_points, cpu_pixel_budget, allow_downloads.unwrap_or(false), &log).await;
    match &result {
        Ok(page) => log.succeed(json!({"regions":page.regions.len(),"warnings":page.warnings.len()})),
        Err(_) => log.fail("recognition_failed"),
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn ocr_workspace_recognize_inner(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    page_path: String,
    config: Option<crate::extractors::OcrPipelineConfig>,
    engine: Option<String>,
    language: Option<String>,
    zone: Option<PixelRect>,
    page_width_points: Option<f64>,
    page_height_points: Option<f64>,
    cpu_pixel_budget: Option<bool>,
    allow_downloads: bool,
    log: &OperationLog,
) -> Result<RecognizedPage, String> {
    log.step("validating",Value::Null);
    let root = workspace_root(&app)?;
    let path = managed_page(&root, &page_path)?;
    let size = checked_physical_size(page_width_points, page_height_points)?;
    let provided_config = config.is_some();
    let mut options = if provided_config { crate::extractors::ExtractOptions::default() }
        else { state.bg_ingest.lock().await.ocr_options() };
    let mut config = config.unwrap_or_else(|| options.ocr_pipeline.clone());
    if let Some(engine) = &engine { config.engine = Some(engine.clone()); }
    if let Some(language) = &language { config.language = Some(language.clone()); }
    if let Some(budget) = cpu_pixel_budget { config.cpu_pixel_budget = budget; }
    options.ocr_pipeline = config;
    let (config, warnings) = crate::extractors::ocr_policy::resolve_extraction_config(&options)?;
    log.step("policy_resolved",json!({"engine":config.engine,"language":config.language,"stages":config.stages.len(),"cpu_budget":config.cpu_pixel_budget}));
    let legacy_rec_lang = options.ocr_rec_lang;
    crate::extractors::ocr_policy::validate_runtime(&config)?;
    let cpu_budget = config.cpu_pixel_budget;
    // Recognition never downloads implicitly. Missing models are reported
    // (MODEL_DOWNLOAD_REQUIRED) so the UI can ask once, or installed first
    // when the caller already has the user's agreement.
    let plan_config = config.clone();
    let plan = tokio::task::spawn_blocking(move || crate::ocr_models::plan(&plan_config, &[])).await
        .map_err(|e| format!("Model plan: {e}"))?;
    if !allow_downloads { crate::ocr_models::ensure_or_gate(&plan.models, false, log)?; }
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    log.step("recognizing",Value::Null);
    let result = tokio::task::spawn_blocking(move || -> Result<RecognizedPage, String> {
        if !plan.missing.is_empty() {
            crate::ocr_models::install_models(&plan.missing, None, &|_| {})?;
        }
        let mut result = recognize_page(&root, &path, &config, zone, warnings, size, cpu_budget, legacy_rec_lang)?;
        if zone.is_some() || !config.enabled {
            // managed_page validated the canonical path used for all I/O.
            // A zone leaves the working image unchanged, so retain its exact
            // project identity even with Windows extended paths or symlinks.
            result.page.image_path = page_path;
        }
        Ok(result)
    }).await
        .map_err(|e| format!("Recognition worker: {e}"))??;
    log_review_grouping(log, &result.diagnostics);
    Ok(result)
}

fn log_review_grouping(log: &OperationLog, diagnostics: &Value) {
    for (stage, before, after) in [
        ("line_grouping", "words", "lines"),
        ("paragraph_grouping", "lines", "paragraphs"),
    ] {
        if let (Some(count), Some(regions)) = (
            diagnostics[stage][before].as_u64(), diagnostics[stage][after].as_u64(),
        ) {
            // Diagnostics can contain document text and source paths. Emit
            // only these measured counts on the existing recognition scope.
            log.step(stage, json!({"count": count, "regions": regions}));
        }
    }
}

/// The CPU pixel budget limits the Qwen2-VL vision input; it has no meaning
/// for other engines, and the worker cannot run in sandboxed/mobile builds.
#[cfg(test)]
fn check_cpu_budget(config: &crate::extractors::OcrPipelineConfig) -> Result<(), String> {
    crate::extractors::ocr_policy::validate_cpu_budget(config)
}

/// Recognize in the isolated low-memory worker and present the result with
/// honest provenance: VLM regions carry no calibrated detector score, so only
/// per-character recognition evidence is reported as a confidence.
fn recognize_with_cpu_budget(path: &Path, config: &crate::extractors::OcrPipelineConfig) -> Result<crate::extractors::ocr_crispembed::OcrReviewResult, String> {
    crate::extractors::ocr_worker::run_cpu_budget_review(path,
        &crate::extractors::ocr_crispembed::review_pipeline_config(config))
        .map_err(|e| format!("Low-memory CPU recognition: {e:#}"))
}

fn map_system_tesseract_regions(source: Vec<crate::ocr_word_evidence::SystemTesseractRegion>, offset: (u32, u32), width: u32, height: u32) -> Result<Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>, String> {
    use crate::extractors::ocr_crispembed::OcrReviewRegion;
    let mut regions = Vec::with_capacity(source.len());
    for mut region in source {
        region.x += offset.0 as f32;
        region.y += offset.1 as f32;
        for word in &mut region.word_evidence.words {
            word.x += offset.0 as f32;
            word.y += offset.1 as f32;
        }
        crate::ocr_word_evidence::validate_word_evidence(&region.word_evidence, &region.text, width, height)?;
        checked_rect(PixelRect { x:region.x as f64, y:region.y as f64, w:region.w as f64, h:region.h as f64 }, width, height)?;
        regions.push(OcrReviewRegion {
            text:region.text, x:region.x, y:region.y, w:region.w, h:region.h,
            confidence:None, confidence_basis:"unknown".into(),
            char_conf:Vec::new(), char_conf_unit:"unknown".into(), char_conf_aligned:false,
            word_evidence:Some(region.word_evidence), kind:"text".into(),
        });
    }
    Ok(regions)
}

/// Plain-text legacy engines share selection and model handling with file
/// extraction. They do not supply verified word geometry or confidence.
fn recognize_plain_engine_for_review(path: &Path, config: &crate::extractors::OcrPipelineConfig, rec_lang: crate::extractors::OcrRecLang) -> Result<crate::extractors::ocr_crispembed::OcrReviewResult, String> {
    use crate::extractors::ocr_crispembed::{OcrReviewRegion, OcrReviewResult};
    let (width, height) = checked_dimensions(path)?;
    let mut recognition = config.clone();
    // Geometry/preparation was already applied to the reviewed snapshot.
    recognition.enabled = false;
    recognition.layout = false;
    let options = crate::extractors::ExtractOptions { try_ocr: true, ocr_pipeline: recognition, ocr_rec_lang: rec_lang, ..Default::default() };
    let text = crate::extractors::ocr_image_page(path, &options)
        .map_err(|e| format!("Recognize page: {e:#}"))?.full_text;
    let regions = if text.trim().is_empty() { Vec::new() } else {
        vec![OcrReviewRegion { text, x: 0.0, y: 0.0, w: width as f32, h: height as f32,
            confidence: None, confidence_basis: "unknown".into(), char_conf: Vec::new(),
            char_conf_unit: "unknown".into(), char_conf_aligned: false, word_evidence: None, kind: "text".into() }]
    };
    Ok(OcrReviewResult { image_path: path.to_owned(), width, height, regions,
        warnings: vec!["This engine returns plain text. The review block covers the page; word positions and confidence are unavailable.".into()],
        diagnostics: json!({"engine": config.engine, "geometry": "full_page_text_block", "native_word_evidence": false}) })
}

fn translate_crop_review_regions(
    regions: &mut [crate::extractors::ocr_crispembed::OcrReviewRegion],
    crop: crate::ocr_reading_order::Rect,
    width: u32,
    height: u32,
) -> Result<(), String> {
    for region in regions {
        let mapped = crate::ocr_reading_order::crop_rect_to_page(
            crate::ocr_reading_order::Rect { x:region.x, y:region.y, w:region.w, h:region.h },
            crop, width, height,
        ).ok_or("Layout recognition returned invalid crop coordinates")?;
        if let Some(evidence) = &mut region.word_evidence {
            // Page bounds alone would accept words outside their real crop.
            crate::ocr_word_evidence::validate_word_evidence(evidence, &region.text, crop.w as u32, crop.h as u32)?;
            for word in &mut evidence.words {
                word.x += crop.x;
                word.y += crop.y;
            }
            crate::ocr_word_evidence::validate_word_evidence(evidence, &region.text, width, height)?;
        }
        region.x = mapped.x;
        region.y = mapped.y;
    }
    Ok(())
}

/// Use layout as recognition input, rather than trying to split already
/// recognized page-wide lines. The planner opts in only for unambiguous columns.
fn recognize_cc_columns(
    path: &Path,
    config: &crate::extractors::OcrPipelineConfig,
    layout: &[crate::extractors::layout::LayoutRegion],
) -> Result<Option<crate::extractors::ocr_crispembed::OcrReviewResult>, String> {
    use crate::extractors::ocr_crispembed::{ocr_regions_for_review, OcrReviewResult};
    let (width, height) = checked_dimensions(path)?;
    let boxes: Vec<_> = layout.iter().map(|r| crate::ocr_reading_order::Rect {
        x: r.x1, y: r.y1, w: r.x2 - r.x1, h: r.y2 - r.y1,
    }).collect();
    let Some(crops) = crate::ocr_reading_order::column_recognition_crops(&boxes, width, height) else {
        return Ok(None);
    };
    let image = decode_page(path)?;
    let mut regions = Vec::new();
    let mut warnings = Vec::new();
    let mut observations = Vec::new();
    for (index, crop) in crops.iter().enumerate() {
        let (x, y, w, h) = checked_rect(PixelRect {
            x: crop.x as f64, y: crop.y as f64, w: crop.w as f64, h: crop.h as f64,
        }, width, height)?;
        let temporary = tempfile::Builder::new().prefix("ocr_layout_").suffix(".png")
            .tempfile().map_err(|e| format!("Create layout recognition input: {e}"))?;
        image.crop_imm(x, y, w, h).save(temporary.path())
            .map_err(|e| format!("Prepare layout recognition input: {e}"))?;
        let mut result = ocr_regions_for_review(temporary.path(), config)
            .map_err(|e| format!("Recognize layout region {}: {e:#}", index + 1))?;
        if result.width != w || result.height != h {
            return Err(format!("Layout region {} changed image coordinates", index + 1));
        }
        // Keep the complete original engine snapshot and local coordinates,
        // including an empty crop that requires whole-page fallback.
        observations.push(json!({"crop":{"x":x,"y":y,"w":w,"h":h},
            "raw_regions": result.regions, "diagnostics": result.diagnostics, "warnings":result.warnings}));
        if result.regions.is_empty() || result.regions.iter().all(|r| r.text.trim().is_empty()) {
            let mut fallback = ocr_regions_for_review(path, config)
                .map_err(|e| format!("Recognize whole page after empty layout region: {e:#}"))?;
            warnings.append(&mut result.warnings);
            fallback.warnings.extend(warnings);
            fallback.warnings.push("A detected layout region returned no text; used whole-page recognition instead. Review the reading order.".into());
            fallback.diagnostics["engine"] = json!(config.engine);
            fallback.diagnostics["layout_recognition"] = json!({
                "strategy":"whole_page_after_empty_cc_crop","crops":observations,
            });
            return Ok(Some(fallback));
        }
        if regions.len().saturating_add(result.regions.len()) > MAX_REGIONS {
            return Err("Layout recognition returned too many regions".into());
        }
        translate_crop_review_regions(&mut result.regions, *crop, width, height)?;
        warnings.append(&mut result.warnings);
        regions.extend(result.regions);
    }
    Ok(Some(OcrReviewResult {
        image_path: path.to_owned(), width, height, regions, warnings,
        diagnostics: json!({"coordinate_space":"prepared_image_pixels","internal_cleanup":false,
            "requested_config":config,"engine":config.engine,
            "layout_recognition":{"strategy":"cc_crops_before_recognition","crops":observations}}),
    }))
}

fn recognize_page(root: &Path, path: &Path, config: &crate::extractors::OcrPipelineConfig, zone: Option<PixelRect>, mut warnings: Vec<String>, physical_size: Option<(f64, f64)>, cpu_budget: bool, legacy_rec_lang: crate::extractors::OcrRecLang) -> Result<RecognizedPage, String> {
    let started = std::time::Instant::now();
    let (page, recognition_path, offset) = if let Some(zone) = zone {
        // A zone result must align with the current whole page. Geometric
        // repair is a separate page action, never an implicit crop transform.
        let image = decode_page(path)?;
        let (x, y, w, h) = checked_rect(zone, image.width(), image.height())?;
        let crop = persist_image(root, &image.crop_imm(x, y, w, h))?;
        warnings.push("Zone recognition preserves the current page geometry; scan repair settings are applied only when recognizing a complete page.".into());
        (PageAsset { image_path: path.to_string_lossy().into_owned(), width: image.width(), height: image.height(),
            page_width_points:physical_size.map(|(w, _)| w), page_height_points:physical_size.map(|(_, h)| h), warnings: Vec::new() }, PathBuf::from(crop.image_path), (x, y))
    } else if !config.enabled {
        let (width, height) = checked_dimensions(path)?;
        let mut page = PageAsset { image_path: path.to_string_lossy().into_owned(), width, height,
            page_width_points: None, page_height_points: None, warnings: Vec::new() };
        set_physical_size(&mut page, physical_size);
        (page, path.to_owned(), (0, 0))
    } else {
        let prepared = prepare_page(root, path, config, physical_size)?;
        warnings.extend(prepared.warnings);
        let recognition_path = PathBuf::from(&prepared.page.image_path);
        (prepared.page, recognition_path, (0, 0))
    };
    let mut column_layout = None;
    let column_result = if config.experimental_cc_column_crops
        && !cpu_budget && zone.is_none() && config.enabled && config.layout
        && config.layout_engine == "cc" && config.engine.as_deref() == Some("tesseract")
        && !config.router && config.stages.len() <= 1
        && config.stages.first().is_none_or(|stage| stage.engine == "tesseract") {
        let layout = crate::extractors::ocr_crispembed::cc_detect_regions(&recognition_path);
        let result = recognize_cc_columns(&recognition_path, config, &layout)?;
        column_layout = Some(layout);
        result
    } else { None };
    let mut result = if let Some(result) = column_result {
        result
    } else if cpu_budget {
        recognize_with_cpu_budget(&recognition_path, config)?
    } else if config.engine.as_deref() == Some("system_tesseract") {
        let source = crate::ocr_word_evidence::recognize(&recognition_path, config.language.as_deref())?;
        let mut diagnostics = source.diagnostics;
        diagnostics["engine"] = json!("system_tesseract");
        diagnostics["full_text"] = json!(source.full_text);
        crate::extractors::ocr_crispembed::OcrReviewResult {
            image_path: recognition_path.clone(), width: source.width, height: source.height,
            regions: map_system_tesseract_regions(source.regions, (0, 0), source.width, source.height)?,
            warnings: source.warnings, diagnostics,
        }
    } else if matches!(config.engine.as_deref(), Some("ocrs" | "paddle")) {
        recognize_plain_engine_for_review(&recognition_path, config, legacy_rec_lang)?
    } else {
        crate::extractors::ocr_crispembed::ocr_regions_for_review(&recognition_path, config)
            .map_err(|e| format!("Recognize page: {e:#}"))?
    };
    warnings.append(&mut result.warnings);
    if result.regions.is_empty() {
        warnings.push("No text was returned. Verify whether this page is blank, or retry with a different engine. An empty result is not evidence of successful recognition.".into());
    }
    result.regions = organize_review_regions(std::mem::take(&mut result.regions), &mut result.diagnostics,
        |regions, diagnostics| {
            if config.enabled && config.layout && zone.is_none() {
                if let Some(layout) = &column_layout {
                    apply_review_layout(layout, config, regions, diagnostics, &mut warnings);
                } else {
                    annotate_layout(&recognition_path, config, regions, diagnostics, &mut warnings);
                }
            }
        });
    for region in &mut result.regions {
        region.x += offset.0 as f32;
        region.y += offset.1 as f32;
        if let Some(evidence) = &mut region.word_evidence {
            for word in &mut evidence.words {
                word.x += offset.0 as f32;
                word.y += offset.1 as f32;
            }
            crate::ocr_word_evidence::validate_word_evidence(evidence, &region.text, page.width, page.height)?;
        }
        // A malformed native box must not reach a searchable export. Preserve
        // the previous frontend result by failing the whole replacement.
        checked_rect(PixelRect { x: region.x as f64, y: region.y as f64, w: region.w as f64, h: region.h as f64 }, page.width, page.height)
            .map_err(|e| format!("Recognition returned invalid region geometry: {e}"))?;
    }
    if let Some(zone) = zone {
        result.diagnostics["zone"] = serde_json::to_value(zone).map_err(|e| e.to_string())?;
        result.diagnostics["effective_zone"] = json!({"x":offset.0,"y":offset.1,"w":result.width,"h":result.height});
        result.diagnostics["recognition_image_path"] = json!(recognition_path.to_string_lossy());
        result.diagnostics["region_offset"] = json!({"x":offset.0,"y":offset.1});
    }
    result.diagnostics["requested_config"] = serde_json::to_value(config).map_err(|e| e.to_string())?;
    Ok(RecognizedPage { page, regions: result.regions, elapsed_ms: started.elapsed().as_millis() as u64, warnings, diagnostics: result.diagnostics })
}

/// Classify and order individual lines before joining them into paragraphs.
/// A layout heading, table or formula must remain a boundary even when its
/// geometry resembles adjacent prose.
fn organize_review_regions(
    regions: Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>,
    diagnostics: &mut Value,
    annotate: impl FnOnce(&mut Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>, &mut Value),
) -> Vec<crate::extractors::ocr_crispembed::OcrReviewRegion> {
    let detected = regions.len();
    let mut regions = merge_word_regions_into_lines(regions);
    if regions.len() != detected {
        diagnostics["line_grouping"] = json!({"words": detected, "lines": regions.len()});
    }
    annotate(&mut regions, diagnostics);
    let lines = regions.len();
    let regions = group_lines_into_paragraphs(regions);
    if regions.len() != lines {
        diagnostics["paragraph_grouping"] = json!({"lines": lines, "paragraphs": regions.len()});
    }
    regions
}

fn has_aligned_character_scores(region: &crate::extractors::ocr_crispembed::OcrReviewRegion) -> bool {
    region.confidence_basis == "recognition" && region.char_conf_aligned
        && region.char_conf_unit == "unicode_codepoint" && !region.char_conf.is_empty()
        && region.char_conf.len() == region.text.chars().count()
        && region.char_conf.iter().all(|score| score.is_finite() && (0.0..=1.0).contains(score))
}

/// Join word regions that sit on the same text line into one line region.
/// Words are joined with one space. Aligned per-codepoint confidences stay
/// aligned: the inserted space takes the lower neighbouring score, so a word's
/// own scores (and its minimum) are unchanged. Regions with native word
/// evidence (already line-level, with word offsets) and non-text regions are
/// left exactly as they are and separate grouping runs. The native reading
/// order determines the order of the resulting lines, including across columns.
fn merge_word_regions_into_lines(regions: Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>) -> Vec<crate::extractors::ocr_crispembed::OcrReviewRegion> {
    use crate::extractors::ocr_crispembed::OcrReviewRegion;
    let mergeable = |r: &OcrReviewRegion| r.kind == "text" && r.word_evidence.is_none()
        && [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite())
        && r.x >= 0.0 && r.y >= 0.0 && r.h > 0.0 && r.w > 0.0;
    if regions.iter().filter(|r| mergeable(r)).count() < 2 { return regions; }
    let mut out: Vec<OcrReviewRegion> = Vec::with_capacity(regions.len());
    let mut words = Vec::new();
    for (index, region) in regions.into_iter().enumerate() {
        if mergeable(&region) {
            words.push((index, region));
        } else {
            // Never move or merge words across an untouched region. Invalid
            // geometry also stays untouched so the caller can reject it.
            out.extend(group_word_run(std::mem::take(&mut words)));
            out.push(region);
        }
    }
    out.extend(group_word_run(words));
    out
}

fn group_word_run(mut words: Vec<(usize, crate::extractors::ocr_crispembed::OcrReviewRegion)>) -> Vec<crate::extractors::ocr_crispembed::OcrReviewRegion> {
    use crate::extractors::ocr_crispembed::OcrReviewRegion;
    // Lines by vertical overlap, then words left-to-right within a line.
    words.sort_by(|(_, a), (_, b)| (a.y + a.h / 2.0).total_cmp(&(b.y + b.h / 2.0)));
    let mut lines: Vec<Vec<(usize, OcrReviewRegion)>> = Vec::new();
    for word in words {
        let fits = |line: &Vec<(usize, OcrReviewRegion)>| {
            let top = line.iter().map(|(_, r)| r.y).fold(f32::MAX, f32::min);
            let bottom = line.iter().map(|(_, r)| r.y + r.h).fold(f32::MIN, f32::max);
            let height = line.iter().map(|(_, r)| r.h).sum::<f32>() / line.len() as f32;
            let overlap = (bottom.min(word.1.y + word.1.h) - top.max(word.1.y)).max(0.0);
            overlap >= 0.5 * height.min(word.1.h)
        };
        match lines.iter_mut().rev().take(3).find(|line| fits(line)) {
            Some(line) => line.push(word),
            None => lines.push(vec![word]),
        }
    }
    let mut merged = Vec::new();
    for mut line in lines {
        line.sort_by(|(_, a), (_, b)| a.x.total_cmp(&b.x));
        // Split a line where the horizontal gap is a column gutter, not a space.
        let height = line.iter().map(|(_, r)| r.h).sum::<f32>() / line.len() as f32;
        let mut segments: Vec<Vec<(usize, OcrReviewRegion)>> = vec![];
        for word in line {
            match segments.last_mut() {
                Some(segment) if word.1.x - segment.iter().map(|(_, r)| r.x + r.w).fold(f32::MIN, f32::max) <= 2.5 * height => segment.push(word),
                _ => segments.push(vec![word]),
            }
        }
        for segment in segments {
            let anchor = segment.iter().map(|(index, _)| *index).min().expect("nonempty line segment");
            merged.push((anchor, join_line(segment.into_iter().map(|(_, region)| region).collect())));
        }
    }
    // Geometry groups words; it must not replace the provider's reading order
    // with a row-first traversal of a multi-column page.
    merged.sort_by_key(|(anchor, _)| *anchor);
    merged.into_iter().map(|(_, region)| region).collect()
}

fn join_line(mut words: Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>) -> crate::extractors::ocr_crispembed::OcrReviewRegion {
    if words.len() == 1 { return words.pop().unwrap(); }
    let x = words.iter().map(|r| r.x).fold(f32::MAX, f32::min);
    let y = words.iter().map(|r| r.y).fold(f32::MAX, f32::min);
    let right = words.iter().map(|r| r.x + r.w).fold(f32::MIN, f32::max);
    let bottom = words.iter().map(|r| r.y + r.h).fold(f32::MIN, f32::max);
    let first = &words[0];
    let uniform_basis = words.iter().all(|r| r.confidence_basis == first.confidence_basis);
    let complete_scores = uniform_basis && words.iter().all(|r| r.confidence
        .is_some_and(|score| score.is_finite() && (0.0..=1.0).contains(&score)));
    let aligned = words.iter().all(has_aligned_character_scores);
    let mut text = String::new();
    let mut char_conf: Vec<f32> = Vec::new();
    let (mut weighted, mut weight) = (0.0f64, 0.0f64);
    for (index, word) in words.iter().enumerate() {
        if index > 0 {
            text.push(' ');
            if aligned {
                let left = char_conf.last().copied().unwrap_or(1.0);
                let right = word.char_conf.first().copied().unwrap_or(left);
                char_conf.push(left.min(right));
            }
        }
        text.push_str(&word.text);
        if aligned { char_conf.extend_from_slice(&word.char_conf); }
        if let Some(score) = word.confidence.filter(|s| s.is_finite() && (0.0..=1.0).contains(s)) {
            let n = word.text.chars().count().max(1) as f64;
            weighted += f64::from(score) * n; weight += n;
        }
    }
    crate::extractors::ocr_crispembed::OcrReviewRegion {
        text, x, y, w: right - x, h: bottom - y,
        // A partial mean cannot describe the complete line, and a detector
        // score must never acquire the first word's recognition provenance.
        confidence: complete_scores.then(|| (weighted / weight) as f32),
        confidence_basis: if uniform_basis { first.confidence_basis.clone() } else { "unknown".into() },
        char_conf: if aligned { char_conf } else { Vec::new() },
        char_conf_unit: if aligned { first.char_conf_unit.clone() } else { "unknown".into() },
        char_conf_aligned: aligned,
        word_evidence: None,
        kind: "text".into(),
    }
}

/// Joins consecutive text lines (in reading order) into paragraphs: the next
/// line starts just below the previous one, has a similar height and shares
/// its horizontal extent. Lines are joined with "\n" so their breaks survive.
/// Regions with native word evidence (already paragraph-level) and non-text
/// regions are never joined.
fn group_lines_into_paragraphs(regions: Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>) -> Vec<crate::extractors::ocr_crispembed::OcrReviewRegion> {
    use crate::extractors::ocr_crispembed::OcrReviewRegion;
    let joinable = |r: &OcrReviewRegion| r.kind == "text" && r.word_evidence.is_none()
        && [r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite()) && r.x >= 0.0 && r.y >= 0.0 && r.h > 0.0 && r.w > 0.0;
    let continues = |paragraph: &[OcrReviewRegion], next: &OcrReviewRegion| {
        let last = paragraph.last().expect("nonempty paragraph");
        let height = paragraph.iter().map(|r| r.h).sum::<f32>() / paragraph.len() as f32;
        let gap = next.y - (last.y + last.h);
        let similar = next.h <= 1.5 * height && height <= 1.5 * next.h;
        let left = paragraph.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        let right = paragraph.iter().map(|r| r.x + r.w).fold(f32::MIN, f32::max);
        let overlap = (right.min(next.x + next.w) - left.max(next.x)).max(0.0);
        let aligned = (next.x - last.x).abs() <= 2.0 * height || overlap >= 0.5 * next.w.min(right - left);
        gap > -0.3 * height && gap <= 0.8 * height && similar && aligned
    };
    let mut out = Vec::with_capacity(regions.len());
    let mut paragraph: Vec<OcrReviewRegion> = Vec::new();
    for region in regions {
        if joinable(&region) && (paragraph.is_empty() || continues(&paragraph, &region)) {
            paragraph.push(region);
            continue;
        }
        if !paragraph.is_empty() { out.push(join_paragraph(std::mem::take(&mut paragraph))); }
        if joinable(&region) { paragraph.push(region); } else { out.push(region); }
    }
    if !paragraph.is_empty() { out.push(join_paragraph(paragraph)); }
    out
}

fn join_paragraph(mut lines: Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>) -> crate::extractors::ocr_crispembed::OcrReviewRegion {
    if lines.len() == 1 { return lines.pop().unwrap(); }
    let first = lines[0].clone();
    let uniform_basis = lines.iter().all(|r| r.confidence_basis == first.confidence_basis);
    let complete_scores = uniform_basis && lines.iter().all(|r| r.confidence
        .is_some_and(|score| score.is_finite() && (0.0..=1.0).contains(&score)));
    let aligned = lines.iter().all(has_aligned_character_scores);
    let x = lines.iter().map(|r| r.x).fold(f32::MAX, f32::min);
    let y = lines.iter().map(|r| r.y).fold(f32::MAX, f32::min);
    let right = lines.iter().map(|r| r.x + r.w).fold(f32::MIN, f32::max);
    let bottom = lines.iter().map(|r| r.y + r.h).fold(f32::MIN, f32::max);
    let mut text = String::new();
    let mut char_conf: Vec<f32> = Vec::new();
    let (mut weighted, mut weight) = (0.0f64, 0.0f64);
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            text.push('\n');
            if aligned {
                let left = char_conf.last().copied().unwrap_or(1.0);
                char_conf.push(left.min(line.char_conf.first().copied().unwrap_or(left)));
            }
        }
        text.push_str(&line.text);
        if aligned { char_conf.extend_from_slice(&line.char_conf); }
        if let Some(score) = line.confidence.filter(|s| s.is_finite() && (0.0..=1.0).contains(s)) {
            let n = line.text.chars().count().max(1) as f64;
            weighted += f64::from(score) * n; weight += n;
        }
    }
    crate::extractors::ocr_crispembed::OcrReviewRegion {
        text, x, y, w: right - x, h: bottom - y,
        confidence: complete_scores.then(|| (weighted / weight) as f32),
        confidence_basis: if uniform_basis { first.confidence_basis } else { "unknown".into() },
        char_conf: if aligned { char_conf } else { Vec::new() },
        char_conf_unit: if aligned { first.char_conf_unit } else { "unknown".into() },
        char_conf_aligned: aligned,
        word_evidence: None,
        kind: "text".into(),
    }
}

fn annotate_layout(path: &Path, config: &crate::extractors::OcrPipelineConfig, regions: &mut Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>, diagnostics: &mut Value, warnings: &mut Vec<String>) {
    use crate::extractors::layout::LayoutDetector;
    let detected = if config.layout_engine == "cc" {
        Ok(crate::extractors::ocr_crispembed::cc_detect_regions(path))
    } else {
        LayoutDetector::load(config.layout_model.as_deref().unwrap_or("layout-heron"), 0)
            .and_then(|engine| engine.detect(path, config.layout_threshold))
    };
    let layout = match detected {
        Ok(layout) => layout,
        Err(e) => { warnings.push(format!("Layout analysis failed; recognized text was retained: {e:#}")); return; },
    };
    apply_review_layout(&layout, config, regions, diagnostics, warnings);
}

fn apply_review_layout(
    layout: &[crate::extractors::layout::LayoutRegion],
    config: &crate::extractors::OcrPipelineConfig,
    regions: &mut Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>,
    diagnostics: &mut Value,
    warnings: &mut Vec<String>,
) {
    use crate::extractors::layout::RegionKind;
    diagnostics["layout"] = Value::Array(layout.iter().map(|r| json!({"x":r.x1,"y":r.y1,"w":r.x2-r.x1,"h":r.y2-r.y1,"label":r.label_name,"score":r.score})).collect());
    for region in regions.iter_mut() {
        let best = layout.iter().filter_map(|r| {
            let w = (r.x2.min(region.x + region.w) - r.x1.max(region.x)).max(0.0);
            let h = (r.y2.min(region.y + region.h) - r.y1.max(region.y)).max(0.0);
            let coverage = w * h / (region.w * region.h).max(1.0);
            if coverage >= 0.5 { Some((r, coverage)) } else { None }
        }).max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((layout, _)) = best {
            region.kind = match &layout.kind {
                RegionKind::Title => "title", RegionKind::Table => "table", RegionKind::Formula => "formula",
                RegionKind::Figure => "picture", RegionKind::Header => "header", RegionKind::Footer => "footer", _ => "text",
            }.into();
        }
    }
    let text_boxes: Vec<_> = regions.iter().map(|r| crate::ocr_reading_order::Rect { x:r.x, y:r.y, w:r.w, h:r.h }).collect();
    let layout_boxes: Vec<_> = layout.iter().map(|r| crate::ocr_reading_order::Rect { x:r.x1, y:r.y1, w:r.x2-r.x1, h:r.y2-r.y1 }).collect();
    let order = crate::ocr_reading_order::order_regions(&text_boxes, &layout_boxes);
    diagnostics["layout_reading_order"] = json!({"strategy":"layout_blocks_and_column_gutters","indices":order});
    // Keep every original region even if a future ordering implementation
    // returns incomplete indices. Geometry repair must never lose text.
    let mut rank = vec![usize::MAX; regions.len()];
    for (position, &index) in order.iter().enumerate() {
        if index < rank.len() && rank[index] == usize::MAX { rank[index] = position; }
    }
    let mut ordered: Vec<_> = std::mem::take(regions).into_iter().enumerate().collect();
    ordered.sort_by_key(|(index, _)| (rank[*index], *index));
    *regions = ordered.into_iter().map(|(_, region)| region).collect();
    if config.drop_headers_footers {
        let count = regions.iter().filter(|r| r.kind == "header" || r.kind == "footer").count();
        regions.retain(|r| r.kind != "header" && r.kind != "footer");
        if count > 0 { warnings.push(format!("Excluded {count} detected header/footer regions according to this profile.")); }
    }
}

fn validate_project(project: &Value) -> Result<String, String> {
    if project.get("version").and_then(Value::as_u64) != Some(PROJECT_VERSION) {
        return Err("Unsupported OCR project version. Open this project with a compatible application version.".into());
    }
    let id = project.get("id").and_then(Value::as_str).filter(|id| valid_project_id(id))
        .ok_or("The OCR project ID is invalid.")?;
    let pages = project.get("pages").and_then(Value::as_array).ok_or("The OCR project has no page list.")?;
    if pages.len() > MAX_PAGES { return Err(format!("A project can contain at most {MAX_PAGES} pages.")); }
    let mut ids = HashSet::new();
    for (index, page) in pages.iter().enumerate() {
        let page_id = page.get("id").and_then(Value::as_str).ok_or("A project page has no ID.")?;
        if !ids.insert(page_id) { return Err("The project contains duplicate page IDs.".into()); }
        for key in ["imagePath", "originalImagePath"] {
            if page.get(key).and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 32_768 && !s.contains('\0')).is_none() {
                return Err(format!("Page {} has an invalid {key}.", index + 1));
            }
        }
    }
    Ok(id.to_owned())
}

fn asset_warnings(root: &Path, project: &Value) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(pages) = project.get("pages").and_then(Value::as_array) {
        for (i, page) in pages.iter().enumerate() {
            if let Some(path) = page.get("imagePath").and_then(Value::as_str) {
                // Probe only paths within the application's asset directory.
                // A project JSON cannot be used to inspect arbitrary files.
                let expected_root = root.join("assets");
                if !Path::new(path).starts_with(&expected_root) || managed_page(root, path).is_err() {
                    warnings.push(format!("Page {} image is unavailable. Text and review edits were retained; reimport the source before recognition or export.", i + 1));
                }
            }
        }
    }
    warnings
}

fn read_project_file(path: &Path) -> Result<SavedProject, String> {
    let len = fs::metadata(path).map_err(|e| format!("Read project: {e}"))?.len();
    if len > MAX_PROJECT_BYTES { return Err("The project exceeds the 64 MB metadata limit.".into()); }
    let bytes = fs::read(path).map_err(|e| format!("Read project: {e}"))?;
    let mut saved: SavedProject = serde_json::from_slice(&bytes).map_err(|e| format!("Read project JSON: {e}"))?;
    if saved.schema_version != PROJECT_VERSION { return Err("Unsupported saved project version.".into()); }
    let id = saved.project.get("id").and_then(Value::as_str).ok_or("Saved project has no ID.")?;
    if !valid_project_id(id) { return Err("Saved project has an invalid ID.".into()); }
    saved.path = path.to_string_lossy().into_owned();
    Ok(saved)
}

fn load_project(root: &Path, id: &str) -> Result<Option<SavedProject>, String> {
    if !valid_project_id(id) { return Err("Invalid OCR project ID.".into()); }
    let path = root.join("projects").join(format!("{id}.json"));
    if !path.exists() { return Ok(None); }
    match read_project_file(&path) {
        Ok(saved) => Ok(Some(saved)),
        Err(error) => {
            let backup = path.with_extension("previous.json");
            let mut saved = read_project_file(&backup).map_err(|_| error.clone())?;
            saved.path = path.to_string_lossy().into_owned();
            saved.warnings.push(format!("Recovered the previous saved revision because the latest project file could not be read: {error}"));
            Ok(Some(saved))
        },
    }
}

fn save_project(root: &Path, project: Value, expected_revision: u64) -> Result<SavedProject, String> {
    let id = validate_project(&project)?;
    let _writer = PROJECT_WRITER.get_or_init(|| Mutex::new(())).lock()
        .map_err(|_| "The project save lock is unavailable.".to_string())?;
    fs::create_dir_all(root.join("projects")).map_err(|e| format!("Create project directory: {e}"))?;
    // SQLite's OS-backed write lock also serializes separate application
    // processes, and is released by the OS after a crash. The JSON files remain
    // the project format; this database is only the cross-process save mutex.
    let process_lock = rusqlite::Connection::open(root.join("save-lock.sqlite"))
        .map_err(|e| format!("Open project save lock: {e}"))?;
    process_lock.busy_timeout(std::time::Duration::from_secs(2)).map_err(|e| e.to_string())?;
    process_lock.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| format!("Another application instance is saving this workspace; retry after it finishes: {e}"))?;
    let existing = load_project(root, &id)?;
    let actual = existing.as_ref().map(|s| s.revision).unwrap_or(0);
    if actual != expected_revision {
        return Err(format!("Project changed on disk (expected revision {expected_revision}, current {actual}). Reload the project before saving."));
    }
    let path = root.join("projects").join(format!("{id}.json"));
    let warnings = asset_warnings(root, &project);
    let mut saved = SavedProject {
        schema_version: PROJECT_VERSION, project, revision: actual.checked_add(1).ok_or("Project revision overflow.")?,
        saved_at: now_ms(), path: path.to_string_lossy().into_owned(), warnings,
    };
    let bytes = serde_json::to_vec(&saved).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES { return Err("The project exceeds the 64 MB metadata limit.".into()); }
    if let Some(previous) = existing {
        let previous_bytes = serde_json::to_vec(&previous).map_err(|e| e.to_string())?;
        atomic_write(&path.with_extension("previous.json"), &previous_bytes, true)?;
    }
    atomic_write(&path, &bytes, true)?;
    if let Err(e) = atomic_write(&root.join("latest-project"), id.as_bytes(), true) {
        saved.warnings.push(format!("Project saved, but the recent-project pointer could not be updated: {e}"));
    }
    Ok(saved)
}

#[tauri::command]
pub async fn ocr_workspace_save(app: tauri::AppHandle, project: Value, expected_revision: Option<u64>) -> Result<SavedProject, String> {
    let log=OperationLog::start("ocr.project_save",json!({"surface":"gui","revision":expected_revision}));
    let result: Result<SavedProject,String> = async {
    let root = workspace_root(&app)?;
    tokio::task::spawn_blocking(move || save_project(&root, project, expected_revision.unwrap_or(0))).await
        .map_err(|e| format!("Project save worker: {e}"))?
    }.await;
    match &result {Ok(saved)=>log.succeed(json!({"revision":saved.revision,"warnings":saved.warnings.len()})),Err(_)=>log.fail("project_save_failed")}
    result
}

#[tauri::command]
pub async fn ocr_workspace_load(app: tauri::AppHandle, project_id: Option<String>) -> Result<Option<SavedProject>, String> {
    let root = workspace_root(&app)?;
    tokio::task::spawn_blocking(move || {
        let id = match project_id {
            Some(id) => id,
            None => match fs::read_to_string(root.join("latest-project")) {
                Ok(id) => id,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(format!("Read latest project: {e}")),
            },
        };
        let mut saved = load_project(&root, id.trim())?;
        if let Some(saved) = &mut saved {
            saved.warnings.extend(asset_warnings(&root, &saved.project));
        }
        Ok(saved)
    }).await.map_err(|e| format!("Project load worker: {e}"))?
}

#[tauri::command]
pub async fn ocr_workspace_list(app: tauri::AppHandle) -> Result<Vec<ProjectSummary>, String> {
    let root = workspace_root(&app)?;
    tokio::task::spawn_blocking(move || {
        let mut projects = Vec::new();
        for entry in fs::read_dir(root.join("projects")).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") || path.to_string_lossy().ends_with(".previous.json") { continue; }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue; };
            if let Ok(Some(saved)) = load_project(&root, id) {
                projects.push(ProjectSummary {
                    id: id.to_string(), name: saved.project.get("name").and_then(Value::as_str).unwrap_or("OCR project").to_string(),
                    revision: saved.revision, updated_at: saved.saved_at,
                    page_count: saved.project.get("pages").and_then(Value::as_array).map(|p| p.len()).unwrap_or(0),
                });
            }
        }
        projects.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(projects)
    }).await.map_err(|e| format!("Project list worker: {e}"))?
}

/// Immutable source coverage and evidence, kept separate from a manual passage.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PassageSourceComponent {
    pub version: u32,
    pub id: String,
    pub source_region_id: String,
    pub page_id: String,
    pub image_path: String,
    pub width: u32,
    pub height: u32,
    pub bounds_at_capture: PixelRect,
    pub kind: String,
    pub original_text: String,
    pub text_at_capture: String,
    pub confidence: Option<f32>,
    pub confidence_basis: String,
    #[serde(rename = "char_conf")]
    pub char_conf: Vec<f32>,
    pub char_confidence_unit: String,
    pub char_confidence_aligned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_evidence: Option<crate::ocr_word_evidence::WordEvidenceEnvelope>,
}

fn validate_source_components(components: &[PassageSourceComponent]) -> Result<usize, String> {
    let bytes = serde_json::to_vec(components).map_err(|e| e.to_string())?.len();
    if components.is_empty() || components.len() > 128 || bytes > 8 * 1024 * 1024 {
        return Err("Passage source history exceeds 128 components or 8 MB. No evidence was truncated.".into());
    }
    let valid_id = |value: &str| !value.trim().is_empty() && value.encode_utf16().count() <= 200 && !value.contains('\0');
    let score = |value: f32| value.is_finite() && (0.0..=1.0).contains(&value);
    let mut ids = HashSet::new();
    for source in components {
        if source.version != 1 || !valid_id(&source.id) || !ids.insert(&source.id) ||
            !valid_id(&source.source_region_id) || !valid_id(&source.page_id) ||
            source.image_path.is_empty() || source.image_path.encode_utf16().count() > 10_000 || source.image_path.contains('\0') ||
            source.width == 0 || source.height == 0 || source.width > 100_000 || source.height > 100_000 ||
            u64::from(source.width) * u64::from(source.height) > MAX_PAGE_PIXELS ||
            !["text","title","table","picture","formula","header","footer"].contains(&source.kind.as_str()) ||
            !["recognition","detection","unknown","manual"].contains(&source.confidence_basis.as_str()) ||
            source.confidence.is_some_and(|value| !score(value)) || source.char_conf.len() > 1_000_000 ||
            source.char_conf.iter().any(|value| !score(*value)) ||
            !["unicode_codepoint","token","unknown"].contains(&source.char_confidence_unit.as_str()) {
            return Err("Passage source history contains invalid metadata.".into());
        }
        checked_rect(source.bounds_at_capture, source.width, source.height)?;
        if source.char_confidence_aligned && (source.char_confidence_unit != "unicode_codepoint" ||
            source.char_conf.is_empty() || source.char_conf.len() != source.original_text.chars().count()) {
            return Err("Passage source character evidence does not match its original text.".into());
        }
        if let Some(words) = &source.word_evidence {
            crate::ocr_word_evidence::validate_word_evidence(words, &source.original_text, source.width, source.height)?;
        }
    }
    Ok(bytes)
}

#[derive(Serialize, Deserialize)]
pub struct ExportRegion {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Unknown is null in review JSON. Paired renderers omit unknown scores;
    /// legacy output receives neutral zero only at the rendering boundary.
    pub confidence: Option<f32>,
    #[serde(default)]
    pub char_conf: Vec<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_basis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_conf_unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_conf_aligned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_char_conf: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_char_conf_aligned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_evidence: Option<crate::ocr_word_evidence::WordEvidenceEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_word_evidence: Option<crate::ocr_word_evidence::WordEvidenceEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_components: Option<Vec<PassageSourceComponent>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator_before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_source_groups: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ExportPage {
    pub image_path: String,
    pub width: i32,
    pub height: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_width_points: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_height_points: Option<f64>,
    pub regions: Vec<ExportRegion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_page: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structure: Option<crate::ocr_document::DocumentStructure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub figure_regions: Option<Vec<ExportRegion>>,
}

fn structure_sources(regions: &[ExportRegion]) -> Vec<crate::ocr_document::StructureSource<'_>> {
    regions.iter().map(|region| crate::ocr_document::StructureSource {
        id:region.id.as_deref().unwrap_or(""),text:region.text.as_str(),kind:region.kind.as_deref().unwrap_or("text"),
    }).collect()
}

fn validate_export_pages(pages: &[ExportPage]) -> Result<(), String> {
    if pages.is_empty() || pages.len() > MAX_PAGES { return Err("Select at least one page to export.".into()); }
    for (i, page) in pages.iter().enumerate() {
        let (width, height) = checked_dimensions(Path::new(&page.image_path))?;
        if page.width <= 0 || page.height <= 0 || page.width as u32 != width || page.height as u32 != height {
            return Err(format!("Page {} dimensions do not match its image. Recognize the current page image before exporting.", i + 1));
        }
        checked_physical_size(page.page_width_points, page.page_height_points)
            .map_err(|e| format!("Page {}: {e}", i + 1))?;
        let figures=page.figure_regions.as_deref().unwrap_or(&[]);
        if page.regions.len() + figures.len() > MAX_REGIONS { return Err(format!("Page {} has too many regions.", i + 1)); }
        if !figures.is_empty() && page.structure.is_none() { return Err("Figure source records require a document structure.".into()); }
        if figures.iter().any(|region| region.kind.as_deref()!=Some("picture")) { return Err("Figure source records must describe picture regions.".into()); }
        if let Some(structure)=&page.structure {
            crate::ocr_document::validate_structure(structure,&structure_sources(&page.regions),&structure_sources(figures))?;
        }
        let mut source_bytes = 0;
        if let Some(archived) = page.provenance.as_ref().and_then(|value| value.get("passageSources")) {
            let components: Vec<PassageSourceComponent> = serde_json::from_value(archived.clone()).map_err(|e| format!("Invalid archived passage sources: {e}"))?;
            source_bytes += validate_source_components(&components)?;
        }
        for (j, region) in page.regions.iter().chain(figures).enumerate() {
            checked_rect(PixelRect { x: region.x as f64, y: region.y as f64, w: region.w as f64, h: region.h as f64 }, width, height)
                .map_err(|e| format!("Page {}, region {}: {e}", i + 1, j + 1))?;
            if region.confidence.is_some_and(|score| !score.is_finite() || !(0.0..=1.0).contains(&score)) {
                return Err(format!("Page {}, region {} has an invalid confidence.", i + 1, j + 1));
            }
            if let Some(evidence) = &region.original_word_evidence {
                let original = region.original_text.as_deref().ok_or("Original word evidence requires its original text.")?;
                crate::ocr_word_evidence::validate_word_evidence(evidence, original, width, height)?;
            }
            if let Some(evidence) = &region.word_evidence {
                let original = region.original_text.as_deref().ok_or("Current word evidence requires its original text.")?;
                if original != region.text {
                    return Err("Edited text cannot retain the original word confidence or word boxes.".into());
                }
                crate::ocr_word_evidence::validate_word_evidence(evidence, &region.text, width, height)?;
            }
            if let Some(components) = &region.source_components {
                source_bytes += validate_source_components(components)?;
            }
            if source_bytes > 16 * 1024 * 1024 { return Err("Page source history exceeds 16 MB. No evidence was truncated.".into()); }
            if region.separator_before.as_ref().is_some_and(|separator| !["", " ", "\n", "\n\n"].contains(&separator.as_str())) {
                return Err("Invalid passage separator.".into());
            }
            if let Some(groups) = &region.shared_source_groups {
                let unique: HashSet<_> = groups.iter().collect();
                if groups.len() > 128 || unique.len() != groups.len() || groups.iter().any(|group|
                    group.trim().is_empty() || group.encode_utf16().count() > 200 || group.contains('\0')) {
                    return Err("Invalid shared source groups.".into());
                }
            }
        }
    }
    Ok(())
}

fn output_path(path: &str, pages: &[ExportPage], source_uris: &[String], root: &Path) -> Result<PathBuf, String> {
    let requested = Path::new(path);
    if !requested.is_absolute() { return Err("Choose an absolute output file path.".into()); }
    let parent = requested.parent().ok_or("Choose an output directory.")?;
    let parent = fs::canonicalize(parent).map_err(|e| format!("Open output directory: {e}"))?;
    let file_name = requested.file_name().ok_or("Choose an output filename.")?;
    let resolved = parent.join(file_name);
    let canonical = if resolved.exists() { fs::canonicalize(&resolved).map_err(|e| e.to_string())? } else { resolved.clone() };
    if canonical.starts_with(fs::canonicalize(root).map_err(|e| e.to_string())?) {
        return Err("Choose an export location outside the OCR project data directory.".into());
    }
    for source in pages.iter().map(|p| p.image_path.as_str()).chain(source_uris.iter().map(String::as_str)) {
        if let Ok(source) = local_file(source) {
            if canonical == source { return Err("The export cannot replace a source document or page image. Choose a different filename.".into()); }
        }
    }
    Ok(resolved)
}

/// Reviewed passages per page for the text-only writers. An explicit passage
/// separator (from a split or merge, including a blank line) continues the
/// previous paragraph; passages without one stay separate paragraphs, as before.
fn export_text_paragraphs(pages: &[ExportPage]) -> Vec<Vec<String>> {
    pages.iter().map(|page| {
        let mut paragraphs: Vec<String> = Vec::with_capacity(page.regions.len());
        for (index, region) in page.regions.iter().enumerate() {
            match (paragraphs.last_mut(), region.separator_before.as_deref()) {
                (Some(current), Some(separator)) if index > 0 => {
                    current.push_str(separator);
                    current.push_str(&region.text);
                }
                _ => paragraphs.push(region.text.clone()),
            }
        }
        paragraphs
    }).collect()
}

fn borrowed_pages(pages: &[Vec<String>]) -> Vec<Vec<&str>> {
    pages.iter().map(|page| page.iter().map(String::as_str).collect()).collect()
}

fn export_text_pages(pages: &[ExportPage]) -> Vec<Vec<&str>> {
    pages.iter().map(|page| page.regions.iter().map(|region| region.text.as_str()).collect()).collect()
}

fn joined_text(pages: &[ExportPage]) -> String {
    pages.iter().map(|page| joined_page_text(page, "\n")).collect::<Vec<_>>().join("\u{000c}")
}

fn joined_page_text(page: &ExportPage, default_separator: &str) -> String {
    let mut text = String::new();
    for (index, region) in page.regions.iter().enumerate() {
        if index > 0 { text.push_str(region.separator_before.as_deref().unwrap_or(default_separator)); }
        text.push_str(&region.text);
    }
    text
}

fn has_shared_source_overlap(page: &ExportPage) -> bool {
    let mut groups: std::collections::HashMap<&str, Vec<&ExportRegion>> = std::collections::HashMap::new();
    for region in &page.regions {
        for id in region.shared_source_groups.iter().flatten() {
            let members = groups.entry(id.as_str()).or_default();
            if members.iter().any(|other| (other.x + other.w).min(region.x + region.w) > other.x.max(region.x)
                && (other.y + other.h).min(region.y + region.h) > other.y.max(region.y)) { return true; }
            members.push(region);
        }
    }
    false
}

fn transcription_confidence(region: &ExportRegion) -> f32 {
    let unknown = if cfg!(feature = "ocr-reviewed-pdf") { -1.0 } else { 0.0 };
    if region.confidence_basis.as_deref() == Some("recognition")
        && region.original_text.as_deref() == Some(region.text.as_str()) {
        region.confidence.filter(|score| score.is_finite() && (0.0..=1.0).contains(score)).unwrap_or(unknown)
    } else { unknown }
}

/// Adapt validated current page content to the pure Word writer. Capture paths
/// in provenance are never opened; pictures come only from the current page
/// images already admitted by managed_page and validate_export_pages.
#[cfg(feature = "ocr-office")]
fn export_docx(pages: &[ExportPage], options: &crate::ocr_office::OfficeOptions) -> Result<crate::ocr_docx::DocxOutput, String> {
    use crate::ocr_docx::{DocxFigure, DocxPage, DocxRegion};
    use crate::ocr_office::{OfficeSourcePage, OfficeSourceRegion, select_office_objects};
    // Positional identities keep plain legacy exports readable, but must never
    // authorize an explicit extraction selection or removal of source text.
    for selection in &options.objects {
        let page = pages.iter().find(|page| page.id.as_deref() == Some(selection.page_id.as_str()))
            .ok_or("Selected Word content requires the supplied stable page identity within this export scope. Reopen the project and select it again.")?;
        if selection.placement == "replace-region" && !page.regions.iter().any(|region|
            region.id.as_deref().is_some_and(|id| Some(id) == selection.region_id.as_deref())) {
            return Err("Replacing Word source text requires the supplied stable passage identity. Choose the passage again.".into());
        }
    }
    let identities: Vec<_> = pages.iter().enumerate().map(|(index, page)|
        page.id.clone().unwrap_or_else(|| format!("legacy-page-{}", index + 1))).collect();
    let region_ids: Vec<Vec<String>> = pages.iter().map(|page| page.regions.iter().enumerate().map(|(index, region)|
        region.id.clone().unwrap_or_else(|| format!("legacy-region-{}", index + 1))).collect()).collect();
    let sources: Vec<_> = pages.iter().enumerate().map(|(index, page)| OfficeSourcePage {
        id: identities[index].as_str(), provenance: page.provenance.as_ref(),
        regions: page.regions.iter().enumerate().map(|(position, region)| OfficeSourceRegion {
            id: region_ids[index][position].as_str(), kind: region.kind.as_deref().unwrap_or("text"),
        }).collect(),
    }).collect();
    let selected = select_office_objects(&sources, options)?;
    let figure_count: usize = pages.iter().map(|page| page.figure_regions.as_ref().map_or(0, Vec::len)).sum();
    if figure_count > 1_024 { return Err("Word export supports at most 1,024 pictures per document. Export a smaller page selection.".into()); }
    let mut picture_bytes = 0usize;
    let mut document = Vec::with_capacity(pages.len());
    for (index, page) in pages.iter().enumerate() {
        let width_points = page.page_width_points.unwrap_or(f64::from(page.width) * 72.0 / 300.0);
        let height_points = page.page_height_points.unwrap_or(f64::from(page.height) * 72.0 / 300.0);
        let mut figures = vec![];
        if let Some(regions) = page.figure_regions.as_ref().filter(|regions| !regions.is_empty()) {
            let image = decode_page(Path::new(&page.image_path))?;
            for region in regions {
                let region_id = region.id.as_deref().ok_or("A Word picture needs its stable source region identity.")?;
                let (x, y, width, height) = checked_rect(PixelRect {
                    x: f64::from(region.x), y: f64::from(region.y), w: f64::from(region.w), h: f64::from(region.h),
                }, image.width(), image.height())?;
                let mut output = std::io::Cursor::new(Vec::new());
                image.crop_imm(x, y, width, height).write_to(&mut output, image::ImageFormat::Png)
                    .map_err(|error| format!("Prepare current picture pixels: {error}"))?;
                let png = output.into_inner();
                picture_bytes = picture_bytes.saturating_add(png.len());
                if picture_bytes > 64 * 1024 * 1024 { return Err("Word picture data exceeds 64 MB. Export a smaller page selection.".into()); }
                figures.push(DocxFigure { region_id: region_id.to_owned(), png, width_px: width, height_px: height,
                    width_points: f64::from(width) / f64::from(page.width) * width_points,
                    height_points: f64::from(height) / f64::from(page.height) * height_points });
            }
        }
        document.push(DocxPage {
            id: identities[index].clone(), page_width_points: width_points, page_height_points: height_points,
            regions: page.regions.iter().enumerate().map(|(position, region)| DocxRegion {
                id: region_ids[index][position].clone(), kind: region.kind.clone().unwrap_or_else(|| "text".into()),
                text: region.text.clone(), separator_before: region.separator_before.clone(),
            }).collect(),
            figures, structure: page.structure.clone(),
            objects: selected.iter().filter(|(page_id, _)| page_id == &identities[index]).map(|(_, object)| object.clone()).collect(),
        });
    }
    crate::ocr_docx::render_docx(&document)
}

/// A workbook contains only saved table strings. Current page images and OCR
/// geometry are deliberately not inputs, so historical tables remain exportable
/// after a source image is removed or a page is waiting for recognition.
#[cfg(feature = "ocr-office")]
fn export_xlsx(pages: &[ExportPage], options: &crate::ocr_office::WorkbookOptions) -> Result<crate::ocr_xlsx::XlsxOutput, String> {
    use crate::ocr_office::{OfficeSourcePage, select_workbook_tables};
    let sources = pages.iter().map(|page| {
        Ok(OfficeSourcePage {
            id: page.id.as_deref().ok_or("Workbook selections require supplied stable page identities. Reopen the project and select the tables again.")?,
            provenance: page.provenance.as_ref(), regions: vec![],
        })
    }).collect::<Result<Vec<_>, String>>()?;
    let selected = select_workbook_tables(&sources, options)?;
    let sheets = selected.into_iter().map(|sheet| crate::ocr_xlsx::XlsxSheet {
        page_id: sheet.page_id, object_id: sheet.object_id, name: sheet.name, table: sheet.table,
    }).collect::<Vec<_>>();
    crate::ocr_xlsx::render_xlsx(&sheets)
}

fn validate_export_format(format: &str, pdfa: bool) -> Result<(), String> {
    if pdfa && format != "pdf" { return Err("PDF/A applies only to PDF output.".into()); }
    if pdfa && !crate::extractors::ocr_render::reviewed_archival_pdf_available() {
        return Err("PDF/A-2b requires a matching archival-capable native renderer.".into());
    }
    if OcrOutputFormat::from_name(format) == Some(OcrOutputFormat::Pdf) && !reviewed_pdf_available() {
        return Err("Searchable PDF requires the matching updated CrispEmbed renderer and an OCR workspace build. Choose hOCR, ALTO, text, or JSON with this build.".into());
    }
    if format == "docx" && !cfg!(any(feature = "ocr-office", feature = "ocr-text-export")) {
        return Err("Word export is unavailable in this build. Choose Plain text or HTML document.".into());
    }
    if format == "xlsx" && !cfg!(feature = "ocr-office") {
        return Err("Workbook export is unavailable in this build. Install a build with Office export support.".into());
    }
    Ok(())
}

/// Overlapping shared sources cannot be bound to page geometry. Checked before
/// renderer availability so the actionable passage error is reported first.
fn validate_shared_sources(pages: &[ExportPage], format: &str) -> Result<(), String> {
    if OcrOutputFormat::from_name(format).is_some_and(|format| format.needs_render_binding()) && pages.iter().any(has_shared_source_overlap) {
        return Err("Passages share overlapping source areas. Divide their boxes or merge the passages before exporting PDF, hOCR, or ALTO. Text and JSON retain the exact edited text.".into());
    }
    Ok(())
}

fn validate_export_text(pages: &[ExportPage], format: &str) -> Result<(), String> {
    if ["pdf", "html", "hocr", "alto"].contains(&format) {
        crate::ocr_text_export::validate_xml_text(&export_text_pages(pages))?;
    }
    if OcrOutputFormat::from_name(format) == Some(OcrOutputFormat::Pdf)
        && !crate::extractors::ocr_render::reviewed_bidi_pdf_available()
        && pages.iter().any(|page| page.regions.iter().any(|region| crate::ocr_text_export::needs_bidirectional_pdf(&region.text))) {
        return Err(crate::ocr_text_export::PDF_BIDI_UNAVAILABLE.into());
    }
    // The structured Office writer validates its own text and object bounds.
    if format == "docx" && !cfg!(feature = "ocr-office") {
        crate::ocr_text_export::validate_docx(&borrowed_pages(&export_text_paragraphs(pages)))?;
    }
    Ok(())
}

fn reviewed_render_pages(pages: &[ExportPage]) -> Vec<RenderPage> {
    pages.iter().map(|page| RenderPage::from_regions(page.regions.iter().map(|region| OcrRegion {
        text: region.text.clone(), x: region.x, y: region.y, w: region.w, h: region.h,
        confidence: transcription_confidence(region),
    }).collect(), page.width, page.height, page.image_path.clone()).with_physical_size(
        page.page_width_points.unwrap_or(f64::from(page.width) * 72.0 / 300.0) as f32,
        page.page_height_points.unwrap_or(f64::from(page.height) * 72.0 / 300.0) as f32,
    )).collect()
}

fn export_bytes(pages: &[ExportPage], format: &str, pdfa: bool) -> Result<Vec<u8>, String> {
    validate_shared_sources(pages, format)?;
    validate_export_format(format, pdfa)?;
    validate_export_text(pages, format)?;
    match format {
        // Passage separators decide line joins; without them this equals
        // ocr_text_export::text (LF between passages, form feed between pages).
        "text" | "txt" | "plain" => Ok(joined_text(pages).into_bytes()),
        "json" => serde_json::to_vec_pretty(&json!({"schema_version": 1, "pages": pages})).map_err(|e| e.to_string()),
        "markdown" => Ok(pages.iter().enumerate().map(|(i, p)| format!("<!-- Page {} -->\n\n{}", i + 1, joined_page_text(p, "\n\n"))).collect::<Vec<_>>().join("\n\n---\n\n").into_bytes()),
        "html" => {
            crate::ocr_text_export::html(&borrowed_pages(&export_text_paragraphs(pages))).map(String::into_bytes)
        },
        "docx" => {
            #[cfg(feature = "ocr-office")]
            { export_docx(pages, &crate::ocr_office::OfficeOptions::default()).map(|output| output.bytes) }
            #[cfg(all(not(feature = "ocr-office"), feature = "ocr-text-export"))]
            { crate::ocr_text_export::docx(&borrowed_pages(&export_text_paragraphs(pages))) }
            #[cfg(not(any(feature = "ocr-office", feature = "ocr-text-export")))]
            { Err("Word export is unavailable in this build.".into()) }
        },
        _ => {
            let format = OcrOutputFormat::from_name(format).ok_or_else(|| format!("Unsupported OCR export format: {format}"))?;
            let render_pages = reviewed_render_pages(pages);
            crate::extractors::ocr_render::render(&render_pages, format, pdfa).map_err(|e| format!("Render OCR document: {e:#}"))
        },
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
const MAX_STREAMED_PDF_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const MAX_STREAMED_PDF_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Stream PDFs into the same atomic output transaction used by other exports.
/// The limit bounds file size, not process memory; page decoding still needs
/// working memory. Mobile matches the native document-provider copy limit.
fn write_export(
    path: &Path,
    pages: &[ExportPage],
    format: &str,
    pdfa: bool,
    overwrite: bool,
    max_pdf_bytes: u64,
    prepared: Option<Vec<u8>>,
    control: &ExportControl,
    mut progress: impl FnMut(&str, u64),
) -> Result<usize, String> {
    let log=OperationLog::start("ocr.write_export",json!({"format":format,"pages":pages.len(),"overwrite":overwrite}));
    let result=(|| {
    log.step("validating",Value::Null);
    control.check().map_err(|error| error.to_string())?;
    validate_shared_sources(pages, format)?;
    validate_export_format(format, pdfa)?;
    validate_export_text(pages, format)?;
    let streaming_pdf = OcrOutputFormat::from_name(format) == Some(OcrOutputFormat::Pdf)
        && crate::extractors::ocr_render::reviewed_streaming_pdf_available();
    atomic_write_with(path, overwrite, |file| {
        control.check().map_err(|error| error.to_string())?;
        progress("rendering", 0);
        log.step("rendering",Value::Null);
        let written = {
            let mut buffered = std::io::BufWriter::with_capacity(64 * 1024, &mut *file);
            let mut last_update = std::time::Instant::now();
            // Cancellation is outside BufWriter so Interrupted reaches the
            // native callback instead of being retried by its flush loop.
            let mut writer = ControlledWriter::new(&mut buffered, control, |bytes| {
                if last_update.elapsed() >= std::time::Duration::from_millis(100) {
                    progress("writing", bytes);
                    last_update = std::time::Instant::now();
                }
            });
            let count = if streaming_pdf {
                crate::extractors::ocr_render::render_pdf_to_writer(
                    &reviewed_render_pages(pages), pdfa, max_pdf_bytes, &mut writer,
                ).map_err(|error| format!("Render OCR PDF: {error:#}"))?
            } else {
                // Office documents are rendered by the caller, which also
                // reports their warnings; everything else renders here.
                let bytes = match prepared { Some(bytes) => bytes, None => export_bytes(pages, format, pdfa)? };
                writer.write_all_checked(&bytes).map_err(|error| format!("Write OCR export: {error}"))?;
                bytes.len() as u64
            };
            writer.flush().map_err(|error| format!("Flush OCR export: {error}"))?;
            count
        };
        if written == 0 && !["text", "txt", "plain"].contains(&format) {
            return Err("The renderer produced an empty document.".into());
        }
        let actual = file.metadata().map_err(|error| format!("Check rendered document: {error}"))?.len();
        if actual != written {
            return Err("The renderer byte count does not match the staged document. No output was published.".into());
        }
        let count = usize::try_from(written).map_err(|_| "The rendered document is too large for this device.")?;
        control.begin_commit().map_err(|error| error.to_string())?;
        log.step("committing",json!({"bytes":written}));
        progress("committing", written);
        Ok(count)
    })
    })();
    match &result {
        Ok(bytes)=>log.succeed(json!({"bytes":bytes,"saved":true})),
        Err(error) if atomic_output::is_export_cancelled(error)=>log.cancel(),
        Err(_)=>log.fail("export_write_failed"),
    }
    result
}

#[tauri::command]
pub fn ocr_workspace_cancel_export(request_id: String) -> Result<bool, String> {
    let log=OperationLog::start("ocr.export_cancel",json!({"surface":"gui"}));
    let result=export_control::cancel(&request_id);
    match &result {Ok(accepted)=>log.succeed(json!({"accepted":accepted})),Err(_)=>log.fail("cancel_request_failed")}
    result
}

#[derive(Debug, Serialize)]
pub struct WorkspaceExportResult {
    pub saved_path: String, pub bytes: usize, pub pages: usize, pub format: String,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheets: Option<usize>,
}

#[tauri::command]
pub async fn ocr_workspace_export(
    app: tauri::AppHandle,
    output_path: String,
    format: String,
    pdfa: bool,
    pages: Vec<ExportPage>,
    overwrite: Option<bool>,
    source_uris: Option<Vec<String>>,
    request_id: Option<String>,
    office_options: Option<crate::ocr_office::OfficeOptions>,
    workbook_options: Option<crate::ocr_office::WorkbookOptions>,
) -> Result<WorkspaceExportResult, String> {
    let log=std::sync::Arc::new(OperationLog::start_with_id("ocr.export",json!({"surface":"gui","format":format,"pages":pages.len()}),request_id.as_deref()));
    let result: Result<WorkspaceExportResult,String> = async {
    log.step("validating",Value::Null);
    let job = ExportJob::begin(request_id)?;
    let root = workspace_root(&app)?;
    let worker_log=log.clone();
    tokio::task::spawn_blocking(move || {
        if office_options.is_some() && format != "docx" {
            return Err("Word object placement options are only supported for DOCX export.".into());
        }
        if workbook_options.is_some() && format != "xlsx" {
            return Err("Workbook worksheet choices are only supported for XLSX export.".into());
        }
        if format != "xlsx" {
            for page in &pages { managed_page(&root, &page.image_path)?; }
            validate_export_pages(&pages)?;
        } else if pages.is_empty() || pages.len() > MAX_PAGES {
            return Err("Select saved tables from 1–10,000 source pages for this workbook.".into());
        }
        let path = self::output_path(&output_path, &pages, &source_uris.unwrap_or_default(), &root)?;
        #[allow(unused_mut)]
        let (mut prepared, mut warnings, mut sheets, mut exported_pages) = (None, vec![], None, pages.len());
        if format == "xlsx" && !pdfa {
            #[cfg(feature = "ocr-office")]
            {
                let options = workbook_options.ok_or("Select at least one saved table for this workbook.")?;
                let output = export_xlsx(&pages, &options)?;
                exported_pages = options.sheets.iter().map(|sheet| sheet.page_id.as_str()).collect::<HashSet<_>>().len();
                sheets = Some(options.sheets.len());
                warnings = output.warnings;
                prepared = Some(output.bytes);
            }
            #[cfg(not(feature = "ocr-office"))]
            { let _ = workbook_options; return Err("Workbook export is unavailable in this build. Install a build with Office export support.".into()); }
        } else if format == "docx" && !pdfa {
            #[cfg(feature = "ocr-office")]
            {
                let output = export_docx(&pages, &office_options.unwrap_or_default())?;
                warnings = output.warnings;
                prepared = Some(output.bytes);
            }
            #[cfg(not(feature = "ocr-office"))]
            { let _ = office_options; }
        }
        let mut last_stage="";
        let mut last_log=std::time::Instant::now();
        let bytes = write_export(&path, &pages, &format, pdfa, overwrite.unwrap_or(false),
            MAX_STREAMED_PDF_BYTES, prepared, job.control(), |phase, bytes| {
                let stage=match phase {"rendering"=>"rendering","committing"=>"committing",_=>"writing"};
                if stage!=last_stage || last_log.elapsed()>=std::time::Duration::from_secs(5) {
                    worker_log.step(stage,json!({"bytes":bytes})); last_stage=stage;last_log=std::time::Instant::now();
                }
                let _ = app.emit("ocr-workspace-export", json!({
                    "request_id": job.id(), "phase": phase, "bytes": bytes,
                }));
            })?;
        Ok(WorkspaceExportResult {
            saved_path: path.to_string_lossy().into_owned(), bytes, pages: exported_pages,
            format: if pdfa { "pdfa".into() } else { format },
            warnings, sheets,
        })
    }).await.map_err(|e| format!("Export worker: {e}"))?
    }.await;
    match &result {
        Ok(export)=>log.succeed(json!({"bytes":export.bytes,"pages":export.pages,"format":export.format,"warnings":export.warnings.len()})),
        Err(error) if atomic_output::is_export_cancelled(error)=>log.cancel(),
        Err(_)=>log.fail("export_failed"),
    }
    result
}

/// Create a private render destination before showing the mobile save picker.
/// Its own directory preserves the requested display name without collisions.
#[tauri::command]
pub fn ocr_workspace_export_target(app: tauri::AppHandle, name: String) -> Result<String, String> {
    if name.is_empty() || name.len() > 240 || name.contains(['/', '\\']) || name.chars().any(char::is_control)
        || name == "." || name == ".." {
        return Err("Choose a short filename without directory separators.".into());
    }
    let directory = app.path().app_cache_dir().map_err(|e| e.to_string())?
        .join("ocr-exports").join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&directory).map_err(|e| format!("Create export staging directory: {e}"))?;
    Ok(directory.join(name).to_string_lossy().into_owned())
}

#[tauri::command]
pub fn ocr_workspace_discard_export(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let root = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("ocr-exports");
    discard_staged_export(&root, Path::new(&path))
}

fn discard_staged_export(root: &Path, path: &Path) -> Result<(), String> {
    let cache = fs::canonicalize(root.parent().ok_or("Invalid export staging root.")?).map_err(|e| e.to_string())?;
    let expected_root = cache.join("ocr-exports");
    let root = fs::canonicalize(root).map_err(|e| format!("Open export staging: {e}"))?;
    if root != expected_root { return Err("Export staging must remain in the application cache.".into()); }
    let directory = path.parent().ok_or("Invalid staged export.")?;
    let directory = fs::canonicalize(directory).map_err(|e| format!("Open staged export directory: {e}"))?;
    if directory.parent() != Some(root.as_path())
        || directory.file_name().and_then(|v| v.to_str()).and_then(|v| uuid::Uuid::parse_str(v).ok()).is_none() {
        return Err("Only temporary OCR export files can be discarded.".into());
    }
    let name = path.file_name().ok_or("Invalid staged export filename.")?;
    let file = directory.join(name);
    if file.exists() {
        let canonical = fs::canonicalize(&file).map_err(|e| e.to_string())?;
        if canonical.parent() != Some(directory.as_path()) { return Err("A staged export cannot point outside its directory.".into()); }
        fs::remove_file(file).map_err(|e| format!("Remove staged export: {e}"))?;
    }
    // Only an empty, single-export directory is removed; never recursive.
    let _ = fs::remove_dir(directory);
    Ok(())
}

/// Save extracted fields, table markup/CSV or formula source with the same
/// source protection and atomic write behavior as document export.
#[tauri::command]
pub async fn ocr_workspace_write_text(
    app: tauri::AppHandle,
    output_path: String,
    text: String,
    source_uris: Vec<String>,
    overwrite: Option<bool>,
) -> Result<Value, String> {
    let root = workspace_root(&app)?;
    if text.len() > 64 * 1024 * 1024 { return Err("Text export exceeds the 64 MB limit.".into()); }
    if text.contains('\0') { return Err("Text export contains null characters.".into()); }
    tokio::task::spawn_blocking(move || {
        let path = self::output_path(&output_path, &[], &source_uris, &root)?;
        atomic_write(&path, text.as_bytes(), overwrite.unwrap_or(false))?;
        Ok(json!({"saved_path":path.to_string_lossy(),"bytes":text.len()}))
    }).await.map_err(|e| format!("Text export worker: {e}"))?
}

/// Extract fields from the reviewed text, so manual corrections are reflected
/// in field values instead of being replaced by a second OCR pass.
#[tauri::command]
pub async fn ocr_workspace_extract_fields(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    text: String,
    labels: Vec<String>,
    threshold: Option<f32>,
) -> Result<Vec<KieFieldDto>, String> {
    if !cfg!(feature = "crispembed") { return Err("Field extraction requires CrispEmbed.".into()); }
    if text.trim().is_empty() { return Err("Recognize or enter text before extracting fields.".into()); }
    if text.len() > 1_000_000 { return Err("Select at most 1 MB of reviewed text for one field extraction.".into()); }
    let labels: Vec<_> = labels.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if labels.is_empty() || labels.len() > 64 || labels.iter().any(|s| s.len() > 128) {
        return Err("Enter between 1 and 64 field labels, each at most 128 characters.".into());
    }
    let threshold = threshold.unwrap_or(0.5);
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) { return Err("Field confidence must be between 0 and 1.".into()); }
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let (model, cache_dir) = {
        let index = state.index.lock().await;
        (index.config.ner_model.unwrap_or_default(), crate::index::resolve_model_cache_dir(&index.config, &data_dir))
    };
    let handle = crate::index::ner::NerHandle::new(model, labels, threshold, 0, 0, cache_dir);
    let fields = handle.try_extract_fields(&text).await.map_err(|e| format!("Field extraction: {e:#}"))?;
    Ok(fields.into_iter().map(|(label, value, score)| KieFieldDto { label, value, score }).collect())
}

#[cfg(test)]
mod tests {
    fn word(text: &str, x: f32, y: f32, w: f32, h: f32, score: f32) -> crate::extractors::ocr_crispembed::OcrReviewRegion {
        crate::extractors::ocr_crispembed::OcrReviewRegion {
            text: text.into(), x, y, w, h, confidence: Some(score), confidence_basis: "recognition".into(),
            char_conf: vec![score; text.chars().count()], char_conf_unit: "unicode_codepoint".into(),
            char_conf_aligned: true, word_evidence: None, kind: "text".into(),
        }
    }

    #[test]
    fn consecutive_lines_become_paragraphs_and_separate_blocks_stay_apart() {
        let mut title = word("Kopf", 0.0, 0.0, 200.0, 40.0, 0.9);
        title.kind = "title".into();
        let paragraphs = group_lines_into_paragraphs(vec![
            title,
            word("erste Zeile", 0.0, 60.0, 300.0, 20.0, 0.9),
            word("zweite Zeile", 2.0, 84.0, 280.0, 20.0, 0.5),
            // A blank line's distance starts a new paragraph.
            word("neuer Absatz", 0.0, 140.0, 250.0, 20.0, 0.8),
            // Another column, beside the paragraph above.
            word("rechte Spalte", 600.0, 162.0, 200.0, 20.0, 0.8),
        ]);
        assert_eq!(paragraphs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["Kopf", "erste Zeile\nzweite Zeile", "neuer Absatz", "rechte Spalte"]);
        let joined = &paragraphs[1];
        assert_eq!((joined.x, joined.y, joined.w, joined.h), (0.0, 60.0, 300.0, 44.0));
        assert!(joined.char_conf_aligned);
        assert_eq!(joined.char_conf.len(), joined.text.chars().count());
        let expected = (0.9 * 11.0 + 0.5 * 12.0) / 23.0;
        assert!((joined.confidence.unwrap() - expected as f32).abs() < 1e-6);
    }

    #[test]
    fn paragraph_confidence_preserves_only_matching_valid_character_evidence() {
        let first = word("A💡é", 0.0, 0.0, 100.0, 20.0, 0.9);
        let second = word("次行", 0.0, 24.0, 100.0, 20.0, 0.6);
        let joined = group_lines_into_paragraphs(vec![first.clone(), second.clone()]);
        assert_eq!(joined[0].text, "A💡é\n次行");
        assert_eq!(joined[0].char_conf, [0.9, 0.9, 0.9, 0.6, 0.6, 0.6]);
        assert!(joined[0].char_conf_aligned);
        assert_eq!(joined[0].char_conf_unit, "unicode_codepoint");

        for basis in ["detection", "unknown"] {
            let mut other = second.clone();
            other.confidence_basis = basis.into();
            let joined = group_lines_into_paragraphs(vec![first.clone(), other]);
            assert_eq!(joined.len(), 1);
            assert_eq!(joined[0].confidence_basis, "unknown");
            assert!(joined[0].confidence.is_none());
            assert!(!joined[0].char_conf_aligned);
            assert!(joined[0].char_conf.is_empty());
        }
        // Equal lengths and matching units do not make token/unknown scores
        // codepoint evidence, even if every line reports the same unit.
        for unit in ["token", "unknown"] {
            let mut a = first.clone();
            let mut b = second.clone();
            a.char_conf_unit = unit.into();
            b.char_conf_unit = unit.into();
            let joined = group_lines_into_paragraphs(vec![a, b]);
            assert!(joined[0].confidence.is_some());
            assert_eq!(joined[0].confidence_basis, "recognition");
            assert!(!joined[0].char_conf_aligned);
            assert!(joined[0].char_conf.is_empty());
            assert_eq!(joined[0].char_conf_unit, "unknown");
        }
        for score in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
            let mut other = second.clone();
            other.char_conf[0] = score;
            let joined = group_lines_into_paragraphs(vec![first.clone(), other]);
            assert!(!joined[0].char_conf_aligned);
            assert!(joined[0].char_conf.is_empty());
        }
        for score in [None, Some(f32::NAN), Some(1.1)] {
            let mut other = second.clone();
            other.confidence = score;
            let joined = group_lines_into_paragraphs(vec![first.clone(), other]);
            assert!(joined[0].confidence.is_none(), "a partial mean cannot describe the whole paragraph");
            // Independent, valid codepoint evidence stays usable.
            assert!(joined[0].char_conf_aligned);
        }
        let joined = group_lines_into_paragraphs(vec![
            word("", 0.0, 0.0, 100.0, 20.0, 0.9),
            word("", 0.0, 24.0, 100.0, 20.0, 0.9),
        ]);
        assert!(!joined[0].char_conf_aligned);
        assert!(joined[0].char_conf.is_empty(), "an inserted newline must not invent character evidence");
    }

    #[test]
    fn paragraph_grouping_keeps_native_evidence_nontext_and_invalid_boxes_untouched() {
        use crate::ocr_word_evidence::{NativeWordEvidence, WordEvidenceEnvelope};
        let mut native = word("native", 0.0, 24.0, 100.0, 20.0, 0.8);
        native.word_evidence = Some(WordEvidenceEnvelope {
            version: 1, offset_unit: "utf16".into(), source: "tesseract_tsv".into(),
            words: vec![NativeWordEvidence {
                text: "native".into(), start: 0, end: 6, x: 0.0, y: 24.0, w: 100.0, h: 20.0,
                confidence: Some(0.8), confidence_basis: "recognition".into(), alternatives: None,
            }],
        });
        let mut table = word("table", 0.0, 72.0, 100.0, 20.0, 0.8);
        table.kind = "table".into();
        let invalid = word("outside", -1.0, 120.0, 100.0, 20.0, 0.8);
        let expected = [&native, &table, &invalid].map(|r| serde_json::to_value(r).unwrap());
        let grouped = group_lines_into_paragraphs(vec![
            word("before", 0.0, 0.0, 100.0, 20.0, 0.9), native,
            word("between", 0.0, 48.0, 100.0, 20.0, 0.9), table,
            word("after", 0.0, 96.0, 100.0, 20.0, 0.9), invalid,
            word("last", 0.0, 144.0, 100.0, 20.0, 0.9),
        ]);
        assert_eq!(grouped.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["before", "native", "between", "table", "after", "outside", "last"]);
        for (index, expected) in [1, 3, 5].into_iter().zip(expected) {
            assert_eq!(serde_json::to_value(&grouped[index]).unwrap(), expected);
        }
    }

    #[test]
    fn review_layout_classifies_lines_before_paragraphs_are_joined() {
        use crate::extractors::layout::{LayoutRegion, RegionKind};
        let layout = vec![
            LayoutRegion { x1: 0.0, y1: 0.0, x2: 300.0, y2: 20.0, score: 0.9, kind: RegionKind::Title, label_name: "title".into() },
            LayoutRegion { x1: 0.0, y1: 24.0, x2: 300.0, y2: 68.0, score: 0.9, kind: RegionKind::Text, label_name: "text".into() },
            LayoutRegion { x1: 0.0, y1: 72.0, x2: 300.0, y2: 92.0, score: 0.9, kind: RegionKind::Formula, label_name: "formula".into() },
            LayoutRegion { x1: 0.0, y1: 96.0, x2: 300.0, y2: 116.0, score: 0.9, kind: RegionKind::Text, label_name: "text".into() },
        ];
        let config = crate::extractors::OcrPipelineConfig::default();
        let mut diagnostics = json!({});
        let mut warnings = vec![];
        // Every recognizer line starts as ordinary text with identical size
        // and spacing. Only the real layout classifier provides boundaries.
        let regions = organize_review_regions(vec![
            word("heading", 0.0, 0.0, 300.0, 20.0, 0.9),
            word("body first", 0.0, 24.0, 300.0, 20.0, 0.9),
            word("body second", 0.0, 48.0, 300.0, 20.0, 0.9),
            word("x = y", 0.0, 72.0, 300.0, 20.0, 0.9),
            word("after formula", 0.0, 96.0, 300.0, 20.0, 0.9),
        ], &mut diagnostics, |regions, diagnostics| {
            apply_review_layout(&layout, &config, regions, diagnostics, &mut warnings);
        });
        assert_eq!(regions.iter().map(|r| (r.kind.as_str(), r.text.as_str())).collect::<Vec<_>>(),
            [("title", "heading"), ("text", "body first\nbody second"), ("formula", "x = y"), ("text", "after formula")]);
        assert_eq!(diagnostics["paragraph_grouping"], json!({"lines": 5, "paragraphs": 4}));
        assert!(warnings.is_empty());
    }

    #[test]
    fn grouping_receipts_share_recognition_scope_without_source_content() {
        let (_, events) = crate::operation_log::capture(|| {
            let log = OperationLog::start("ocr.workspace_recognition", json!({"surface":"gui"}));
            let mut diagnostics = json!({"full_text":"private document text", "recognition_image_path":"/private/source.png"});
            let regions = organize_review_regions(vec![
                word("private", 0.0, 0.0, 100.0, 20.0, 0.9),
                word("document", 110.0, 0.0, 100.0, 20.0, 0.9),
                word("text", 0.0, 24.0, 200.0, 20.0, 0.9),
            ], &mut diagnostics, |_, _| {});
            log_review_grouping(&log, &diagnostics);
            log.succeed(json!({"regions": regions.len()}));
        });
        assert_eq!(events.iter().map(|event| event.phase.as_str()).collect::<Vec<_>>(),
            ["started", "stage", "stage", "completed"]);
        assert!(events.iter().all(|event| event.operation_id == events[0].operation_id));
        assert_eq!(events[1].stage.as_deref(), Some("line_grouping"));
        assert_eq!(events[1].context["count"], json!(3));
        assert_eq!(events[1].context["regions"], json!(2));
        assert_eq!(events[2].stage.as_deref(), Some("paragraph_grouping"));
        assert_eq!(events[2].context["count"], json!(2));
        assert_eq!(events[2].context["regions"], json!(1));
        let logged = serde_json::to_string(&events).unwrap();
        assert!(!logged.contains("private"));
        assert!(!logged.contains("source.png"));
        assert!(!logged.contains("full_text"));
    }

    #[test]
    fn word_regions_on_one_line_become_one_aligned_line() {
        let lines = merge_word_regions_into_lines(vec![
            word("Welt", 60.0, 11.0, 40.0, 20.0, 0.9), word("Hallo", 0.0, 10.0, 50.0, 20.0, 0.5),
            word("zweite", 0.0, 50.0, 60.0, 20.0, 0.8), word("Zeile", 70.0, 51.0, 50.0, 20.0, 0.7),
        ]);
        assert_eq!(lines.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["Hallo Welt", "zweite Zeile"]);
        let first = &lines[0];
        assert!(first.char_conf_aligned);
        assert_eq!(first.char_conf.len(), first.text.chars().count());
        assert_eq!(first.char_conf[..5], [0.5; 5]); // "Hallo" keeps its own scores
        assert_eq!(first.char_conf[5], 0.5);        // the space takes the lower neighbour
        assert_eq!((first.x, first.w), (0.0, 100.0));
    }

    #[test]
    fn column_gutters_and_word_evidence_regions_are_not_joined() {
        let mut evidence = word("Kopf", 0.0, 0.0, 40.0, 20.0, 0.9);
        evidence.kind = "title".into();
        let lines = merge_word_regions_into_lines(vec![
            evidence, word("links", 0.0, 40.0, 50.0, 20.0, 0.9), word("rechts", 400.0, 40.0, 60.0, 20.0, 0.9),
        ]);
        assert_eq!(lines.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["Kopf", "links", "rechts"]);
    }

    #[test]
    fn word_line_grouping_preserves_native_column_reading_order() {
        let lines = merge_word_regions_into_lines(vec![
            word("left-top", 0.0, 0.0, 50.0, 20.0, 0.9),
            word("left-bottom", 0.0, 50.0, 50.0, 20.0, 0.9),
            word("right-top", 400.0, 0.0, 60.0, 20.0, 0.9),
            word("right-bottom", 400.0, 50.0, 60.0, 20.0, 0.9),
        ]);
        assert_eq!(lines.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["left-top", "left-bottom", "right-top", "right-bottom"]);
    }

    #[test]
    fn word_line_grouping_keeps_native_evidence_and_nontext_as_ordered_barriers() {
        use crate::ocr_word_evidence::{NativeWordEvidence, WordEvidenceEnvelope};
        let mut native = word("native", 50.0, 20.0, 40.0, 20.0, 0.9);
        native.word_evidence = Some(WordEvidenceEnvelope {
            version: 1, offset_unit: "utf16".into(), source: "tesseract_tsv".into(),
            words: vec![NativeWordEvidence {
                text: "native".into(), start: 0, end: 6, x: 50.0, y: 20.0, w: 40.0, h: 20.0,
                confidence: Some(0.7), confidence_basis: "recognition".into(), alternatives: None,
            }],
        });
        let mut picture = word("[image]", 0.0, 50.0, 100.0, 100.0, 0.8);
        picture.kind = "picture".into();
        let expected_native = serde_json::to_value(&native).unwrap();
        let expected_picture = serde_json::to_value(&picture).unwrap();
        let lines = merge_word_regions_into_lines(vec![
            word("before", 0.0, 20.0, 40.0, 20.0, 0.9), native,
            word("between", 100.0, 20.0, 50.0, 20.0, 0.9), picture,
            word("after", 160.0, 20.0, 40.0, 20.0, 0.9),
        ]);
        assert_eq!(lines.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["before", "native", "between", "[image]", "after"]);
        assert_eq!(serde_json::to_value(&lines[1]).unwrap(), expected_native);
        assert_eq!(serde_json::to_value(&lines[3]).unwrap(), expected_picture);
    }

    #[test]
    fn word_line_confidence_does_not_promote_mixed_bases_or_token_units() {
        for basis in ["detection", "unknown"] {
            let mut other = word("maybe", 50.0, 0.0, 40.0, 20.0, 0.99);
            other.confidence_basis = basis.into();
            let lines = merge_word_regions_into_lines(vec![word("seen", 0.0, 0.0, 40.0, 20.0, 0.9), other]);
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].text, "seen maybe");
            assert_eq!(lines[0].confidence_basis, "unknown");
            assert!(lines[0].confidence.is_none());
            assert!(!lines[0].char_conf_aligned);
            assert!(lines[0].char_conf.is_empty());
        }
        for unit in ["token", "unknown"] {
            let mut other = word("maybe", 50.0, 0.0, 40.0, 20.0, 0.9);
            other.char_conf_unit = unit.into();
            let lines = merge_word_regions_into_lines(vec![word("seen", 0.0, 0.0, 40.0, 20.0, 0.9), other]);
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].confidence_basis, "recognition");
            assert!(lines[0].confidence.is_some());
            assert!(!lines[0].char_conf_aligned);
            assert!(lines[0].char_conf.is_empty());
            assert_eq!(lines[0].char_conf_unit, "unknown");
        }
    }

    #[test]
    fn word_line_confidence_requires_scores_for_every_word() {
        for score in [None, Some(f32::NAN), Some(1.1)] {
            let mut unscored = word("unknown", 50.0, 0.0, 60.0, 20.0, 0.99);
            unscored.confidence = score;
            unscored.char_conf.clear();
            unscored.char_conf_aligned = false;
            let lines = merge_word_regions_into_lines(vec![word("known", 0.0, 0.0, 40.0, 20.0, 1.0), unscored]);
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].text, "known unknown");
            assert_eq!(lines[0].confidence_basis, "recognition");
            assert!(lines[0].confidence.is_none(), "a partial score cannot qualify the whole line");
            assert!(!lines[0].char_conf_aligned);
            assert!(lines[0].char_conf.is_empty());
        }
    }

    use super::*;

    #[test]
    fn captured_bytes_are_archived_and_imported_by_signature_without_modifying_sources() {
        let root = tempfile::tempdir().unwrap();
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(24, 32, image::Rgb([230, 220, 210])));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image.write_to(&mut encoded, image::ImageFormat::Png).unwrap();
        let bytes = encoded.into_inner();
        let captured = import_bytes(root.path(), "../camera\0.jpg", &bytes, false, None).unwrap();
        assert_eq!(captured.name, "camera.jpg");
        assert_eq!(captured.pages.len(), 1);
        assert_eq!((captured.pages[0].width, captured.pages[0].height), (24, 32));
        assert!(captured.source_uri.ends_with(".png"));
        assert_eq!(fs::read(&captured.source_uri).unwrap(), bytes);
        assert_ne!(captured.source_uri, captured.pages[0].image_path);
        let again = import_bytes(root.path(), "another.png", &bytes, false, None).unwrap();
        assert_eq!(again.source_uri, captured.source_uri);
        assert_eq!(again.pages[0].image_path, captured.pages[0].image_path);
        let provider = import_bytes(root.path(), "Selected scan.png", &bytes, true, Some("content://provider/Case%3AA%2FB")).unwrap();
        assert_eq!(provider.source_uri, "content://provider/Case%3AA%2FB");
        assert_eq!(provider.pages[0].image_path, captured.pages[0].image_path);
        assert_eq!(fs::read(&captured.source_uri).unwrap(), bytes);
    }

    #[test]
    fn capture_rejects_empty_unsupported_and_nonprovider_input_before_archiving() {
        let root = tempfile::tempdir().unwrap();
        assert!(import_bytes(root.path(), "page.jpg", &[], false, None).is_err());
        assert!(import_bytes(root.path(), "page.jpg", b"%PDF-1.7", false, None).is_err());
        assert!(import_bytes(root.path(), "page.jpg", b"invalid", true, Some("https://example.com/page")).is_err());
        assert!(!root.path().join("sources").exists());
    }

    fn pinned_oriented_jpeg() -> Vec<u8> {
        use base64::Engine;
        let encoded = include_str!("../../tests/fixtures/mobile-documents/oriented.jpg.b64")
            .split_whitespace().collect::<String>();
        base64::engine::general_purpose::STANDARD.decode(encoded).unwrap()
    }

    #[test]
    fn jpeg_file_and_camera_imports_reject_incomplete_sources_before_persisting_pixels() {
        let complete = pinned_oriented_jpeg();
        let missing_end = complete[..complete.len() - 2].to_vec();
        let truncated_scan = complete[..complete.len() - 100].to_vec();
        let mut metadata_end = vec![0xff, 0xd8, 0xff, 0xef, 0, 4, 0xff, 0xd9];
        metadata_end.extend_from_slice(&complete[2..complete.len() - 100]);
        for broken in [missing_end, truncated_scan, metadata_end] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("phone.jpg");
            fs::write(&source, &broken).unwrap();
            let error = import_document(root.path(), &source).unwrap_err();
            assert!(error.contains("JPEG source is incomplete or malformed"), "{error}");
            let error = import_bytes(root.path(), "phone.jpg", &broken, false, None).unwrap_err();
            assert!(error.contains("JPEG source is incomplete or malformed"), "{error}");
            assert_eq!(fs::read(&source).unwrap(), broken);
            let archives: Vec<_> = fs::read_dir(root.path().join("sources")).unwrap().collect();
            assert_eq!(archives.len(), 1);
            assert_eq!(fs::read(archives[0].as_ref().unwrap().path()).unwrap(), broken);
            let assets = root.path().join("assets");
            assert!(!assets.exists() || fs::read_dir(assets).unwrap().next().is_none());
        }
    }

    #[test]
    fn complete_jpeg_metadata_and_trailing_bytes_keep_archive_identity_and_orientation() {
        let complete = pinned_oriented_jpeg();
        let mut trailing = complete.clone();
        trailing.extend_from_slice(b"trailing camera metadata");
        let mut metadata_end = vec![0xff, 0xd8, 0xff, 0xef, 0, 4, 0xff, 0xd9];
        metadata_end.extend_from_slice(&complete[2..]);
        for bytes in [complete, trailing, metadata_end] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("phone.jpg");
            fs::write(&source, &bytes).unwrap();
            let local = import_document(root.path(), &source).unwrap();
            let imported = import_bytes(root.path(), "phone.jpg", &bytes, true,
                Some("content://provider/Photo%3ACase.jpg")).unwrap();
            assert_eq!(imported.source_uri, "content://provider/Photo%3ACase.jpg");
            assert_eq!(imported.pages.len(), 1);
            assert_eq!((imported.pages[0].width, imported.pages[0].height), (80, 120));
            assert_eq!(local.pages[0].image_path, imported.pages[0].image_path);
            let pixels = decode_page(Path::new(&imported.pages[0].image_path)).unwrap().to_rgb8();
            let top_left = pixels.get_pixel(10, 10);
            assert!(top_left[2] > 180 && top_left[0] < 70);
            assert_eq!(fs::read(imported.archive_uri.unwrap()).unwrap(), bytes);
            assert_eq!(fs::read(&source).unwrap(), bytes);
        }
    }

    #[test]
    fn camera_reimport_rejects_damaged_existing_pixels_and_preserves_originals() {
        let root = tempfile::tempdir().unwrap();
        let bytes = pinned_oriented_jpeg();
        let source = root.path().join("phone.jpg");
        fs::write(&source, &bytes).unwrap();
        let first = import_bytes(root.path(), "phone.jpg", &bytes, true,
            Some("content://provider/Photo%3ACase.jpg")).unwrap();
        let asset = Path::new(&first.pages[0].image_path);
        let complete = fs::read(asset).unwrap();
        let mut changed = complete.clone();
        changed[complete.len() / 2] ^= 1;
        for damaged in [&complete[..complete.len() - 1], changed.as_slice()] {
            fs::write(asset, damaged).unwrap();
            let error = import_document(root.path(), &source).unwrap_err();
            assert!(error.contains("Verify existing page snapshot"), "{error}");
            let error = import_bytes(root.path(), "phone.jpg", &bytes, true,
                Some("content://provider/Photo%3ACase.jpg")).unwrap_err();
            assert!(error.contains("Verify existing page snapshot"), "{error}");
            assert_eq!(fs::read(asset).unwrap(), damaged);
            assert_eq!(fs::read(&source).unwrap(), bytes);
            assert_eq!(fs::read(first.archive_uri.as_ref().unwrap()).unwrap(), bytes);
            assert_eq!(fs::read_dir(root.path().join("assets")).unwrap().count(), 1);
        }
        fs::write(asset, &complete).unwrap();
        let restored = import_bytes(root.path(), "phone.jpg", &bytes, false, None).unwrap();
        assert_eq!(restored.pages[0].image_path, first.pages[0].image_path);
        assert_eq!((restored.pages[0].width, restored.pages[0].height), (80, 120));
        assert_eq!(fs::read(asset).unwrap(), complete);
    }

    #[cfg(unix)]
    #[test]
    fn camera_import_cannot_reuse_an_asset_symlink_or_write_through_a_cache_alias() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let bytes = pinned_oriented_jpeg();
        let first = import_bytes(root.path(), "phone.jpg", &bytes, false, None).unwrap();
        let asset = Path::new(&first.pages[0].image_path);
        let complete = fs::read(asset).unwrap();
        let external = outside.path().join("unchanged.png");
        fs::write(&external, &complete).unwrap();
        fs::remove_file(asset).unwrap();
        symlink(&external, asset).unwrap();
        let error = import_bytes(root.path(), "phone.jpg", &bytes, false, None).unwrap_err();
        assert!(error.contains("Verify existing page snapshot"), "{error}");
        assert!(fs::symlink_metadata(asset).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(first.archive_uri.unwrap()).unwrap(), bytes);

        let aliased_root = tempfile::tempdir().unwrap();
        symlink(outside.path(), aliased_root.path().join("assets")).unwrap();
        let error = import_bytes(aliased_root.path(), "phone.jpg", &bytes, false, None).unwrap_err();
        assert!(error.contains("page directory must not be a symbolic link"), "{error}");
        assert_eq!(fs::read(&external).unwrap(), complete);
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
        let archives: Vec<_> = fs::read_dir(aliased_root.path().join("sources")).unwrap().collect();
        assert_eq!(archives.len(), 1);
        assert_eq!(fs::read(archives[0].as_ref().unwrap().path()).unwrap(), bytes);
    }

    #[test]
    fn export_cleanup_only_removes_a_private_staged_file() {
        let cache = tempfile::tempdir().unwrap();
        let root = cache.path().join("ocr-exports");
        let directory = root.join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&directory).unwrap();
        let original = cache.path().join("source.pdf");
        fs::write(&original, b"original source").unwrap();
        assert!(discard_staged_export(&root, &original).is_err());
        let file = directory.join("Reviewed.txt");
        fs::write(&file, b"reviewed").unwrap();
        discard_staged_export(&root, &file).unwrap();
        assert!(!directory.exists());
        assert_eq!(fs::read(&original).unwrap(), b"original source");
    }

    #[cfg(feature = "ocr-photo-tools")]
    #[test]
    fn explicit_photo_geometry_keeps_identity_and_rejects_invalid_corners() {
        let root = tempfile::tempdir().unwrap();
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_fn(80, 100, |x, y| image::Rgb([x as u8, y as u8, 210])));
        let original = persist_image(root.path(), &image).unwrap();
        let quad = vec![PhotoPoint { x: 0.0, y: 0.0 }, PhotoPoint { x: 79.0, y: 0.0 }, PhotoPoint { x: 79.0, y: 99.0 }, PhotoPoint { x: 0.0, y: 99.0 }];
        let path = Path::new(&original.image_path);
        let corrected = transform_page_with_photo(root.path(), path, "perspective", None, None, None, Some((80.0, 100.0)), Some(quad.clone()), None).unwrap();
        assert_eq!(corrected.pages[0].image_path, original.image_path);
        let no_rotation = transform_page_with_photo(root.path(), path, "deskew", None, None, None, Some((80.0, 100.0)), None, Some(0.0)).unwrap();
        assert_eq!(no_rotation.pages[0].image_path, original.image_path);
        let rotated = transform_page_with_photo(root.path(), path, "deskew", None, None, None, Some((80.0, 100.0)), None, Some(8.0)).unwrap();
        assert!(rotated.pages[0].width > 80 && rotated.pages[0].height > 100);
        assert_eq!(rotated.pages[0].page_width_points, Some(f64::from(rotated.pages[0].width)));
        let mut crossed = quad; crossed.swap(1, 2);
        assert!(transform_page_with_photo(root.path(), path, "perspective", None, None, None, None, Some(crossed), None).is_err());
        assert!(transform_page_with_photo(root.path(), path, "deskew", None, None, None, None, None, Some(f64::NAN)).is_err());
        assert_eq!(decode_page(path).unwrap().to_rgb8(), image.to_rgb8());
    }

    fn image() -> DynamicImage {
        DynamicImage::ImageRgb8(image::RgbImage::from_fn(8, 4, |x, y| image::Rgb([x as u8, y as u8, 255])))
    }

    fn project(root: &Path) -> Value {
        let asset = persist_image(root, &image()).unwrap();
        json!({"version":1,"id":"project-test","name":"Test project","documents":[],
            "pages":[{"id":"page-1","imagePath":asset.image_path,"originalImagePath":asset.image_path,"width":8,"height":4,"regions":[]}]})
    }

    fn export_page(root: &Path, text: &str) -> ExportPage {
        let asset = persist_image(root, &image()).unwrap();
        ExportPage { image_path: asset.image_path, width: 8, height: 4, page_width_points:None, page_height_points:None, regions: vec![ExportRegion {
            text: text.into(), x: 1.0, y: 1.0, w: 6.0, h: 2.0, confidence: Some(0.0), char_conf: vec![],
            id: None, original_text: None, confidence_basis: None, char_conf_unit: None,
            char_conf_aligned: None, original_char_conf: None, original_char_conf_aligned: None,
            word_evidence: None, original_word_evidence: None,
            source_components: None, separator_before: None, shared_source_groups: None,
            review: None, kind: None,
        }], id: None, name: None, source_uri: None, source_page: None, approved: None, provenance: None, structure:None, figure_regions:None }
    }

    #[test]
    fn export_receipts_follow_publication_and_never_log_reviewed_content() {
        let root=tempfile::tempdir().unwrap();
        let pages=vec![export_page(root.path(),"Private patient content")];
        let path=root.path().join("private-patient.txt");
        let (result,events)=crate::operation_log::capture(|| write_export(&path,&pages,"text",false,false,
            MAX_STREAMED_PDF_BYTES,None,&ExportControl::default(),|_,_|{}));
        assert_eq!(result.unwrap(),fs::metadata(&path).unwrap().len() as usize);
        assert_eq!(events.last().unwrap().phase,"completed");
        assert!(events.iter().any(|event|event.stage.as_deref()==Some("committing")));
        let original=fs::read(&path).unwrap();
        let (result,failed)=crate::operation_log::capture(|| write_export(&path,&pages,"text",false,false,
            MAX_STREAMED_PDF_BYTES,None,&ExportControl::default(),|_,_|{}));
        assert!(result.is_err());
        assert_eq!(fs::read(&path).unwrap(),original);
        assert_eq!(failed.last().unwrap().phase,"failed");
        assert!(!failed.iter().any(|event|event.phase=="completed"));
        let logged=serde_json::to_string(&[events,failed]).unwrap();
        assert!(!logged.contains("Private patient") && !logged.contains("private-patient"));
    }

    #[test]
    fn identical_page_snapshots_are_deduplicated_and_readable() {
        let root = tempfile::tempdir().unwrap();
        let first = persist_image(root.path(), &image()).unwrap();
        let second = persist_image(root.path(), &image()).unwrap();
        assert_eq!(first.image_path, second.image_path);
        assert_eq!(fs::read_dir(root.path().join("assets")).unwrap().count(), 1);
        assert_eq!(image::open(first.image_path).unwrap().to_rgb8(), image().to_rgb8());
    }

    #[test]
    fn system_word_boxes_translate_from_crop_once_without_inventing_character_scores() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n1\t1\t0\t0\t0\t0\t0\t0\t100\t40\t-1\t\n5\t1\t1\t1\t1\t1\t2\t3\t30\t20\t42\tInv0ice\n";
        let native = crate::ocr_word_evidence::parse_tesseract(tsv, "Inv0ice\n", 100, 40).unwrap();
        let regions = map_system_tesseract_regions(native.regions.clone(), (70, 80), 200, 150).unwrap();
        assert_eq!((regions[0].x, regions[0].y), (72.0, 83.0));
        let word = &regions[0].word_evidence.as_ref().unwrap().words[0];
        assert_eq!((word.x, word.y, word.start, word.end), (72.0, 83.0, 0, 7));
        assert_eq!(word.confidence, Some(0.42));
        assert!(regions[0].confidence.is_none());
        assert!(regions[0].char_conf.is_empty());
        assert!(!regions[0].char_conf_aligned);
        assert!(map_system_tesseract_regions(native.regions, (190, 140), 200, 150).is_err());
    }

    #[test]
    fn json_export_retains_original_word_evidence_but_rejects_stale_current_ranges() {
        use crate::ocr_word_evidence::{NativeWordEvidence, WordEvidenceEnvelope};
        let root = tempfile::tempdir().unwrap();
        let mut page = export_page(root.path(), "Hi");
        let envelope = WordEvidenceEnvelope { version:1, offset_unit:"utf16".into(), source:"tesseract_tsv".into(), words:vec![
            NativeWordEvidence { text:"Hi".into(), start:0, end:2, x:1.0, y:1.0, w:2.0, h:1.0,
                confidence:None, confidence_basis:"recognition".into(), alternatives:None },
        ] };
        page.regions[0].original_text = Some("Hi".into());
        page.regions[0].original_word_evidence = Some(envelope.clone());
        page.regions[0].word_evidence = Some(envelope.clone());
        validate_export_pages(std::slice::from_ref(&page)).unwrap();
        page.regions[0].original_text = None;
        page.regions[0].original_word_evidence = None;
        assert!(validate_export_pages(std::slice::from_ref(&page)).unwrap_err().contains("requires its original text"));
        page.regions[0].original_text = Some("Hi".into());
        page.regions[0].original_word_evidence = Some(envelope.clone());
        page.regions[0].text = "Hello".into();
        assert!(validate_export_pages(std::slice::from_ref(&page)).unwrap_err().contains("Edited text"));
        page.regions[0].word_evidence = None;
        validate_export_pages(std::slice::from_ref(&page)).unwrap();
        let json: Value = serde_json::from_slice(&export_bytes(&[page], "json", false).unwrap()).unwrap();
        assert_eq!(json["pages"][0]["regions"][0]["original_word_evidence"], serde_json::to_value(envelope).unwrap());
        assert!(json["pages"][0]["regions"][0].get("word_evidence").is_none());
    }

    #[test]
    fn manual_passage_separators_and_original_sources_survive_export() {
        let root = tempfile::tempdir().unwrap();
        let mut page = export_page(root.path(), "Hel");
        let source: PassageSourceComponent = serde_json::from_value(json!({
            "version":1,"id":"source-a","sourceRegionId":"original-a","pageId":"page-a",
            "imagePath":page.image_path,"width":8,"height":4,
            "boundsAtCapture":{"x":1,"y":1,"w":6,"h":2},"kind":"text",
            "originalText":"Hell0","textAtCapture":"Hello","confidence":null,"confidenceBasis":"unknown",
            "char_conf":[],"charConfidenceUnit":"unknown","charConfidenceAligned":false,
            "wordEvidence":{"version":1,"offset_unit":"utf16","source":"fixture","words":[
                {"text":"Hell0","start":0,"end":5,"x":1,"y":1,"w":6,"h":2,"confidence":0.42,"confidence_basis":"recognition"}
            ]}
        })).unwrap();
        page.regions[0].source_components = Some(vec![source.clone()]);
        page.regions[0].shared_source_groups = Some(vec!["shared-a".into()]);
        let mut second = export_page(root.path(), "lo").regions.remove(0);
        second.source_components = Some(vec![source]);
        second.shared_source_groups = Some(vec!["shared-a".into()]);
        second.separator_before = Some(String::new());
        page.regions.push(second);
        validate_export_pages(std::slice::from_ref(&page)).unwrap();
        for separator in ["", " ", "\n", "\n\n"] {
            page.regions[1].separator_before = Some(separator.into());
            let expected = format!("Hel{separator}lo");
            assert_eq!(export_bytes(std::slice::from_ref(&page), "text", false).unwrap(), expected.as_bytes());
            let html = String::from_utf8(export_bytes(std::slice::from_ref(&page), "html", false).unwrap()).unwrap();
            let document = scraper::Html::parse_document(&html);
            let paragraphs = document.select(&scraper::Selector::parse("section > p").unwrap()).collect::<Vec<_>>();
            assert_eq!(paragraphs.len(), 1, "a passage separator must not create another HTML paragraph");
            assert_eq!(paragraphs[0].text().collect::<String>(), expected);
            assert_eq!(paragraphs[0].value().attr("dir"), Some("auto"));
        }
        page.regions[1].separator_before = Some(String::new());
        let json: Value = serde_json::from_slice(&export_bytes(std::slice::from_ref(&page), "json", false).unwrap()).unwrap();
        assert_eq!(json["pages"][0]["regions"][1]["separator_before"], "");
        assert_eq!(json["pages"][0]["regions"][0]["source_components"][0]["originalText"], "Hell0");
        assert_eq!(json["pages"][0]["regions"][0]["source_components"][0]["textAtCapture"], "Hello");
        assert_eq!(json["pages"][0]["regions"][0]["source_components"][0]["wordEvidence"]["words"][0]["text"], "Hell0");
        for format in ["pdf","hocr","alto","XML"] {
            assert!(export_bytes(std::slice::from_ref(&page), format, false).unwrap_err().contains("overlapping source areas"));
        }
        page.regions[0].w = 3.0;
        page.regions[1].x = 4.0;
        page.regions[1].w = 3.0;
        assert!(!has_shared_source_overlap(&page));
        page.regions[1].separator_before = Some("malformed".into());
        assert!(validate_export_pages(std::slice::from_ref(&page)).unwrap_err().contains("separator"));
    }

    #[test]
    fn logical_structure_and_figure_sources_survive_json_without_changing_plain_text() {
        let root=tempfile::tempdir().unwrap();
        let mut page=export_page(root.path(),"Heading");
        page.regions[0].id=Some("text-a".into()); page.regions[0].kind=Some("title".into());
        let mut figure=export_page(root.path(),"Not transcription").regions.remove(0);
        figure.id=Some("figure-a".into()); figure.kind=Some("picture".into());
        page.figure_regions=Some(vec![figure]);
        page.structure=Some(serde_json::from_value(json!({
            "version":1,"sections":[{"id":"section-a","columns":1}],"blocks":[
                {"id":"heading-a","sourceRegionId":"text-a","sectionId":"section-a","column":1,
                    "role":"heading","headingLevel":1,"alignment":"center","breakBefore":"none","runs":[{"start":0,"end":7,"bold":true}]},
                {"id":"image-a","sourceRegionId":"figure-a","sectionId":"section-a","column":1,
                    "role":"figure","alignment":"start","breakBefore":"none","runs":[],"altText":"A diagram","decorative":false}
            ]
        })).unwrap());
        validate_export_pages(std::slice::from_ref(&page)).unwrap();
        assert_eq!(export_bytes(std::slice::from_ref(&page),"text",false).unwrap(),b"Heading");
        let json:Value=serde_json::from_slice(&export_bytes(std::slice::from_ref(&page),"json",false).unwrap()).unwrap();
        assert_eq!(json["pages"][0]["structure"]["blocks"][0]["runs"][0]["bold"],true);
        assert_eq!(json["pages"][0]["figure_regions"][0]["id"],"figure-a");
        assert_eq!(json["pages"][0]["regions"].as_array().unwrap().len(),1);
        page.figure_regions=None;
        assert!(validate_export_pages(&[page]).unwrap_err().contains("exactly one block"));
    }

    #[test]
    fn transparent_scans_use_the_same_white_paper_for_preview_and_recognition() {
        let root = tempfile::tempdir().unwrap();
        let input = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(3, 1, |x, _| {
            image::Rgba([0, 0, 0, [0, 255, 128][x as usize]])
        }));
        let saved = persist_image(root.path(), &input).unwrap();
        let output = image::open(saved.image_path).unwrap();
        assert!(!output.color().has_alpha());
        let pixels = output.to_rgb8();
        assert_eq!(pixels.get_pixel(0, 0).0, [255, 255, 255]);
        assert_eq!(pixels.get_pixel(1, 0).0, [0, 0, 0]);
        assert_eq!(pixels.get_pixel(2, 0).0, [127, 127, 127]);
        assert_eq!(input.to_rgba8().get_pixel(0, 0).0, [0, 0, 0, 0]);
    }

    #[test]
    fn rotation_keeps_source_bytes_and_changes_snapshot_dimensions() {
        let root = tempfile::tempdir().unwrap();
        let source = persist_image(root.path(), &image()).unwrap();
        let before = fs::read(&source.image_path).unwrap();
        let rotated = transform_page(root.path(), Path::new(&source.image_path), "rotate", Some(90), None, None, Some((200.0, 100.0))).unwrap();
        assert_eq!((rotated.pages[0].width, rotated.pages[0].height), (4, 8));
        assert_eq!(fs::read(&source.image_path).unwrap(), before);
        assert_ne!(source.image_path, rotated.pages[0].image_path);
        assert_eq!(rotated.pages[0].page_width_points, Some(100.0));
        assert_eq!(rotated.pages[0].page_height_points, Some(200.0));
        assert_eq!(image::open(&rotated.pages[0].image_path).unwrap().to_rgb8(), image().rotate90().to_rgb8());
    }

    #[test]
    fn split_preserves_both_halves_and_the_last_pixel() {
        let root = tempfile::tempdir().unwrap();
        let source = persist_image(root.path(), &image()).unwrap();
        let split = transform_page(root.path(), Path::new(&source.image_path), "split", None, None, Some(3), Some((800.0, 400.0))).unwrap();
        assert_eq!(split.pages.len(), 2);
        assert_eq!((split.pages[0].width, split.pages[1].width), (3, 5));
        assert_eq!(split.pages[0].page_width_points, Some(300.0));
        assert_eq!(split.pages[1].page_width_points, Some(500.0));
        let right = image::open(&split.pages[1].image_path).unwrap().to_rgb8();
        assert_eq!(right.get_pixel(4, 3), image().to_rgb8().get_pixel(7, 3));
        assert!(transform_page(root.path(), Path::new(&source.image_path), "split", None, None, Some(8), None).is_err());
    }

    #[test]
    fn crop_bounds_reject_invalid_values_and_include_fractional_edges() {
        for rect in [
            PixelRect{x:-1.0,y:0.0,w:2.0,h:2.0}, PixelRect{x:0.0,y:0.0,w:0.0,h:2.0},
            PixelRect{x:7.0,y:0.0,w:2.0,h:2.0}, PixelRect{x:f64::NAN,y:0.0,w:2.0,h:2.0},
        ] { assert!(checked_rect(rect, 8, 4).is_err()); }
        assert_eq!(checked_rect(PixelRect{x:1.5,y:0.5,w:2.5,h:2.0}, 8, 4).unwrap(), (1, 0, 3, 3));
    }

    #[test]
    fn crop_scales_paper_size_but_repair_preserves_sampling_independence() {
        let root = tempfile::tempdir().unwrap();
        let source = persist_image(root.path(), &image()).unwrap();
        let crop = transform_page(root.path(), Path::new(&source.image_path), "crop", None,
            Some(PixelRect{x:1.0,y:1.0,w:3.0,h:2.0}), None, Some((80.0,40.0))).unwrap();
        assert_eq!(crop.pages[0].page_width_points, Some(30.0));
        assert_eq!(crop.pages[0].page_height_points, Some(20.0));
        let mut config = crate::extractors::OcrPipelineConfig::default();
        config.cleanup_enabled = false;
        let prepared = prepare_page(root.path(), Path::new(&source.image_path), &config, Some((80.0,40.0))).unwrap();
        assert_eq!(prepared.page.page_width_points, Some(80.0));
        assert_eq!(prepared.page.page_height_points, Some(40.0));
        assert!(checked_physical_size(Some(80.0), None).is_err());
        assert!(checked_physical_size(Some(f64::NAN), Some(40.0)).is_err());
        assert!(checked_physical_size(Some(20_000.0), Some(40.0)).is_err());
    }

    #[test]
    fn inactive_stage_crop_and_split_preserve_review_pixels_and_paper_size() {
        let root = tempfile::tempdir().unwrap();
        let input = image();
        let source = persist_image(root.path(), &input).unwrap();
        let config = serde_json::from_value(json!({
            "enabled": true, "cleanup_enabled": true,
            "stages": [{"engine":"tesseract", "rec_model":"custom-language.gguf",
                "cleanup":{"enabled":false,"auto_crop":true,"page_split":true}}],
        })).unwrap();
        let prepared = prepare_page(root.path(), Path::new(&source.image_path), &config, Some((80.0,40.0))).unwrap();
        assert_eq!((prepared.page.width, prepared.page.height), (input.width(), input.height()));
        assert_eq!(decode_page(Path::new(&prepared.page.image_path)).unwrap().to_rgb8(), input.to_rgb8());
        assert_eq!(prepared.page.page_width_points, Some(80.0));
        assert_eq!(prepared.page.page_height_points, Some(40.0));
        assert!(prepared.warnings.is_empty(), "inactive crop/split options must not run or request review");
    }

    #[test]
    fn save_rejects_stale_writers_without_losing_newer_edits() {
        let root = tempfile::tempdir().unwrap();
        let original = project(root.path());
        let first = save_project(root.path(), original.clone(), 0).unwrap();
        assert_eq!(first.revision, 1);
        let mut newer = original.clone(); newer["name"] = json!("Corrected invoice");
        assert_eq!(save_project(root.path(), newer, 1).unwrap().revision, 2);
        assert!(save_project(root.path(), original, 1).unwrap_err().contains("current 2"));
        let loaded = load_project(root.path(), "project-test").unwrap().unwrap();
        assert_eq!(loaded.project["name"], "Corrected invoice");
        assert_eq!(loaded.revision, 2);
    }

    #[test]
    fn recovery_preserves_a_valid_previous_revision_after_corruption() {
        let root = tempfile::tempdir().unwrap();
        let original = project(root.path());
        save_project(root.path(), original.clone(), 0).unwrap();
        let second = save_project(root.path(), original.clone(), 1).unwrap();
        fs::write(&second.path, b"{interrupted").unwrap();
        let recovered = load_project(root.path(), "project-test").unwrap().unwrap();
        assert_eq!(recovered.revision, 1);
        assert!(!recovered.warnings.is_empty());
        save_project(root.path(), original, recovered.revision).unwrap();
        assert_eq!(load_project(root.path(), "project-test").unwrap().unwrap().revision, 2);
    }

    #[test]
    fn invalid_projects_cannot_replace_saved_work() {
        let root = tempfile::tempdir().unwrap();
        let original = project(root.path());
        let first = save_project(root.path(), original.clone(), 0).unwrap();
        let before = fs::read(&first.path).unwrap();
        let mut future = original.clone(); future["version"] = json!(999);
        assert!(save_project(root.path(), future, 1).is_err());
        let mut traversal = original.clone(); traversal["id"] = json!("../../outside");
        assert!(save_project(root.path(), traversal, 0).is_err());
        let mut malformed = original; malformed["pages"][0]["imagePath"] = json!(false);
        assert!(save_project(root.path(), malformed, 1).is_err());
        assert_eq!(fs::read(&first.path).unwrap(), before);
    }

    #[test]
    fn missing_assets_do_not_prevent_saving_reviewed_text() {
        let root = tempfile::tempdir().unwrap();
        let mut value = project(root.path());
        let path = value["pages"][0]["imagePath"].as_str().unwrap().to_string();
        fs::remove_file(path).unwrap();
        value["pages"][0]["regions"] = json!([{"text":"Recover this correction"}]);
        let saved = save_project(root.path(), value, 0).unwrap();
        assert!(!saved.warnings.is_empty());
        assert_eq!(load_project(root.path(), "project-test").unwrap().unwrap().project["pages"][0]["regions"][0]["text"], "Recover this correction");
    }

    #[test]
    fn export_rejects_stale_page_geometry_and_outside_regions() {
        let root = tempfile::tempdir().unwrap();
        let mut page = export_page(root.path(), "reviewed");
        page.width = 4;
        assert!(validate_export_pages(&[page]).is_err());
        let mut page = export_page(root.path(), "reviewed"); page.regions[0].x = 7.0;
        assert!(validate_export_pages(&[page]).is_err());
        assert!(validate_export_pages(&[]).is_err());
    }

    #[test]
    fn export_capabilities_never_offer_an_unverified_pdf_renderer() {
        let capabilities = ocr_workspace_capabilities();
        let formats = capabilities["formats"].as_array().unwrap();
        assert_eq!(formats.iter().any(|format| format == "pdf"), reviewed_pdf_available());
        assert_eq!(formats.iter().any(|format| format == "docx"), cfg!(feature = "ocr-office"));
        assert_eq!(capabilities["structured_docx"], json!(cfg!(feature = "ocr-office")));
        assert_eq!(formats.iter().any(|format| format == "xlsx"), cfg!(feature = "ocr-office"));
        assert_eq!(capabilities["structured_xlsx"], json!(cfg!(feature = "ocr-office")));
        if !reviewed_pdf_available() { assert!(capabilities["pdf_export_reason"].as_str().is_some()); }
        let root = tempfile::tempdir().unwrap();
        let pages = vec![export_page(root.path(), "reviewed")];
        let archival = crate::extractors::ocr_render::reviewed_archival_pdf_available();
        assert_eq!(formats.iter().any(|format| format == "pdfa"), archival);
        assert_eq!(capabilities["archival_pdf_export"], archival);
        if archival {
            let bytes = export_bytes(&pages, "pdf", true).unwrap();
            assert_eq!(lopdf::Document::load_mem(&bytes).unwrap().get_pages().len(), 1);
        } else {
            assert!(capabilities["pdfa_export_reason"].as_str().is_some());
            assert!(export_bytes(&pages, "pdf", true).unwrap_err().contains("PDF/A"));
        }
        assert!(export_bytes(&pages, "text", true).unwrap_err().contains("only to PDF"));
        if !reviewed_pdf_available() {
            assert!(export_bytes(&pages, "pdf", false).unwrap_err().contains("matching updated CrispEmbed renderer"));
        }
        assert_eq!(export_bytes(&pages, "text", false).unwrap(), b"reviewed");
    }

    #[cfg(feature = "ocr-office")]
    fn office_part(bytes: &[u8], name: &str) -> Vec<u8> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut part = archive.by_name(name).unwrap();
        let mut result = vec![];
        part.read_to_end(&mut result).unwrap();
        result
    }

    #[cfg(feature = "ocr-office")]
    #[test]
    fn docx_adapter_retains_literal_text_and_blank_source_pages() {
        let root = tempfile::tempdir().unwrap();
        let mut first = export_page(root.path(), "  Hello\tworld\r\n\n😀 & < >  ");
        first.page_width_points = Some(612.0); first.page_height_points = Some(792.0);
        let mut blank = export_page(root.path(), ""); blank.regions.clear();
        blank.page_width_points = Some(612.0); blank.page_height_points = Some(792.0);
        let pages = vec![first, blank];
        validate_export_pages(&pages).unwrap();
        let output = export_docx(&pages, &crate::ocr_office::OfficeOptions::default()).unwrap();
        let xml = String::from_utf8(office_part(&output.bytes, "word/document.xml")).unwrap();
        assert!(xml.contains("  Hello")); assert!(xml.contains("world"));
        assert!(xml.contains("😀 &amp; &lt; &gt;  "));
        assert!(xml.contains("<w:tab")); assert!(xml.contains("<w:br"));
        assert!(output.warnings.iter().any(|warning| warning.contains("CR") || warning.contains("line break")));
        // Distinct page sections retain the explicit trailing blank source page.
        assert!(xml.matches("<w:sectPr").count() >= 2 || xml.contains("w:type=\"page\""));
    }

    #[cfg(feature = "ocr-office")]
    #[test]
    fn docx_adapter_never_selects_or_replaces_by_generated_legacy_identity() {
        use crate::ocr_office::{OfficeObjectSelection, OfficeOptions};
        let root = tempfile::tempdir().unwrap();
        let mut page = export_page(root.path(), "This source text must not be replaced by its position.");
        page.regions[0].kind = Some("table".into());
        let mut options = OfficeOptions { version: 1, objects: vec![OfficeObjectSelection {
            page_id: "legacy-page-1".into(), object_id: "table-object".into(),
            placement: "append".into(), region_id: None,
        }] };
        assert!(export_docx(std::slice::from_ref(&page), &options).unwrap_err().contains("supplied stable page identity"));
        page.id = Some("real-page".into());
        options.objects[0].page_id = "real-page".into();
        options.objects[0].placement = "replace-region".into();
        options.objects[0].region_id = Some("legacy-region-1".into());
        assert!(export_docx(std::slice::from_ref(&page), &options).unwrap_err().contains("supplied stable passage identity"));
        page.regions[0].id = Some("real-passage".into());
        options.objects[0].region_id = Some("real-passage".into());
        // Real identities advance to object validation; they do not authorize a
        // nonexistent extraction merely because the page/region are known.
        assert!(export_docx(&[page], &options).unwrap_err().contains("selected extraction data is missing"));
    }

    #[cfg(feature = "ocr-office")]
    #[test]
    fn docx_adapter_uses_current_picture_pixels_and_explicit_corrected_table() {
        use crate::ocr_office::{OfficeObjectSelection, OfficeOptions};
        let root = tempfile::tempdir().unwrap();
        let mut page = export_page(root.path(), "OCR table passage omitted only by explicit replacement");
        page.id = Some("page".into()); page.page_width_points = Some(612.0); page.page_height_points = Some(792.0);
        page.regions[0].id = Some("table-region".into()); page.regions[0].kind = Some("table".into());
        let mut figure = export_page(root.path(), "").regions.remove(0);
        figure.id = Some("picture-region".into()); figure.kind = Some("picture".into());
        figure.x = 2.0; figure.y = 1.0; figure.w = 3.0; figure.h = 2.0;
        page.figure_regions = Some(vec![figure]);
        page.structure = Some(serde_json::from_value(json!({"version":1,"sections":[{"id":"section","columns":1}],"blocks":[
            {"id":"table-block","sourceRegionId":"table-region","sectionId":"section","column":1,"role":"table","alignment":"start","breakBefore":"none","runs":[]},
            {"id":"picture-block","sourceRegionId":"picture-region","sectionId":"section","column":1,"role":"figure","alignment":"start","breakBefore":"none","runs":[],"altText":"Current measured crop"}
        ]})).unwrap());
        let table = |text: &str| json!({"version":1,"rows":1,"columns":1,"cells":[{"id":"cell","row":0,"column":0,"rowSpan":1,"columnSpan":1,"text":text}]});
        page.provenance = Some(json!({"extractionObjects":{"version":1,"objects":[{
            "version":1,"id":"table-object","kind":"table","name":"Corrected table","createdAt":"2026-11-09T00:00:00.000Z","updatedAt":"2026-11-09T00:00:00.000Z",
            "excluded":false,"review":"corrected","captures":[{"id":"capture","pageId":"old-page","documentId":"document","width":8,"height":4,
                "imagePath":"/never-read/untrusted-provenance.png","regionId":null,"bounds":null,"method":"pixels","text":null}],
            "originalOutputKind":"model","originalOutput":"<table><td>old</td></table>","originalData":table("old"),"currentData":table("0012 =SUM(A1) & exact")
        }]}}));
        let pages = vec![page]; validate_export_pages(&pages).unwrap();
        let options = OfficeOptions { version:1, objects:vec![OfficeObjectSelection { page_id:"page".into(),object_id:"table-object".into(),placement:"replace-region".into(),region_id:Some("table-region".into()) }] };
        let output = export_docx(&pages, &options).unwrap();
        let xml = String::from_utf8(office_part(&output.bytes, "word/document.xml")).unwrap();
        assert!(xml.contains("0012 =SUM(A1) &amp; exact"));
        assert!(!xml.contains("OCR table passage omitted")); assert!(xml.contains("<w:tbl"));
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&output.bytes)).unwrap();
        let media: Vec<_> = archive.file_names().filter(|name| name.starts_with("word/media/") && !name.ends_with('/')).map(str::to_owned).collect();
        assert_eq!(media.len(), 1);
        let mut bytes = vec![]; archive.by_name(&media[0]).unwrap().read_to_end(&mut bytes).unwrap();
        let actual = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(actual, image().crop_imm(2, 1, 3, 2).to_rgba8());
        let unselected = export_docx(&pages, &OfficeOptions::default()).unwrap();
        let plain = String::from_utf8(office_part(&unselected.bytes, "word/document.xml")).unwrap();
        assert!(plain.contains("OCR table passage omitted"));
        assert!(!plain.contains("0012 =SUM(A1)"));
    }

    #[cfg(feature = "ocr-office")]
    #[test]
    fn xlsx_adapter_exports_saved_tables_without_current_image_or_ocr_dependencies() {
        use crate::ocr_office::{WorkbookOptions, WorkbookSheetSelection};
        let root = tempfile::tempdir().unwrap();
        let mut page = export_page(root.path(), "UNSELECTED_OCR_PASSAGE_MUST_NOT_EXPORT");
        page.id = Some("page".into());
        // The current image/geometry are not workbook input. Historical capture
        // metadata is validated, but its path is never read either.
        page.image_path = root.path().join("missing-current-image.png").to_string_lossy().into_owned();
        page.width = 0; page.height = 0; page.page_width_points = Some(9_000.0);
        let table = |text: &str| json!({"version":1,"rows":1,"columns":1,"cells":[{"id":"cell","row":0,"column":0,"rowSpan":1,"columnSpan":1,"text":text,"header":true}]});
        let object = |id: &str, current: Value| json!({
            "version":1,"id":id,"kind":"table","name":id,"createdAt":"2026-11-09T00:00:00.000Z","updatedAt":"2026-11-09T00:00:00.000Z",
            "excluded":false,"review":"corrected","captures":[{"id":"capture","pageId":"older-page","documentId":"document","width":8,"height":4,
                "imagePath":"/never-read/missing-source.png","regionId":null,"bounds":null,"method":"pixels","text":null}],
            "originalOutputKind":"model","originalOutput":"ORIGINAL_MODEL_OUTPUT_MUST_NOT_EXPORT","originalData":table("ORIGINAL_CELL_MUST_NOT_EXPORT"),"currentData":current
        });
        page.provenance = Some(json!({"extractionObjects":{"version":1,"objects":[
            object("literal",table(" 0012 =2+2 & < > ")),object("blank",json!({"version":1,"rows":0,"columns":0,"cells":[]}))
        ]}}));
        let options=WorkbookOptions{version:1,sheets:vec![
            WorkbookSheetSelection{page_id:"page".into(),object_id:"blank".into(),sheet_name:"Empty capture".into()},
            WorkbookSheetSelection{page_id:"page".into(),object_id:"literal".into(),sheet_name:"Measurements".into()},
        ]};
        let output=export_xlsx(&[page],&options).unwrap();
        let workbook=String::from_utf8(office_part(&output.bytes,"xl/workbook.xml")).unwrap();
        assert!(workbook.find("Empty capture").unwrap()<workbook.find("Measurements").unwrap());
        let blank=String::from_utf8(office_part(&output.bytes,"xl/worksheets/sheet1.xml")).unwrap();
        let literal=String::from_utf8(office_part(&output.bytes,"xl/worksheets/sheet2.xml")).unwrap();
        assert!(!blank.contains("<c "));
        assert!(literal.contains(" 0012 =2+2 &amp; &lt; &gt; "));
        assert!(literal.contains("t=\"inlineStr\""));
        assert!(!literal.contains("<f>") && !literal.contains("MUST_NOT_EXPORT"));
        assert!(!output.warnings.is_empty());
    }

    #[cfg(feature = "ocr-office")]
    #[test]
    fn xlsx_adapter_requires_real_selected_page_identity_and_saved_table_data() {
        use crate::ocr_office::{WorkbookOptions, WorkbookSheetSelection};
        let root=tempfile::tempdir().unwrap();
        let mut page=export_page(root.path(),"Source text");
        let mut options=WorkbookOptions{version:1,sheets:vec![WorkbookSheetSelection{
            page_id:"legacy-page-1".into(),object_id:"table".into(),sheet_name:"Values".into(),
        }]};
        assert!(export_xlsx(std::slice::from_ref(&page),&options).unwrap_err().contains("supplied stable page identities"));
        page.id=Some("real-page".into());
        assert!(export_xlsx(std::slice::from_ref(&page),&options).unwrap_err().contains("scope"));
        options.sheets[0].page_id="real-page".into();
        assert!(export_xlsx(std::slice::from_ref(&page),&options).unwrap_err().contains("table data is missing"));
        assert!(export_xlsx(&[page],&WorkbookOptions::default()).unwrap_err().contains("1–512"));
    }

    #[test]
    fn blank_reviewed_pages_preserve_document_page_positions() {
        let root = tempfile::tempdir().unwrap();
        let mut blank = export_page(root.path(), ""); blank.regions.clear();
        let pages = vec![export_page(root.path(), "first"), blank, export_page(root.path(), "third")];
        validate_export_pages(&pages).unwrap();
        assert_eq!(String::from_utf8(export_bytes(&pages, "text", false).unwrap()).unwrap(), "first\u{000c}\u{000c}third");
        let output: Value = serde_json::from_slice(&export_bytes(&pages, "json", false).unwrap()).unwrap();
        assert_eq!(output["pages"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn json_export_preserves_unknown_confidence_and_review_evidence() {
        let root = tempfile::tempdir().unwrap();
        let mut page = export_page(root.path(), "corrected amount");
        page.id = Some("page-a".into());
        page.source_uri = Some("/original/invoice.pdf".into());
        page.source_page = Some(2);
        page.approved = Some(true);
        page.provenance = Some(json!({"engine":"tesseract", "confidence_calibrated":false}));
        let region = &mut page.regions[0];
        region.confidence = None;
        region.id = Some("region-a".into());
        region.original_text = Some("original amount".into());
        region.confidence_basis = Some("unknown".into());
        region.char_conf_unit = Some("token".into());
        region.char_conf_aligned = Some(false);
        region.original_char_conf = Some(vec![0.1, 0.9]);
        region.original_char_conf_aligned = Some(false);
        region.review = Some("verified".into());
        let pages = vec![page, export_page(root.path(), "known zero")];
        validate_export_pages(&pages).unwrap();
        let output: Value = serde_json::from_slice(&export_bytes(&pages, "json", false).unwrap()).unwrap();
        let first = &output["pages"][0];
        assert_eq!(first["id"], "page-a");
        assert_eq!(first["source_page"], 2);
        assert_eq!(first["provenance"]["engine"], "tesseract");
        assert!(first["regions"][0]["confidence"].is_null());
        assert_eq!(first["regions"][0]["original_text"], "original amount");
        assert_eq!(first["regions"][0]["confidence_basis"], "unknown");
        assert_eq!(first["regions"][0]["char_conf_aligned"], false);
        assert_eq!(first["regions"][0]["original_char_conf"].as_array().unwrap().len(), 2);
        assert_eq!(first["regions"][0]["review"], "verified");
        assert_eq!(output["pages"][1]["regions"][0]["confidence"], 0.0);
    }

    #[test]
    fn structured_confidence_requires_unchanged_transcription_evidence() {
        let root = tempfile::tempdir().unwrap();
        let mut page = export_page(root.path(), "original");
        let region = &mut page.regions[0];
        let unknown = if cfg!(feature = "ocr-reviewed-pdf") { -1.0 } else { 0.0 };
        region.confidence = Some(0.99);
        region.original_text = Some("original".into());
        region.confidence_basis = Some("detection".into());
        assert_eq!(transcription_confidence(region), unknown);
        region.confidence_basis = Some("recognition".into());
        assert_eq!(transcription_confidence(region), 0.99);
        region.text = "corrected".into();
        assert_eq!(transcription_confidence(region), unknown);
        region.text = "original".into();
        region.confidence = Some(0.0);
        assert_eq!(transcription_confidence(region), 0.0);
        region.confidence = None;
        assert_eq!(transcription_confidence(region), unknown);
    }

    #[test]
    fn html_export_escapes_recognized_markup() {
        let root = tempfile::tempdir().unwrap();
        let pages = vec![export_page(root.path(), "<script>alert('x')</script>&")];
        let html = String::from_utf8(export_bytes(&pages, "html", false).unwrap()).unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("<p dir=\"auto\">"));
        assert!(html.contains("name=\"viewport\""));
    }

    #[test]
    fn text_export_keeps_reviewed_newlines_spaces_and_logical_unicode() {
        let root = tempfile::tempdir().unwrap();
        let text = "  مَرْحَبًا ABC 123\t\r\n\nשָׁלוֹם e\u{301}  \n";
        let pages = vec![export_page(root.path(), text)];
        for format in ["text", "txt", "plain"] {
            assert_eq!(export_bytes(&pages, format, false).unwrap(), text.as_bytes());
        }
        if !crate::extractors::ocr_render::reviewed_bidi_pdf_available() {
            assert_eq!(validate_export_text(&pages, "pdf").unwrap_err(), crate::ocr_text_export::PDF_BIDI_UNAVAILABLE);
        }
        for format in ["text", "html", "docx"] { assert!(validate_export_text(&pages, format).is_ok()); }
    }

    #[cfg(feature = "ocr-reviewed-pdf")]
    #[test]
    fn grouped_paragraph_survives_workspace_spatial_and_text_exports() {
        let root = tempfile::tempdir().unwrap();
        let image = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(320, 200, image::Rgb([255, 255, 255])));
        let asset = persist_image(root.path(), &image).unwrap();
        let grouped = group_lines_into_paragraphs(vec![
            word("First résumé", 20.0, 20.0, 280.0, 20.0, 0.9),
            word("Second 東京", 20.0, 44.0, 280.0, 20.0, 0.8),
        ]);
        assert_eq!(grouped.len(), 1);
        let source = &grouped[0];
        let mut page = export_page(root.path(), &source.text);
        page.image_path = asset.image_path;
        page.width = 320;
        page.height = 200;
        page.page_width_points = Some(320.0);
        page.page_height_points = Some(200.0);
        let region = &mut page.regions[0];
        (region.x, region.y, region.w, region.h) = (source.x, source.y, source.w, source.h);
        region.original_text = Some(source.text.clone());
        region.confidence = source.confidence;
        region.confidence_basis = Some(source.confidence_basis.clone());
        region.char_conf = source.char_conf.clone();
        region.char_conf_unit = Some(source.char_conf_unit.clone());
        region.char_conf_aligned = Some(source.char_conf_aligned);
        let pages = vec![page];
        assert_eq!(export_bytes(&pages, "text", false).unwrap(), source.text.as_bytes());
        let mapped = reviewed_render_pages(&pages);
        assert_eq!(mapped[0].regions[0].text, source.text);
        assert_eq!((mapped[0].regions[0].x, mapped[0].regions[0].y, mapped[0].regions[0].w, mapped[0].regions[0].h),
            (20.0, 20.0, 280.0, 44.0));
        for format in ["hocr", "alto"] {
            let xml = String::from_utf8(export_bytes(&pages, format, false).unwrap()).unwrap();
            assert!(xml.contains("First résumé&#xA;Second 東京"), "{format} must retain the complete multiline region text");
        }
        let pdf = export_bytes(&pages, "pdf", false).unwrap();
        let document = lopdf::Document::load_mem(&pdf).unwrap();
        let extracted = document.extract_text(&[1]).unwrap();
        assert!(extracted.contains("First résumé"));
        assert!(extracted.contains("Second 東京"));
        let page_id = document.get_pages()[&1];
        let content = lopdf::content::Content::decode(&document.get_page_content(page_id).unwrap()).unwrap();
        let baselines: Vec<_> = content.operations.iter().filter(|op| op.operator == "Tm")
            .map(|op| op.operands[5].as_float().unwrap()).collect();
        assert_eq!(baselines.len(), 2, "the paired renderer must lay out both explicit lines");
        assert!(baselines[0] > baselines[1]);
    }

    #[test]
    fn export_never_overwrites_sources_or_internal_assets() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("workspace")).unwrap();
        let workspace = root.path().join("workspace");
        let pages = vec![export_page(&workspace, "text")];
        let source = root.path().join("source.pdf"); fs::write(&source, b"original").unwrap();
        assert!(output_path(source.to_str().unwrap(), &pages, &[source.to_string_lossy().into_owned()], &workspace).is_err());
        assert!(output_path(&pages[0].image_path, &pages, &[], &workspace).is_err());
        assert!(output_path("relative.txt", &pages, &[], &workspace).is_err());
        assert!(output_path(root.path().join("reviewed.txt").to_str().unwrap(), &pages, &[], &workspace).is_ok());
    }

    #[cfg(feature = "ocr-reviewed-pdf")]
    #[test]
    fn streamed_pdf_is_atomic_and_a_native_limit_failure_preserves_the_previous_file() {
        let root = tempfile::tempdir().unwrap();
        let pages = vec![export_page(root.path(), "Reviewed résumé 東京")];
        let path = root.path().join("reviewed.pdf");
        assert!(crate::extractors::ocr_render::reviewed_streaming_pdf_available());
        let count = write_export(&path, &pages, "pdf", false, false, MAX_STREAMED_PDF_BYTES, None,
            &ExportControl::default(), |_, _| {}).unwrap();
        let previous = fs::read(&path).unwrap();
        assert_eq!(previous.len(), count);
        assert!(previous.len() > 4096, "fixture must exceed the native writer's minimum valid cap");
        let document = lopdf::Document::load_mem(&previous).unwrap();
        assert_eq!(document.get_pages().len(), 1);
        assert!(document.extract_text(&[1]).unwrap().contains("Reviewed résumé 東京"));
        let count_before = fs::read_dir(root.path()).unwrap().count();
        let error = write_export(&path, &pages, "pdf", false, true, 4096, None,
            &ExportControl::default(), |_, _| {}).unwrap_err();
        assert!(!error.is_empty());
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), count_before);
    }

    #[cfg(feature = "ocr-reviewed-pdf")]
    #[test]
    fn cancelling_the_native_pdf_writer_after_output_starts_preserves_the_previous_export() {
        let root = tempfile::tempdir().unwrap();
        let pages = vec![export_page(root.path(), "Reviewed résumé 東京")];
        let render_pages = reviewed_render_pages(&pages);
        let path = root.path().join("reviewed.pdf");
        fs::write(&path, b"previous complete export").unwrap();
        let count_before = fs::read_dir(root.path()).unwrap().count();
        let job = ExportJob::begin(None).unwrap();
        let mut first_bytes = 0;
        let result = atomic_write_with(&path, true, |file| {
            let mut writer = ControlledWriter::new(file, job.control(), |bytes| {
                first_bytes = bytes;
                assert!(ocr_workspace_cancel_export(job.id().to_owned()).unwrap());
            });
            crate::extractors::ocr_render::render_pdf_to_writer(
                &render_pages, false, MAX_STREAMED_PDF_BYTES, &mut writer,
            ).map_err(|error| format!("{error:#}"))
        });
        assert!(result.unwrap_err().contains(export_control::CANCELLED));
        assert!(first_bytes > 0, "the native writer must have emitted actual PDF bytes before cancellation");
        assert_eq!(fs::read(&path).unwrap(), b"previous complete export");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), count_before);
    }

    #[test]
    fn exif_orientation_is_baked_into_pixels() {
        let img = image();
        let transposed = orient_image(img.clone(), 5).to_rgb8();
        assert_eq!((transposed.width(), transposed.height()), (4, 8));
        assert_eq!(transposed.get_pixel(2, 6), img.to_rgb8().get_pixel(6, 2));
        assert_eq!(orient_image(img.clone(), 6).to_rgb8(), img.rotate90().to_rgb8());
        assert_eq!(orient_image(img.clone(), 1).to_rgb8(), img.to_rgb8());
    }

    #[test]
    fn image_import_preserves_original_and_creates_durable_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("scan.png"); image().save(&source).unwrap();
        let before = fs::read(&source).unwrap();
        let workspace = root.path().join("workspace");
        // Production commands create the workspace before entering the importer.
        // Keep assets absent here so the cold page-cache creation is still tested.
        fs::create_dir(&workspace).unwrap();
        let imported = import_document(&workspace, &source).unwrap();
        assert_eq!(imported.pages.len(), 1);
        assert_eq!(imported.pages[0].page_width_points, Some(8.0 * 72.0 / 300.0));
        assert_eq!(imported.pages[0].page_height_points, Some(4.0 * 72.0 / 300.0));
        assert_eq!(fs::read(&source).unwrap(), before);
        assert_ne!(Path::new(&imported.pages[0].image_path), source);
        fs::remove_file(&source).unwrap();
        assert!(Path::new(&imported.pages[0].image_path).exists());
    }

    #[test]
    fn jpeg_import_preserves_exif_resolution_and_rotated_paper_axes() {
        use exif::{Field, In, Rational, Tag, Value};
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("600-dpi-scan.jpg");
        image::RgbImage::from_pixel(600, 300, image::Rgb([255, 255, 255])).save(&source).unwrap();
        let encoded = fs::read(&source).unwrap();
        let fields = [
            Field { tag: Tag::Orientation, ifd_num: In::PRIMARY, value: Value::Short(vec![6]) },
            Field { tag: Tag::ResolutionUnit, ifd_num: In::PRIMARY, value: Value::Short(vec![2]) },
            Field { tag: Tag::XResolution, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 600, denom: 1 }]) },
            Field { tag: Tag::YResolution, ifd_num: In::PRIMARY, value: Value::Rational(vec![Rational { num: 600, denom: 1 }]) },
        ];
        let mut writer = exif::experimental::Writer::new();
        for field in &fields { writer.push_field(field); }
        let mut tiff = std::io::Cursor::new(Vec::new());
        writer.write(&mut tiff, true).unwrap();
        let mut metadata = b"Exif\0\0".to_vec();
        metadata.extend_from_slice(&tiff.into_inner());
        let mut bytes = encoded[..2].to_vec();
        // A conflicting ordinary JFIF density must not replace explicit EXIF.
        bytes.extend_from_slice(&[0xff, 0xe0, 0, 16]);
        bytes.extend_from_slice(b"JFIF\0\x01\x02\x01\x01\x2c\x01\x2c\0\0");
        bytes.extend_from_slice(&[0xff, 0xe1]);
        bytes.extend_from_slice(&((metadata.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(&metadata);
        bytes.extend_from_slice(&encoded[2..]);
        fs::write(&source, &bytes).unwrap();

        let imported = import_document(root.path(), &source).unwrap();
        assert_eq!(fs::read(&source).unwrap(), bytes, "resolution import must preserve the source");
        let page = &imported.pages[0];
        assert_eq!((page.width, page.height), (300, 600));
        assert_eq!((page.page_width_points, page.page_height_points), (Some(36.0), Some(72.0)));
        assert_eq!(image::image_dimensions(&page.image_path).unwrap(), (300, 600));
    }

    #[test]
    fn preparation_selects_the_requested_recipe_without_requiring_recognition_weights() {
        let mut config = crate::extractors::OcrPipelineConfig::default();
        config.engine = Some("qwen2vl".into());
        config.stages = vec![
            serde_json::from_value(json!({"engine":"surya","cleanup":{"enabled":true,"denoise":true}})).unwrap(),
            serde_json::from_value(json!({"engine":"qwen2vl","cleanup":{"enabled":false,"denoise":false},"vlm_prompt":"Keep columns"})).unwrap(),
        ];
        let active = active_preparation_config(config.clone());
        assert_eq!(active.stages.len(), 1);
        assert_eq!(active.stages[0].engine, "qwen2vl");
        assert!(!active.stages[0].cleanup.enabled);
        assert!(!active.stages[0].cleanup.denoise);
        assert_eq!(active.stages[0].vlm_prompt, "Keep columns");
        config.engine = Some("auto".into());
        assert_eq!(active_preparation_config(config.clone()).stages.len(), 2);
        config.engine = Some("system_tesseract".into());
        assert!(active_preparation_config(config).stages.is_empty());
    }

    #[test]
    fn capability_catalog_offers_available_legacy_engines_and_custom_embedded_choices() {
        assert!(offered_engines(false, false, false, false).is_empty());
        assert_eq!(offered_engines(false, true, true, false), ["auto", "system_tesseract", "ocrs"]);
        let all = offered_engines(true, true, true, true);
        for engine in ["auto", "surya", "unified", "tesseract_fraktur", "olmocr", "lfm2_vl", "system_tesseract", "ocrs", "paddle"] {
            assert!(all.contains(&engine), "missing {engine}");
        }
        assert_eq!(all.iter().filter(|engine| **engine == "auto").count(), 1);
    }

    #[test]
    fn engine_and_language_choices_produce_explicit_config() {
        let (config, warnings) = recognition_config(Default::default(), Some("tesseract"), Some("de")).unwrap();
        assert!(!config.router);
        assert_eq!(config.stages[0].rec_model.as_deref(), Some("tesseract-deu"));
        assert!(warnings.is_empty());
        let (config, _) = recognition_config(Default::default(), Some("dbnet_trocr"), None).unwrap();
        assert_eq!(config.stages[0].rec_model.as_deref(), Some("trocr-printed"));
        assert!(recognition_config(Default::default(), Some("missing-engine"), None).is_err());
        assert!(recognition_config(Default::default(), Some("tesseract"), Some("../../invalid")).is_err());
        assert!(recognition_config(Default::default(), Some("tesseract"), Some("pol")).unwrap_err().contains("custom language model"));
        let (got, warnings) = recognition_config(Default::default(), Some("got"), Some("de")).unwrap();
        assert!(got.stages[0].vlm_prompt.is_empty());
        assert!(!warnings.is_empty());
    }

    #[test]
    fn cpu_budget_is_limited_to_qwen2vl_in_spawn_capable_builds() {
        let (qwen, _) = recognition_config(Default::default(), Some("qwen2vl"), None).unwrap();
        let (trocr, _) = recognition_config(Default::default(), Some("dbnet_trocr"), None).unwrap();
        if crate::extractors::ocr_worker::CPU_BUDGET_AVAILABLE && cfg!(feature = "crispembed") {
            assert!(check_cpu_budget(&qwen).is_ok());
            assert!(check_cpu_budget(&trocr).unwrap_err().contains("Qwen2 VL engine only"));
            assert!(check_cpu_budget(&Default::default()).is_err(), "automatic routing is not a single Qwen stage");
        } else {
            assert!(check_cpu_budget(&qwen).is_err());
        }
    }

    #[test]
    fn every_embedded_choice_builds_a_stage_and_custom_models_remain_configurable() {
        for engine in WORKSPACE_ENGINES.iter().filter(|e| **e != "auto") {
            let (config, _) = recognition_config(Default::default(), Some(engine), None)
                .unwrap_or_else(|e| panic!("{engine}: {e}"));
            assert_eq!(config.stages.len(), 1, "{engine}");
            assert_eq!(config.stages[0].engine, *engine);
        }
        // Newly offered engines rely on the native per-engine defaults rather
        // than inheriting the DBNet/TrOCR pair or another engine's weights.
        for engine in ["ppocrv6", "easyocr", "tesseract_fraktur", "olmocr", "lfm2_vl"] {
            let (config, _) = recognition_config(Default::default(), Some(engine), None).unwrap();
            assert_eq!(config.stages[0].rec_model, None, "{engine}");
        }
        assert!(WORKSPACE_ENGINES.contains(&"unified"), "custom-model engines share the same catalog");
        // Every engine Settings can put in a pipeline stage must also pass the
        // workspace's stored-config validation, or recognition fails for users
        // who configured it there.
        for engine in ["unified", "tesseract_fraktur", "ppocrv6", "easyocr", "olmocr", "lfm2_vl", "unlimited_ocr2", "minicpm_ocr"] {
            let mut config = crate::extractors::OcrPipelineConfig::default();
            config.stages.push(serde_json::from_value(json!({"engine": engine})).unwrap());
            assert!(validate_config(&config).is_ok(), "{engine}");
        }
    }

    #[test]
    fn engine_changes_do_not_inherit_another_recognizers_weights() {
        let mut profile = crate::extractors::OcrPipelineConfig::default();
        profile.det_model = Some("dbnet-det".into());
        profile.rec_model = Some("trocr-printed".into());
        let (qwen, _) = recognition_config(profile.clone(), Some("qwen2vl"), None).unwrap();
        assert!(qwen.stages[0].det_model.is_none());
        assert!(qwen.stages[0].rec_model.is_none());
        let (tesseract, _) = recognition_config(profile.clone(), Some("tesseract"), Some("de")).unwrap();
        assert_eq!(tesseract.stages[0].rec_model.as_deref(), Some("tesseract-deu"));
        profile.stages = vec![serde_json::from_value(json!({"engine":"qwen2vl","det_model":"custom-qwen.gguf"})).unwrap()];
        let (qwen, _) = recognition_config(profile, Some("qwen2vl"), None).unwrap();
        assert_eq!(qwen.stages[0].det_model.as_deref(), Some("custom-qwen.gguf"));
    }

    #[test]
    fn invalid_configuration_never_reaches_native_models() {
        let mut config = crate::extractors::OcrPipelineConfig::default();
        config.min_confidence = f32::NAN;
        assert!(validate_config(&config).is_err());
        config.min_confidence = 0.5; config.sr_max_short_side = -100;
        assert!(validate_config(&config).is_err());
        let mut config = crate::extractors::OcrPipelineConfig::default();
        config.stages = vec![serde_json::from_value(json!({"engine":"misspelled-engine"})).unwrap()];
        assert!(validate_config(&config).is_err());
        config.stages[0].engine = "tesseract".into();
        config.stages[0].source_type = "misspelled-source".into();
        assert!(validate_config(&config).is_err());
    }
}
