//! Mobile document boundary: originals are archived before native decoders run.
//! Native outputs are untrusted until every path, raster and size is validated.
use super::*;
use std::collections::VecDeque;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use tauri::Emitter;

const MAX_NATIVE_PAGE_PIXELS: u64 = 16_000_000;
const MAX_NATIVE_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
const CANCELLED: &str = "OCR_IMPORT_CANCELLED";

#[derive(Default)]
struct Control { cancelled: AtomicBool, staging: Mutex<Option<PathBuf>> }
#[derive(Default)]
struct Registry { active: Option<(String, Arc<Control>)>, pending: VecDeque<(String, Instant)> }
static IMPORTS: OnceLock<Mutex<Registry>> = OnceLock::new();

pub(super) struct ImportJob { id: String, control: Arc<Control>, app: tauri::AppHandle }
impl ImportJob {
    pub(super) fn begin(app: tauri::AppHandle, request_id: Option<String>) -> Result<Self, String> {
        let id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        uuid::Uuid::parse_str(&id).map_err(|_| "The import request ID is invalid.")?;
        let control = Arc::new(Control::default());
        {
            let mut registry = IMPORTS.get_or_init(Default::default).lock().map_err(|_| "Import state is unavailable.")?;
            if registry.active.is_some() { return Err("Another document is importing. Wait for it to finish or cancel it first.".into()); }
            registry.pending.retain(|(_, started)| started.elapsed() < Duration::from_secs(300));
            if let Some(index) = registry.pending.iter().position(|(pending, _)| pending == &id) {
                registry.pending.remove(index); control.cancelled.store(true, Ordering::Release);
            }
            registry.active = Some((id.clone(), control.clone()));
        }
        let job = Self { id, control, app };
        job.check()?;
        job.progress("archiving", 0, None);
        Ok(job)
    }
    pub(super) fn check(&self) -> Result<(), String> {
        if self.control.cancelled.load(Ordering::Acquire) { Err(CANCELLED.into()) } else { Ok(()) }
    }
    pub(super) fn progress(&self, phase: &str, completed: usize, total: Option<usize>) {
        let _ = self.app.emit("ocr-workspace-import", json!({
            "request_id": self.id, "phase": phase, "completed": completed, "total": total,
        }));
    }
    fn stage(&self, directory: &Path) -> Result<(), String> {
        let mut staging = self.control.staging.lock().map_err(|_| "Import cancellation is unavailable.")?;
        *staging = Some(directory.to_path_buf());
        if self.control.cancelled.load(Ordering::Acquire) {
            fs::write(directory.join("cancel"), []).map_err(|e| format!("Cancel import: {e}"))?;
        }
        Ok(())
    }
}
impl Drop for ImportJob {
    fn drop(&mut self) {
        if let Ok(mut registry) = IMPORTS.get_or_init(Default::default).lock() {
            if registry.active.as_ref().is_some_and(|(id, _)| id == &self.id) { registry.active = None; }
        }
    }
}

pub(super) fn cancel(request_id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(request_id).map_err(|_| "The import request ID is invalid.")?;
    let mut registry = IMPORTS.get_or_init(Default::default).lock().map_err(|_| "Import state is unavailable.")?;
    if let Some((_, control)) = registry.active.as_ref().filter(|(id, _)| id == request_id) {
        control.cancelled.store(true, Ordering::Release);
        if let Some(directory) = control.staging.lock().map_err(|_| "Import cancellation is unavailable.")?.as_ref() {
            fs::write(directory.join("cancel"), []).map_err(|e| format!("Cancel import: {e}"))?;
        }
    } else {
        // Cancellation may reach the native command before the import IPC does.
        // Keep a bounded, expiring set so this race never loses a user's stop.
        registry.pending.retain(|(id, started)| id != request_id && started.elapsed() < Duration::from_secs(300));
        while registry.pending.len() >= 64 { registry.pending.pop_front(); }
        registry.pending.push_back((request_id.to_owned(), Instant::now()));
    }
    Ok(())
}

