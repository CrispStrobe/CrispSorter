//! Durable, non-destructive OCR review projects.
//!
//! Source files are never edited. Imports and page transformations create
//! immutable, content-addressed PNG assets under the application data directory.
//! Project saves use revision checks and same-directory atomic replacement;
//! exports use the reviewed pixels and text, never silently rerun recognition.

use crate::extractors::ocr_render::{OcrOutputFormat, OcrRegion, RenderPage};
use crate::index::tauri_commands::{KieFieldDto, OcrExportResult};
use crate::AppState;
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
use tauri::{Manager, State};

#[path = "ocr_document_import.rs"]
mod document_import;
#[path = "ocr_atomic_output.rs"]
mod atomic_output;
use atomic_output::{atomic_write, atomic_write_with, sync_directory};

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
    let assets = root.join("assets");
    fs::create_dir_all(&assets).map_err(|e| format!("Create page cache: {e}"))?;
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
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {},
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

/// Engines the workspace offers and accepts. All exist in the pinned CrispEmbed
/// runtime and have registry defaults, so a stage needs no explicit model.
/// `unified` (needs an explicit GGUF) and the env-gated experimental engines
/// stay in Settings only.
const WORKSPACE_ENGINES: &[&str] = &[
    "auto", "dbnet_trocr", "tesseract", "tesseract_fraktur", "parseq", "ppocrv6", "easyocr",
    "got", "glm", "qwen2vl", "internvl2", "deepseek_ocr2", "pix2struct", "granite_vision",
    "lightonocr", "qwen3vl", "unlimited_ocr", "olmocr", "lfm2_vl",
];

#[tauri::command]
pub fn ocr_workspace_capabilities() -> Value {
    let native = cfg!(feature = "crispembed");
    let mut formats = vec!["text", "markdown", "html", "json"];
    if native { formats.extend(["hocr", "alto"]); }
    if reviewed_pdf_available() { formats.push("pdf"); }
    if crate::extractors::ocr_render::reviewed_archival_pdf_available() { formats.push("pdfa"); }
    if cfg!(feature = "desktop") { formats.push("docx"); }
    json!({
        "schema_version": PROJECT_VERSION,
        "ocr": native, "crispembed": native,
        "camera_import": true, "photo_tools": cfg!(feature = "ocr-photo-tools"),
        "mobile_model_store": crate::model_provisioning::enabled(),
        "default_engine": if crate::model_provisioning::enabled() { "tesseract" } else { "auto" },
        "default_language": if crate::model_provisioning::enabled() { "eng" } else { "auto" },
        "pdf_import": cfg!(any(feature = "pdf-render", target_os = "android", target_os = "ios")),
        "native_document_import": cfg!(any(target_os = "android", target_os = "ios")),
        "pdf_export_reason": if reviewed_pdf_available() { None } else { Some("Searchable PDF requires the matching updated CrispEmbed renderer and an OCR workspace build. Export hOCR, ALTO, text, or JSON with this build.") },
        "archival_pdf_export": crate::extractors::ocr_render::reviewed_archival_pdf_available(),
        "pdf_bidi_export": crate::extractors::ocr_render::reviewed_bidi_pdf_available(),
        "pdfa_conformance": if crate::extractors::ocr_render::reviewed_archival_pdf_available() { Some("PDF/A-2b") } else { None },
        "pdfa_export_reason": if crate::extractors::ocr_render::reviewed_archival_pdf_available() { None } else { Some("PDF/A-2b export requires a matching native renderer with archival resources. Choose another available output format with this build.") },
        "cleanup": native, "layout": native, "kie": native, "table": native, "math": native,
        "structured_export": native, "formats": formats,
        "review_diagnostics": cfg!(feature = "ocr-review-diagnostics"),
        // Isolated low-memory Qwen2-VL worker (see `ocr_worker::run_cpu_budget`).
        "cpu_budget": native && crate::extractors::ocr_worker::CPU_BUDGET_AVAILABLE,
        "cpu_budget_pixels": crate::extractors::ocr_worker::CPU_PIXEL_BUDGET,
        "engines": if native { WORKSPACE_ENGINES.to_vec() } else { vec![] },
        "reason": if native { None } else { Some("Recognition and document models require a build with CrispEmbed. Project editing, import of images and TIFFs, and text exports remain available.") },
        "pdf_reason": if cfg!(any(feature = "pdf-render", target_os = "android", target_os = "ios")) { None } else { Some("PDF import requires a build with PDF rendering enabled. Import page images or a TIFF instead.") },
        "max_page_pixels": MAX_PAGE_PIXELS,
    })
}