fn private_directory(root: &Path, name: &str) -> Result<PathBuf, String> {
    let root = fs::canonicalize(root).map_err(|e| format!("Open OCR workspace: {e}"))?;
    let directory = root.join(name);
    fs::create_dir_all(&directory).map_err(|e| format!("Create OCR {name} directory: {e}"))?;
    if fs::canonicalize(&directory).map_err(|e| e.to_string())? != directory {
        return Err(format!("The OCR {name} directory must not be a symbolic link."));
    }
    Ok(directory)
}

pub(super) struct ArchivedInput { pub path: PathBuf, pub extension: &'static str, name: String, source_uri: Option<String> }
impl ArchivedInput {
    pub(super) fn attach(&self, mut document: ImportedDocument) -> ImportedDocument {
        // Keep the exact provider identity separately from the immutable source.
        document.archive_uri = Some(self.path.to_string_lossy().into_owned());
        document.source_uri = self.source_uri.clone().unwrap_or_else(|| self.path.to_string_lossy().into_owned());
        document.name = self.name.clone();
        document
    }
}

/// Recognize ISO BMFF still-image brands, not a caller-supplied file extension.
fn phone_image_extension(bytes: &[u8]) -> Option<&'static str> {
    let mut offset = 0usize;
    while offset.checked_add(8)? <= bytes.len().min(64 * 1024) {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        let (length, header) = if size == 1 {
            let end = offset.checked_add(16)?;
            let value = u64::from_be_bytes(bytes.get(offset + 8..end)?.try_into().ok()?);
            (usize::try_from(value).ok()?, 16)
        } else { (size, 8) };
        if length < header || offset.checked_add(length)? > bytes.len() { return None; }
        if kind == b"ftyp" {
            if length < header + 8 || length > 4096 || (length - header) % 4 != 0 { return None; }
            let body = &bytes[offset + header..offset + length];
            let brands: Vec<&[u8]> = std::iter::once(&body[..4]).chain(body[8..].chunks_exact(4)).collect();
            if brands.iter().any(|b| *b == b"avif" || *b == b"avis") { return Some("avif"); }
            if brands.iter().any(|b| [b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs"].iter().any(|kind| *b == *kind)) { return Some("heic"); }
            if brands.iter().any(|b| *b == b"mif1" || *b == b"msf1") { return Some("heif"); }
            return None;
        }
        if kind != b"free" && kind != b"skip" && kind != b"wide" { return None; }
        offset = offset.checked_add(length)?;
    }
    None
}

pub(super) fn archive(root: &Path, name: &str, bytes: &[u8], allow_pdf: bool, source_uri: Option<&str>) -> Result<ArchivedInput, String> {
    if bytes.is_empty() || bytes.len() > MAX_CAPTURE_BYTES { return Err("Choose a nonempty document no larger than 32 MiB.".into()); }
    if source_uri.is_some_and(|uri| !uri.starts_with("content://") && !uri.starts_with("file://")) {
        return Err("The imported provider identity must be a document provider URI.".into());
    }
    let extension = if allow_pdf && bytes.starts_with(b"%PDF-") { "pdf" }
    else if let Some(extension) = phone_image_extension(bytes) { extension }
    else {
        match image::guess_format(bytes).map_err(|_| "This image format is unsupported. Export the photo as JPEG or PNG and try again.")? {
            image::ImageFormat::Png => "png", image::ImageFormat::Jpeg => "jpg",
            image::ImageFormat::Tiff => "tiff", image::ImageFormat::Bmp => "bmp", image::ImageFormat::WebP => "webp",
            _ => return Err("Import JPEG, PNG, TIFF, BMP or WebP, or a phone image format supported by this device.".into()),
        }
    };
    let sources = private_directory(root, "sources")?;
    let digest = Sha256::digest(bytes);
    let path = sources.join(format!("{digest:x}.{extension}"));
    let mut staging = tempfile::NamedTempFile::new_in(&sources).map_err(|e| format!("Stage imported source: {e}"))?;
    staging.write_all(bytes).map_err(|e| format!("Archive imported source: {e}"))?;
    staging.as_file().sync_all().map_err(|e| e.to_string())?;
    match staging.persist_noclobber(&path) {
        Ok(_) => sync_directory(&sources)?,
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.file_type().is_file() || metadata.len() != bytes.len() as u64 || fs::canonicalize(&path).map_err(|e| e.to_string())? != path {
                return Err("The archived source does not match this import. Restore the OCR workspace before retrying.".into());
            }
            let mut existing = File::open(&path).map_err(|e| e.to_string())?.take(bytes.len() as u64 + 1);
            let mut hash = Sha256::new(); let mut buffer = [0u8; 64 * 1024];
            loop { let count = existing.read(&mut buffer).map_err(|e| e.to_string())?; if count == 0 { break; } hash.update(&buffer[..count]); }
            if hash.finalize() != digest { return Err("The archived source failed its content hash check.".into()); }
        }
        Err(e) => return Err(format!("Commit imported source: {}", e.error)),
    }
    let mut display_name: String = name.rsplit(['/', '\\']).next().unwrap_or("").chars().filter(|c| !c.is_control()).take(240).collect();
    if display_name.trim().is_empty() { display_name = format!("Captured page.{extension}"); }
    Ok(ArchivedInput { path, extension, name: display_name, source_uri: source_uri.map(str::to_owned) })
}

pub(super) async fn from_bytes(app: tauri::AppHandle, root: PathBuf, metadata: ImportMetadata, bytes: Vec<u8>, allow_pdf: bool) -> Result<ImportedDocument, String> {
    let job = Arc::new(ImportJob::begin(app.clone(), metadata.request_id)?);
    let archive_root = root.clone(); let archive_job = job.clone();
    let archived = tokio::task::spawn_blocking(move || {
        archive_job.check()?;
        archive(&archive_root, &metadata.name, &bytes, allow_pdf, metadata.source_uri.as_deref())
    }).await.map_err(|e| format!("Source archive worker: {e}"))??;
    job.check()?;
    #[cfg(any(target_os = "android", target_os = "ios"))]
    if ["pdf", "heic", "heif", "avif"].contains(&archived.extension) {
        return native_import(&app, &root, archived, job).await;
    }
    let worker_job = job.clone();
    let document = tokio::task::spawn_blocking(move || {
        worker_job.check()?;
        import_document_with_progress(&root, &archived.path, Some(&worker_job)).map(|document| archived.attach(document))
    }).await.map_err(|e| format!("Document import worker: {e}"))??;
    job.check()?; job.progress("complete", document.pages.len(), Some(document.pages.len()));
    Ok(document)
}

#[derive(Debug, Deserialize)]
struct NativePage { path: String, width: u32, height: u32, width_points: f64, height_points: f64, #[serde(default)] warnings: Vec<String> }
#[derive(Debug, Deserialize)]
struct NativeDocument { pages: Vec<NativePage>, bytes: u64 }

#[cfg(any(target_os = "android", target_os = "ios"))]
async fn native_import(app: &tauri::AppHandle, root: &Path, archived: ArchivedInput, job: Arc<ImportJob>) -> Result<ImportedDocument, String> {
    let staging_root = private_directory(root, "import-staging")?;
    let staging = tempfile::Builder::new().prefix("import-").tempdir_in(&staging_root).map_err(|e| format!("Stage import: {e}"))?;
    job.stage(staging.path())?;
    job.check()?;
    let native_result = {
    let source_path = archived.path.to_str().ok_or("The source path is not valid UTF-8.")?;
    let output_dir = staging.path().to_str().ok_or("The import staging path is not valid UTF-8.")?;
    let kind = if archived.extension == "pdf" { "pdf" } else { "image" };
    let render = tauri_plugin_mobile_provider::rasterize_ocr_document(app, source_path, output_dir, kind);
    tokio::pin!(render);
    let mut last_progress = Vec::new();
    loop {
        tokio::select! {
            result = &mut render => break result,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                // Waiting for native completion keeps staging alive after cancel.
                let progress = staging.path().join("progress.json");
                if let Ok(file) = File::open(progress) {
                    let mut bytes = Vec::new();
                    if file.take(4097).read_to_end(&mut bytes).is_ok() && bytes.len() <= 4096 && bytes != last_progress {
                        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                            if let (Some(completed), Some(total)) = (value["completed"].as_u64(), value["total"].as_u64()) {
                                if total > 0 && total <= MAX_PAGES as u64 && completed <= total { job.progress("rendering", completed as usize, Some(total as usize)); }
                            }
                        }
                        last_progress = bytes;
                    }
                }
            }
        }
    }
    };
    job.check()?;
    let native: NativeDocument = serde_json::from_value(native_result?).map_err(|e| format!("Invalid native import result: {e}"))?;
    let root = root.to_path_buf(); let worker_job = job.clone();
    let result = tokio::task::spawn_blocking(move || {
        let pages = adopt_pages(&root, staging.path(), native, Some(&worker_job))?;
        let document = archived.attach(ImportedDocument { source_uri: String::new(), archive_uri: None, name: String::new(), pages });
        worker_job.check()?;
        Ok::<_, String>(document)
    }).await.map_err(|e| format!("Import verification worker: {e}"))??;
    job.check()?; job.progress("complete", result.pages.len(), Some(result.pages.len()));
    Ok(result)
}

/// Validate and copy a single complete PNG at a time; never retain a document's
/// decoded images together. No native-provided path escapes its import directory.
fn adopt_pages(root: &Path, staging: &Path, native: NativeDocument, job: Option<&ImportJob>) -> Result<Vec<PageAsset>, String> {
    if native.pages.is_empty() || native.pages.len() > MAX_PAGES || native.bytes > MAX_NATIVE_OUTPUT_BYTES {
        return Err("The native document exceeds the page or output-size limit.".into());
    }
    let assets = private_directory(root, "assets")?;
    let staging = fs::canonicalize(staging).map_err(|e| e.to_string())?;
    let page_count = native.pages.len();
    let mut pages = Vec::with_capacity(page_count); let mut total = 0u64;
    for (index, page) in native.pages.into_iter().enumerate() {
        if let Some(job) = job { job.check()?; job.progress("verifying", index, Some(page_count)); }
        let expected = staging.join(format!("page-{:06}.png", index + 1));
        let path = PathBuf::from(&page.path);
        if path != expected || fs::canonicalize(&path).map_err(|e| e.to_string())? != expected ||
            !fs::symlink_metadata(&path).map_err(|e| e.to_string())?.file_type().is_file() {
            return Err("The native importer returned a page outside its private staging directory.".into());
        }
        let length = fs::metadata(&path).map_err(|e| e.to_string())?.len();
        total = total.checked_add(length).ok_or("Rendered document size overflow.")?;
        if length == 0 || total > MAX_NATIVE_OUTPUT_BYTES { return Err("Rendered document exceeds the 512 MiB import limit.".into()); }
        let mut source = File::open(&path).map_err(|e| e.to_string())?;
        let mut signature = [0u8; 8]; source.read_exact(&mut signature).map_err(|e| e.to_string())?;
        if &signature != b"\x89PNG\r\n\x1a\n" { return Err("The native importer did not return a PNG page.".into()); }
        let (width, height) = checked_dimensions(&path)?;
        if width != page.width || height != page.height || u64::from(width) * u64::from(height) > MAX_NATIVE_PAGE_PIXELS {
            return Err("The native page dimensions do not match the verified raster or exceed 16 megapixels.".into());
        }
        // A dimension header alone does not prove a complete, decodable output.
        drop(image::open(&path).map_err(|e| format!("Verify page {}: {e}", index + 1))?);
        let physical = checked_physical_size(Some(page.width_points), Some(page.height_points))?;
        if page.warnings.len() > 16 || page.warnings.iter().any(|warning| warning.len() > 1024) { return Err("Invalid native import warnings.".into()); }
        source.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut target = tempfile::NamedTempFile::new_in(&assets).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new(); let mut buffer = [0u8; 64 * 1024]; let mut copied = 0u64;
        loop {
            if let Some(job) = job { job.check()?; }
            let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 { break; }
            copied += count as u64;
            if copied > length { return Err("A rendered page changed while it was being imported.".into()); }
            target.write_all(&buffer[..count]).map_err(|e| e.to_string())?; hash.update(&buffer[..count]);
        }
        if copied != length { return Err("A rendered page changed while it was being imported.".into()); }
        target.as_file().sync_all().map_err(|e| e.to_string())?;
        let digest = hash.finalize();
        let committed = assets.join(format!("{digest:x}.png"));
        match target.persist_noclobber(&committed) {
            Ok(_) => sync_directory(&assets)?,
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::canonicalize(&committed).map_err(|e| e.to_string())? != committed ||
                    !fs::symlink_metadata(&committed).map_err(|e| e.to_string())?.file_type().is_file() ||
                    fs::metadata(&committed).map_err(|e| e.to_string())?.len() != length { return Err("Invalid existing OCR page asset.".into()); }
                let mut existing = File::open(&committed).map_err(|e| e.to_string())?.take(length + 1);
                let mut existing_hash = Sha256::new();
                loop { let count = existing.read(&mut buffer).map_err(|e| e.to_string())?; if count == 0 { break; } existing_hash.update(&buffer[..count]); }
                if existing_hash.finalize() != digest { return Err("The existing OCR page failed its content hash check. Restore the page cache before retrying.".into()); }
            }
            Err(error) => return Err(format!("Commit imported page: {}", error.error)),
        }
        let mut asset = PageAsset { image_path: committed.to_string_lossy().into_owned(), width, height,
            page_width_points: None, page_height_points: None, warnings: page.warnings };
        set_physical_size(&mut asset, physical); pages.push(asset);
    }
    if total != native.bytes { return Err("The native import byte count does not match the verified pages.".into()); }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brand(major: &[u8; 4], compatible: &[u8]) -> Vec<u8> {
        let mut bytes = ((16 + compatible.len()) as u32).to_be_bytes().to_vec();
        bytes.extend(b"ftyp"); bytes.extend(major); bytes.extend([0; 4]); bytes.extend(compatible); bytes
    }

    #[test]
    fn detects_phone_formats_by_bounded_brands_and_rejects_malformed_boxes() {
        assert_eq!(phone_image_extension(&brand(b"mif1", b"heic")), Some("heic"));
        assert_eq!(phone_image_extension(&brand(b"mif1", b"avif")), Some("avif"));
        assert_eq!(phone_image_extension(&brand(b"mif1", b"")), Some("heif"));
        assert_eq!(phone_image_extension(&brand(b"mp42", b"isom")), None);
        for bytes in [&b"\0\0\0\0ftypheic"[..], &b"\xff\xff\xff\xffftypheic"[..], &b"\0\0\0\x18ftypheic"[..]] { assert_eq!(phone_image_extension(bytes), None); }
        assert_eq!(phone_image_extension(&brand(b"heic", &[0; 4096])), None);
    }

    #[test]
    fn archives_modern_images_without_rewriting_original_bytes_or_provider_identity() {
        let root = tempfile::tempdir().unwrap(); let bytes = brand(b"avif", b"mif1");
        let input = archive(root.path(), "Camera.HEIC", &bytes, false, Some("content://Photo/Case%2F42")).unwrap();
        assert_eq!(input.extension, "avif"); assert_eq!(fs::read(&input.path).unwrap(), bytes);
        let document = input.attach(ImportedDocument { source_uri: String::new(), archive_uri: None, name: String::new(), pages: vec![] });
        assert_eq!(document.source_uri, "content://Photo/Case%2F42");
        assert_eq!(document.archive_uri.unwrap(), input.path.to_string_lossy());
    }

    #[test]
    fn native_adoption_rejects_path_escape_bad_counts_and_truncated_png() {
        let root = tempfile::tempdir().unwrap(); let staging = root.path().join("staging"); fs::create_dir(&staging).unwrap();
        let path = staging.join("page-000001.png");
        image::RgbImage::from_pixel(20, 30, image::Rgb([40, 80, 120])).save(&path).unwrap();
        let bytes = fs::metadata(&path).unwrap().len();
        let make = || NativeDocument { bytes, pages: vec![NativePage { path: path.to_string_lossy().into_owned(), width: 20, height: 30,
            width_points: 200.0, height_points: 300.0, warnings: vec!["Source retained".into()] }] };
        let valid = adopt_pages(root.path(), &staging, make(), None).unwrap();
        assert_eq!(valid[0].page_width_points, Some(200.0)); assert_eq!(valid[0].warnings, vec!["Source retained"]);
        let asset = fs::read(&valid[0].image_path).unwrap();
        fs::write(&valid[0].image_path, vec![0u8; asset.len()]).unwrap();
        assert!(adopt_pages(root.path(), &staging, make(), None).unwrap_err().contains("hash check"));
        fs::write(&valid[0].image_path, asset).unwrap();
        let mut escaped = make(); escaped.pages[0].path = root.path().join("outside.png").to_string_lossy().into_owned();
        assert!(adopt_pages(root.path(), &staging, escaped, None).is_err());
        let mut wrong = make(); wrong.bytes += 1; assert!(adopt_pages(root.path(), &staging, wrong, None).is_err());
        let mut wrong = make(); wrong.pages[0].width += 1; assert!(adopt_pages(root.path(), &staging, wrong, None).is_err());
        let data = fs::read(&path).unwrap(); fs::write(&path, &data[..32]).unwrap();
        let mut truncated = make(); truncated.bytes = 32; assert!(adopt_pages(root.path(), &staging, truncated, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_source_archive_and_rendered_page_symlinks() {
        let root = tempfile::tempdir().unwrap(); let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("sources")).unwrap();
        assert!(archive(root.path(), "image.heic", &brand(b"heic", b""), false, None).is_err());
        let staging = root.path().join("staging"); fs::create_dir(&staging).unwrap();
        let escaped = outside.path().join("page.png"); image::RgbImage::new(10, 10).save(&escaped).unwrap();
        let link = staging.join("page-000001.png"); std::os::unix::fs::symlink(&escaped, &link).unwrap();
        let native = NativeDocument { bytes: fs::metadata(&escaped).unwrap().len(), pages: vec![NativePage {
            path: link.to_string_lossy().into_owned(), width: 10, height: 10, width_points: 100.0, height_points: 100.0, warnings: vec![] }] };
        assert!(adopt_pages(root.path(), &staging, native, None).is_err());
    }

    #[test]
    fn source_pinned_pdf_fixture_is_intact() {
        let bytes = include_bytes!("../../tests/fixtures/mobile-documents/two-pages.pdf");
        let manifest: Value = serde_json::from_str(include_str!("../../tests/fixtures/mobile-documents/manifest.json")).unwrap();
        assert_eq!(manifest["bytes"].as_u64().unwrap(), bytes.len() as u64);
        assert_eq!(manifest["sha256"].as_str().unwrap(), format!("{:x}", Sha256::digest(bytes)));
        assert_eq!(manifest["pages"].as_array().unwrap().len(), 2);
    }
}