fn reviewed_pdf_available() -> bool {
    crate::extractors::ocr_render::reviewed_pdf_render_available()
}

#[tauri::command]
pub async fn ocr_workspace_import(app: tauri::AppHandle, location_uri: String, request_id: Option<String>) -> Result<ImportedDocument, String> {
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
}

/// Camera and provider bytes never become paths supplied by the webview. Keep
/// their originals by content hash before creating the oriented page assets.
#[tauri::command]
pub async fn ocr_workspace_import_image(app: tauri::AppHandle, request: tauri::ipc::Request<'_>) -> Result<ImportedDocument, String> {
    let (mut metadata, bytes) = import_request(&request)?;
    let root = workspace_root(&app)?;
    metadata.source_uri = None;
    document_import::from_bytes(app, root, metadata, bytes, false).await
}

#[tauri::command]
pub async fn ocr_workspace_import_bytes(app: tauri::AppHandle, request: tauri::ipc::Request<'_>) -> Result<ImportedDocument, String> {
    let (metadata, bytes) = import_request(&request)?;
    let root = workspace_root(&app)?;
    document_import::from_bytes(app, root, metadata, bytes, true).await
}

#[tauri::command]
pub fn ocr_workspace_cancel_import(request_id: String) -> Result<(), String> {
    document_import::cancel(&request_id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportMetadata {
    name: String,
    source_uri: Option<String>,
    request_id: Option<String>,
}

fn import_request(request: &tauri::ipc::Request<'_>) -> Result<(ImportMetadata, Vec<u8>), String> {
    use base64::Engine;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("Photo import requires a raw binary payload.".into());
    };
    if bytes.is_empty() || bytes.len() > MAX_CAPTURE_BYTES {
        return Err("Choose a nonempty document no larger than 32 MiB.".into());
    }
    let header = request.headers().get("x-ocr-metadata").ok_or("Missing photo import metadata.")?;
    if header.as_bytes().len() > 16 * 1024 { return Err("Photo import metadata is too large.".into()); }
    let json = base64::engine::general_purpose::STANDARD.decode(header.as_bytes())
        .map_err(|_| "Photo import metadata is invalid.")?;
    let metadata = serde_json::from_slice(&json).map_err(|_| "Photo import metadata is invalid.")?;
    let mut owned = Vec::new();
    owned.try_reserve_exact(bytes.len()).map_err(|_| "Not enough memory to import this photo. Try a smaller capture.")?;
    owned.extend_from_slice(bytes);
    Ok((metadata, owned))
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
    let root = workspace_root(&app)?;
    let path = managed_page(&root, &page_path)?;
    let size = checked_physical_size(page_width_points, page_height_points)?;
    tokio::task::spawn_blocking(move || transform_page_with_photo(&root, &path, &operation, degrees, crop, split_x, size, quad, angle)).await
        .map_err(|e| format!("Page transform worker: {e}"))?
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

#[tauri::command]
pub async fn ocr_workspace_prepare(
    app: tauri::AppHandle,
    page_path: String,
    config: crate::extractors::OcrPipelineConfig,
    page_width_points: Option<f64>,
    page_height_points: Option<f64>,
) -> Result<PreparedPage, String> {
    let root = workspace_root(&app)?;
    let path = managed_page(&root, &page_path)?;
    validate_config(&config)?;
    let size = checked_physical_size(page_width_points, page_height_points)?;
    tokio::task::spawn_blocking(move || prepare_page(&root, &path, &config, size)).await
        .map_err(|e| format!("Page preparation worker: {e}"))?
}

fn prepare_page(root: &Path, source: &Path, config: &crate::extractors::OcrPipelineConfig, mut physical_size: Option<(f64, f64)>) -> Result<PreparedPage, String> {
    use crate::extractors::ocr_crispembed;
    let mut path = source.to_path_buf();
    let mut guards = Vec::new();
    let mut warnings = Vec::new();
    checked_dimensions(&path)?;
    if config.dewarp {
        if let Some((guard, output)) = ocr_crispembed::dewarp_page(&path, config) {
            path = output; guards.push(guard);
        } else {
            warnings.push("Dewarp did not produce an image. The preceding image was retained; inspect curved lines before approving.".into());
        }
    }
    if config.restore {
        let (guard, output) = ocr_crispembed::restore_page(&path, config)
            .ok_or("The selected restoration engine did not produce an image. Check the model installation or disable restoration and retry.")?;
        path = output; guards.push(guard);
    }
    if config.sr {
        let (w, h) = checked_dimensions(&path)?;
        if w.min(h) as i32 <= config.sr_max_short_side {
            if u64::from(w) * u64::from(h) * 16 > MAX_PAGE_PIXELS {
                return Err("A 4× upscale would exceed the 100 megapixel page limit. Crop the page or disable super-resolution.".into());
            }
            let (guard, output) = ocr_crispembed::super_resolve_page(&path, config)
                .ok_or("The selected super-resolution engine did not produce an image. Check the model installation or disable super-resolution and retry.")?;
            path = output; guards.push(guard);
        }
    }
    let image = decode_page(&path)?;
    // Restoration and super-resolution change the sampling density, not the
    // physical sheet size. Only the subsequent pixel crop changes the paper.
    let before_cleanup = (image.width(), image.height());
    let recipe = config.stages.first().map(|s| s.cleanup.clone()).unwrap_or_default();
    if config.stages.iter().any(|s| s.cleanup.page_split) {
        warnings.push("This profile requests spread splitting. Use Split page to create and review both halves; recognition retains the complete current page.".into());
    }
    let use_cleanup = config.cleanup_enabled || config.stages.iter().any(|s| s.cleanup.enabled);
    let denoise = config.denoise || config.stages.iter().any(|s| s.cleanup.denoise);
    let image = if use_cleanup || denoise {
        if config.stages.len() > 1 && config.stages.iter().any(|s| s.cleanup.enabled) {
            warnings.push("Review uses the first stage's cleanup recipe for one shared page image. Internal stage cleanup is disabled so every recognition box aligns with that image.".into());
        }
        cleanup_image(image, config, &recipe, use_cleanup, denoise)?
    } else { image };
    physical_size = physical_size.map(|(w, h)| (w * f64::from(image.width()) / f64::from(before_cleanup.0), h * f64::from(image.height()) / f64::from(before_cleanup.1)));
    let page = persist_image(root, &image)?;
    let page = if recipe.auto_crop {
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
        Some(crate::model_provisioning::resolve_model(config.nafnet_model.as_deref().unwrap_or("nafnet-denoise"), Some(true))
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
    if !config.min_confidence.is_finite() || !(0.0..=1.0).contains(&config.min_confidence)
        || !config.layout_threshold.is_finite() || !(0.0..=1.0).contains(&config.layout_threshold)
        || !(0..=1_000_000).contains(&config.min_chars)
        || !(1..=10_000).contains(&config.sr_max_short_side) || config.stages.len() > 32 {
        return Err("Invalid OCR settings: check confidence, character and resolution limits.".into());
    }
    if !["basic", "tps"].contains(&config.dewarp_engine.as_str())
        || !["restormer", "scunet", "instructir"].contains(&config.restore_engine.as_str())
        || !["pan", "esrgan", "safmn", "hat", "tbsrn", "dat", "swinir"].contains(&config.sr_engine.as_str()) {
        return Err("Unknown scan restoration engine.".into());
    }
    if !["rtdetr", "cc"].contains(&config.layout_engine.as_str())
        || !["got", "glm", "qwen2vl", "internvl2", "qwen3vl"].contains(&config.vlm_ocr_engine.as_str()) {
        return Err("Unknown layout or escalation engine.".into());
    }
    if config.restore_engine == "instructir"
        && !["denoise", "deblur", "dehaze", "derain", "super_resolution", "low_light", "enhance"].contains(&config.restore_task.as_str()) {
        return Err("Unknown restoration task.".into());
    }
    for stage in &config.stages {
        if !["dbnet_trocr", "surya", "got", "glm", "qwen2vl", "internvl2", "tesseract", "parseq", "deepseek_ocr2", "pix2struct", "granite_vision", "lightonocr", "qwen3vl", "unlimited_ocr", "unlimited_ocr2", "minicpm_ocr", "unified", "tesseract_fraktur", "ppocrv6", "easyocr", "olmocr", "lfm2_vl"].contains(&stage.engine.as_str())
            || !["auto", "screenshot", "scanned_doc", "photo"].contains(&stage.source_type.as_str()) {
            return Err("Unknown OCR stage engine or source type.".into());
        }
        if stage.vlm_prompt.len() > 32_768 || stage.vlm_prompt.contains('\0')
            || !stage.min_confidence.is_finite() || !(0.0..=1.0).contains(&stage.min_confidence)
            || !stage.det_prob_threshold.is_finite() || !(0.0..=1.0).contains(&stage.det_prob_threshold)
            || !stage.det_box_threshold.is_finite() || !(0.0..=1.0).contains(&stage.det_box_threshold)
            || !(16..=8192).contains(&stage.det_target_short)
            || !(0..=32768).contains(&stage.vlm_max_tokens)
            || !(0..=1_000_000).contains(&stage.min_chars) {
            return Err("Invalid OCR stage settings.".into());
        }
        let cleanup = &stage.cleanup;
        if !(0..=1).contains(&cleanup.binarize_method)
            || !cleanup.sauvola_k.is_finite() || !(0.0..=1.0).contains(&cleanup.sauvola_k)
            || !(3..=501).contains(&cleanup.sauvola_window)
            || !(1..=501).contains(&cleanup.morph_kernel)
            || !cleanup.border_threshold.is_finite() || !(0.0..=1.0).contains(&cleanup.border_threshold)
            || !cleanup.deskew_max_angle.is_finite() || !(0.0..=45.0).contains(&cleanup.deskew_max_angle) {
            return Err("Invalid scan cleanup parameters.".into());
        }
    }
    let as_json = serde_json::to_string(config).map_err(|e| e.to_string())?;
    if as_json.contains("\\u0000") { return Err("OCR model paths cannot contain null characters.".into()); }
    Ok(())
}

pub(crate) fn recognition_config(mut config: crate::extractors::OcrPipelineConfig, engine: Option<&str>, language: Option<&str>) -> Result<(crate::extractors::OcrPipelineConfig, Vec<String>), String> {
    let engine = engine.filter(|s| !s.is_empty()).unwrap_or("auto");
    let language = language.filter(|s| !s.is_empty() && *s != "auto");
    let mut warnings = Vec::new();
    if let Some(language) = language {
        if language.len() > 32 || !language.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("Choose a language code, for example en, de, eng or deu.".into());
        }
    }
    if engine != "auto" {
        if !WORKSPACE_ENGINES.contains(&engine) {
            return Err(format!("Unsupported OCR engine: {engine}"));
        }
        let configured_stage = config.stages.iter().find(|s| s.engine == engine).cloned();
        let mut stage: crate::extractors::OcrStageSpec = if let Some(stage) = configured_stage {
            stage
        } else {
            let mut stage: crate::extractors::OcrStageSpec = serde_json::from_value(json!({"engine":engine}))
                .map_err(|e| e.to_string())?;
            if ["dbnet_trocr", "tesseract", "parseq"].contains(&engine) {
                stage.det_model = config.det_model.clone();
                if engine == "dbnet_trocr" { stage.rec_model = config.rec_model.clone(); }
            } else if config.vlm_ocr_engine == engine {
                // Flat det/rec model fields describe the DBNet path, never a
                // newly selected vision engine. Only use its matching VLM slot.
                stage.det_model = config.vlm_ocr_model.clone();
            }
            stage
        };
        stage.source_type = "auto".into();
        stage.min_confidence = config.min_confidence;
        stage.min_chars = config.min_chars;
        if engine == "tesseract" && (language.is_some() || stage.rec_model.is_none()) {
            let lang = match language.unwrap_or("eng") {
                "en" => "eng", "de" => "deu", "fr" => "fra", "es" => "spa", "it" => "ita",
                "pt" => "por", "nl" => "nld", "ru" => "rus", "ar" => "ara", "zh" | "chi_sim" => "chi-sim",
                "ja" => "jpn", "ko" => "kor", lang => lang,
            };
            if !["eng", "deu", "fra", "spa", "ita", "por", "nld", "rus", "ara", "chi-sim", "jpn", "kor"].contains(&lang) {
                return Err(format!("No registered Tesseract model is available for '{lang}'. Configure a matching custom language model in an OCR stage and use Automatic language, or select a vision engine."));
            }
            stage.rec_model = Some(format!("tesseract-{lang}"));
        } else if stage.rec_model.is_none() {
            stage.rec_model = match engine {
                "dbnet_trocr" => Some("trocr-printed".into()), "parseq" => Some("parseq".into()), _ => None,
            };
        }
        if let Some(language) = language {
            if ["qwen2vl", "internvl2", "granite_vision", "qwen3vl"].contains(&engine) {
                stage.vlm_prompt = format!("Transcribe the document exactly. The expected language is {language}. Preserve reading order, numbers, punctuation and line breaks. Do not summarize or translate.");
            } else if engine != "tesseract" {
                warnings.push("This recognizer uses its model's trained languages; the language selector does not change that model.".into());
            }
        }
        // Page cleanup is explicit and shared, independent of engine choice.
        stage.cleanup.enabled = config.cleanup_enabled;
        stage.cleanup.denoise = config.denoise;
        config.stages = vec![stage];
        config.router = false;
    } else if language.is_some() {
        warnings.push("Automatic routing uses the configured models' languages. Select Tesseract or a vision engine to apply a specific language hint.".into());
    }
    config.enabled = true;
    validate_config(&config)?;
    Ok((config, warnings))
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
) -> Result<RecognizedPage, String> {
    if !cfg!(feature = "crispembed") { return Err("Recognition requires a build with CrispEmbed.".into()); }
    let root = workspace_root(&app)?;
    let path = managed_page(&root, &page_path)?;
    let size = checked_physical_size(page_width_points, page_height_points)?;
    let config = match config {
        Some(config) => config,
        None => state.bg_ingest.lock().await.ocr_pipeline.clone(),
    };
    let (config, warnings) = recognition_config(config, engine.as_deref(), language.as_deref())?;
    let cpu_budget = cpu_pixel_budget.unwrap_or(false);
    if cpu_budget {
        check_cpu_budget(&config)?;
    }
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    tokio::task::spawn_blocking(move || recognize_page(&root, &path, &config, zone, warnings, size, cpu_budget)).await
        .map_err(|e| format!("Recognition worker: {e}"))?
}

/// The CPU pixel budget limits the Qwen2-VL vision input; it has no meaning
/// for other engines, and the worker cannot run in sandboxed/mobile builds.
fn check_cpu_budget(config: &crate::extractors::OcrPipelineConfig) -> Result<(), String> {
    if !crate::extractors::ocr_worker::CPU_BUDGET_AVAILABLE {
        return Err("The low-memory CPU profile requires a desktop build with sidecars enabled.".into());
    }
    if !(config.stages.len() == 1 && config.stages[0].engine == "qwen2vl") {
        return Err("The low-memory CPU profile applies to the Qwen2 VL engine only. Choose that engine or turn the profile off.".into());
    }
    Ok(())
}

/// Recognize in the isolated low-memory worker and present the result with
/// honest provenance: VLM regions carry no calibrated detector score, so only
/// per-character recognition evidence is reported as a confidence.
fn recognize_with_cpu_budget(path: &Path, config: &crate::extractors::OcrPipelineConfig) -> Result<crate::extractors::ocr_crispembed::OcrReviewResult, String> {
    use crate::extractors::ocr_crispembed::{review_pipeline_config, OcrReviewRegion, OcrReviewResult};
    let (width, height) = image::image_dimensions(path).map_err(|e| format!("Read page dimensions: {e}"))?;
    let regions = crate::extractors::ocr_worker::run_cpu_budget(path, &review_pipeline_config(config))
        .map_err(|e| format!("Low-memory CPU recognition: {e:#}"))?;
    let regions = regions.into_iter().map(|r| {
        let recognition = !r.char_conf.is_empty();
        OcrReviewRegion {
            text: r.text, x: r.x, y: r.y, w: r.w, h: r.h,
            confidence: recognition.then_some(r.confidence),
            confidence_basis: if recognition { "recognition" } else { "unknown" }.into(),
            char_conf: r.char_conf, char_conf_unit: "unknown".into(), char_conf_aligned: false,
            kind: "text".into(),
        }
    }).collect();
    Ok(OcrReviewResult {
        image_path: path.to_owned(), width, height, regions,
        warnings: vec![format!("Low-memory CPU profile: the vision input was limited to {} pixels, so fine print may be missed. Review the result.", crate::extractors::ocr_worker::CPU_PIXEL_BUDGET)],
        diagnostics: json!({"cpu_pixel_budget": crate::extractors::ocr_worker::CPU_PIXEL_BUDGET, "isolated_worker": true}),
    })
}

fn recognize_page(root: &Path, path: &Path, config: &crate::extractors::OcrPipelineConfig, zone: Option<PixelRect>, mut warnings: Vec<String>, physical_size: Option<(f64, f64)>, cpu_budget: bool) -> Result<RecognizedPage, String> {
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
    } else {
        let prepared = prepare_page(root, path, config, physical_size)?;
        warnings.extend(prepared.warnings);
        let recognition_path = PathBuf::from(&prepared.page.image_path);
        (prepared.page, recognition_path, (0, 0))
    };
    let mut result = if cpu_budget {
        recognize_with_cpu_budget(&recognition_path, config)?
    } else {
        crate::extractors::ocr_crispembed::ocr_regions_for_review(&recognition_path, config)
            .map_err(|e| format!("Recognize page: {e:#}"))?
    };
    warnings.append(&mut result.warnings);
    if result.regions.is_empty() {
        warnings.push("No text was returned. Verify whether this page is blank, or retry with a different engine. An empty result is not evidence of successful recognition.".into());
    }
    if config.layout && zone.is_none() {
        annotate_layout(&recognition_path, config, &mut result.regions, &mut result.diagnostics, &mut warnings);
    }
    for region in &mut result.regions {
        region.x += offset.0 as f32;
        region.y += offset.1 as f32;
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

fn annotate_layout(path: &Path, config: &crate::extractors::OcrPipelineConfig, regions: &mut Vec<crate::extractors::ocr_crispembed::OcrReviewRegion>, diagnostics: &mut Value, warnings: &mut Vec<String>) {
    use crate::extractors::layout::{LayoutDetector, RegionKind};
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
    let root = workspace_root(&app)?;
    tokio::task::spawn_blocking(move || save_project(&root, project, expected_revision.unwrap_or(0))).await
        .map_err(|e| format!("Project save worker: {e}"))?
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
        if page.regions.len() > MAX_REGIONS { return Err(format!("Page {} has too many regions.", i + 1)); }
        for (j, region) in page.regions.iter().enumerate() {
            checked_rect(PixelRect { x: region.x as f64, y: region.y as f64, w: region.w as f64, h: region.h as f64 }, width, height)
                .map_err(|e| format!("Page {}, region {}: {e}", i + 1, j + 1))?;
            if region.confidence.is_some_and(|score| !score.is_finite() || !(0.0..=1.0).contains(&score)) {
                return Err(format!("Page {}, region {} has an invalid confidence.", i + 1, j + 1));
            }
            if let Some(ch) = region.text.chars().find(|ch| *ch < ' ' && !['\t', '\n', '\r'].contains(ch)) {
                return Err(format!("Page {}, region {} contains unsupported control character U+{:04X}. Remove it before exporting.", i + 1, j + 1, ch as u32));
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

fn joined_text(pages: &[ExportPage]) -> String {
    pages.iter().map(|p| p.regions.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n")).collect::<Vec<_>>().join("\u{000c}")
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn transcription_confidence(region: &ExportRegion) -> f32 {
    let unknown = if cfg!(feature = "ocr-reviewed-pdf") { -1.0 } else { 0.0 };
    if region.confidence_basis.as_deref() == Some("recognition")
        && region.original_text.as_deref() == Some(region.text.as_str()) {
        region.confidence.filter(|score| score.is_finite() && (0.0..=1.0).contains(score)).unwrap_or(unknown)
    } else { unknown }
}

fn validate_export_format(format: &str, pdfa: bool) -> Result<(), String> {
    if pdfa && format != "pdf" { return Err("PDF/A applies only to PDF output.".into()); }
    if pdfa && !crate::extractors::ocr_render::reviewed_archival_pdf_available() {
        return Err("PDF/A-2b requires a matching archival-capable native renderer.".into());
    }
    if OcrOutputFormat::from_name(format) == Some(OcrOutputFormat::Pdf) && !reviewed_pdf_available() {
        return Err("Searchable PDF requires the matching updated CrispEmbed renderer and an OCR workspace build. Choose hOCR, ALTO, text, or JSON with this build.".into());
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
    validate_export_format(format, pdfa)?;
    match format {
        "json" => serde_json::to_vec_pretty(&json!({"schema_version": 1, "pages": pages})).map_err(|e| e.to_string()),
        "markdown" => Ok(pages.iter().enumerate().map(|(i, p)| format!("<!-- Page {} -->\n\n{}", i + 1, p.regions.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("\n\n"))).collect::<Vec<_>>().join("\n\n---\n\n").into_bytes()),
        "html" => {
            let body = pages.iter().enumerate().map(|(i, page)| format!("<section aria-label=\"Page {}\"><h2>Page {}</h2>{}</section>", i + 1, i + 1, page.regions.iter().map(|r| format!("<p>{}</p>", escape_html(&r.text))).collect::<Vec<_>>().join("\n"))).collect::<Vec<_>>().join("\n");
            Ok(format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>Reviewed OCR document</title><style>body{{font:16px/1.6 system-ui,sans-serif;max-width:72ch;margin:3rem auto;padding:0 1rem}}p{{white-space:pre-wrap}}section{{break-after:page}}section:last-child{{break-after:auto}}h2{{font-size:1rem;color:#555}}</style></head><body>{body}</body></html>").into_bytes())
        },
        "docx" => {
            #[cfg(feature = "desktop")]
            {
                let file = tempfile::Builder::new().suffix(".docx").tempfile().map_err(|e| e.to_string())?;
                let body = joined_text(pages).replace('\u{000c}', "\n\n");
                crate::extractors::export::export_to_docx("", &body, file.path())?;
                fs::read(file.path()).map_err(|e| format!("Read generated Word document: {e}"))
            }
            #[cfg(not(feature = "desktop"))]
            { Err("Word export requires a desktop build.".into()) }
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
) -> Result<usize, String> {
    validate_export_format(format, pdfa)?;
    if OcrOutputFormat::from_name(format) == Some(OcrOutputFormat::Pdf)
        && crate::extractors::ocr_render::reviewed_streaming_pdf_available() {
        let render_pages = reviewed_render_pages(pages);
        return atomic_write_with(path, overwrite, |file| {
            let written = {
                let mut writer = std::io::BufWriter::with_capacity(64 * 1024, &mut *file);
                let count = crate::extractors::ocr_render::render_pdf_to_writer(
                    &render_pages, pdfa, max_pdf_bytes, &mut writer,
                ).map_err(|error| format!("Render OCR PDF: {error:#}"))?;
                writer.flush().map_err(|error| format!("Flush OCR PDF: {error}"))?;
                count
            };
            if written == 0 {
                return Err("The renderer produced an empty document.".into());
            }
            let actual = file.metadata().map_err(|error| format!("Check rendered PDF: {error}"))?.len();
            if actual != written {
                return Err("The renderer byte count does not match the staged PDF. No output was published.".into());
            }
            usize::try_from(written).map_err(|_| "The rendered PDF is too large for this device.".into())
        });
    }
    let bytes = export_bytes(pages, format, pdfa)?;
    if bytes.is_empty() && !["text", "txt", "plain"].contains(&format) {
        return Err("The renderer produced an empty document.".into());
    }
    atomic_write(path, &bytes, overwrite)?;
    Ok(bytes.len())
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
) -> Result<OcrExportResult, String> {
    let root = workspace_root(&app)?;
    tokio::task::spawn_blocking(move || {
        for page in &pages { managed_page(&root, &page.image_path)?; }
        validate_export_pages(&pages)?;
        let path = self::output_path(&output_path, &pages, &source_uris.unwrap_or_default(), &root)?;
        let bytes = write_export(&path, &pages, &format, pdfa, overwrite.unwrap_or(false), MAX_STREAMED_PDF_BYTES)?;
        Ok(OcrExportResult {
            saved_path: path.to_string_lossy().into_owned(), bytes, pages: pages.len(),
            format: if pdfa { "pdfa".into() } else { format },
        })
    }).await.map_err(|e| format!("Export worker: {e}"))?
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
            review: None, kind: None,
        }], id: None, name: None, source_uri: None, source_page: None, approved: None, provenance: None }
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
        let count = write_export(&path, &pages, "pdf", false, false, MAX_STREAMED_PDF_BYTES).unwrap();
        let previous = fs::read(&path).unwrap();
        assert_eq!(previous.len(), count);
        assert!(previous.len() > 4096, "fixture must exceed the native writer's minimum valid cap");
        let document = lopdf::Document::load_mem(&previous).unwrap();
        assert_eq!(document.get_pages().len(), 1);
        assert!(document.extract_text(&[1]).unwrap().contains("Reviewed résumé 東京"));
        let count_before = fs::read_dir(root.path()).unwrap().count();
        let error = write_export(&path, &pages, "pdf", false, true, 4096).unwrap_err();
        assert!(!error.is_empty());
        assert_eq!(fs::read(&path).unwrap(), previous);
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
        if crate::extractors::ocr_worker::CPU_BUDGET_AVAILABLE {
            assert!(check_cpu_budget(&qwen).is_ok());
            assert!(check_cpu_budget(&trocr).unwrap_err().contains("Qwen2 VL engine only"));
            assert!(check_cpu_budget(&Default::default()).is_err(), "automatic routing is not a single Qwen stage");
        } else {
            assert!(check_cpu_budget(&qwen).unwrap_err().contains("sidecars"));
        }
    }

    #[test]
    fn every_offered_engine_builds_a_single_stage_with_registry_defaults() {
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
        assert!(!WORKSPACE_ENGINES.contains(&"unified"), "unified needs an explicit GGUF");
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
