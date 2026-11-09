//! App-private model provisioning for the paired mobile OCR profile.
//!
//! Desktop resolution is delegated unchanged. On mobile, recognition only reads
//! complete verified installs; explicit user requests download over HTTPS in
//! Rust, without subprocesses or process-environment changes.

use std::path::{Path, PathBuf};

#[cfg(any(feature = "ocr-mobile", test))]
#[path = "model_storage.rs"]
mod storage;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ModelStorageReport {
    pub bytes: u64,
    pub reclaimable_bytes: u64,
    pub models: Vec<ModelStorageEntry>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ModelStorageEntry {
    pub name: String,
    pub version: Option<String>,
    pub bytes: u64,
    pub reclaimable_bytes: u64,
    pub in_use: bool,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ModelStorageCleanup {
    pub removed_bytes: u64,
    pub removed_versions: usize,
    pub report: ModelStorageReport,
}

pub fn enabled() -> bool { cfg!(feature = "ocr-mobile") }

pub fn initialize(app_data: &Path) -> Result<(), String> {
    #[cfg(feature = "ocr-mobile")]
    { mobile::initialize(app_data) }
    #[cfg(not(feature = "ocr-mobile"))]
    { let _ = app_data; Ok(()) }
}

#[cfg(feature = "crispembed")]
pub fn resolve_model(name: &str, auto_download: Option<bool>) -> Result<String, String> {
    #[cfg(feature = "ocr-mobile")]
    {
        let _ = auto_download;
        if Path::new(name).is_file() { mobile::resolve_local(name) }
        else { mobile::resolve_installed(name, true) }
    }
    #[cfg(not(feature = "ocr-mobile"))]
    {
        if Path::new(name).is_file() { return Ok(name.to_owned()); }
        crispembed::CrispEmbed::resolve_model(name, auto_download)
    }
}

pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(feature = "ocr-mobile")]
    { mobile::cache_dir() }
    #[cfg(not(feature = "ocr-mobile"))]
    { None }
}

pub fn is_installed(name: &str) -> bool {
    #[cfg(feature = "ocr-mobile")]
    { mobile::resolve_installed(name, false).is_ok() }
    #[cfg(not(feature = "ocr-mobile"))]
    { let _ = name; false }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DownloadProgress {
    pub name: String,
    pub filename: String,
    pub phase: &'static str,
    pub downloaded: u64,
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}
pub const PROGRESS_EVENT: &str = "crispembed-model-download";

#[cfg(feature = "ocr-mobile")]
pub fn download(name: &str, progress: impl Fn(DownloadProgress)) -> Result<String, String> {
    mobile::download(name, progress)
}

/// Small, explicit first-run setup. Other engines use the Settings model picker.
#[tauri::command]
pub async fn ocr_workspace_install_mobile_models(
    app: tauri::AppHandle, language: Option<String>, request_id: String,
) -> Result<Vec<String>, String> {
    #[cfg(feature = "ocr-mobile")]
    {
        use tauri::Emitter;
        uuid::Uuid::parse_str(&request_id).map_err(|_| "The model installation request ID is invalid.")?;
        let (config, _) = crate::ocr_workspace::recognition_config(
            Default::default(), Some("tesseract"),
            language.as_deref().filter(|value| *value != "auto"),
        )?;
        let recognizer = config.stages.first().and_then(|stage| stage.rec_model.clone())
            .ok_or("The basic OCR language model could not be selected.")?;
        tokio::task::spawn_blocking(move || {
            let names = vec!["dbnet-det".to_string(), recognizer];
            for name in &names {
                download(name, |mut update| {
                    update.request_id = Some(request_id.clone());
                    let _ = app.emit(PROGRESS_EVENT, update);
                })?;
            }
            Ok(names)
        }).await.map_err(|error| format!("Model installer: {error}"))?
    }
    #[cfg(not(feature = "ocr-mobile"))]
    {
        let _ = (app, language, request_id);
        Err("The mobile model installer is unavailable in this build.".into())
    }
}

#[tauri::command]
pub async fn ocr_workspace_model_storage() -> Result<ModelStorageReport, String> {
    #[cfg(feature = "ocr-mobile")]
    { tokio::task::spawn_blocking(mobile::storage_report).await.map_err(|error| format!("Model storage: {error}"))? }
    #[cfg(not(feature = "ocr-mobile"))]
    { Err("Private model storage is unavailable in this build.".into()) }
}

#[tauri::command]
pub async fn ocr_workspace_clean_model_storage() -> Result<ModelStorageCleanup, String> {
    #[cfg(feature = "ocr-mobile")]
    { tokio::task::spawn_blocking(mobile::clean_storage).await.map_err(|error| format!("Model cleanup: {error}"))? }
    #[cfg(not(feature = "ocr-mobile"))]
    { Err("Private model storage is unavailable in this build.".into()) }
}

#[tauri::command]
pub async fn ocr_workspace_remove_mobile_model(name: String, expected_version: String) -> Result<ModelStorageCleanup, String> {
    #[cfg(feature = "ocr-mobile")]
    { tokio::task::spawn_blocking(move || mobile::remove_model(&name, &expected_version)).await.map_err(|error| format!("Remove model: {error}"))? }
    #[cfg(not(feature = "ocr-mobile"))]
    { let _ = (name, expected_version); Err("Private model storage is unavailable in this build.".into()) }
}

#[cfg(any(feature = "ocr-mobile", test))]
mod mobile {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::{HashMap, HashSet};
    use std::io::{Read, Write};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, SystemTime};
    #[cfg(feature = "ocr-mobile")]
    use std::time::Instant;

    const MAX_MODEL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
    const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    #[cfg(feature = "ocr-mobile")]
    static INSTALL: Mutex<()> = Mutex::new(());
    type Verification = (u64, SystemTime, String);
    static VERIFIED: OnceLock<Mutex<HashMap<PathBuf, Verification>>> = OnceLock::new();

    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
    struct Artifact {
        filename: String,
        url: String,
        sha256: String,
        primary: bool,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Manifest {
        schema_version: u32,
        model_name: String,
        version: String,
        files: Vec<Artifact>,
    }

    fn directory(parent: &Path, name: &str, create: bool) -> Result<Option<PathBuf>, String> {
        let path = parent.join(name);
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&path).map_err(|error| format!("Create model directory: {error}"))?;
                path.symlink_metadata().map_err(|error| format!("Inspect model directory: {error}"))?
            }
            Err(error) => return Err(format!("Inspect model directory: {error}")),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir()
            || path.canonicalize().ok().as_deref() != Some(path.as_path()) {
            return Err("The OCR model directory is not a regular private directory.".into());
        }
        Ok(Some(path))
    }

    pub(super) fn initialize(app_data: &Path) -> Result<(), String> {
        std::fs::create_dir_all(app_data).map_err(|error| format!("Create app data directory: {error}"))?;
        let app_data = app_data.canonicalize().map_err(|error| format!("Resolve app data directory: {error}"))?;
        let root = directory(&app_data, "ocr-models", true)?.ok_or("The OCR model store is unavailable.")?;
        if ROOT.get().is_some_and(|previous| previous != &root) {
            return Err("The OCR model store is already initialized at another location.".into());
        }
        let _ = ROOT.set(root.clone());
        // Best effort: an interrupted download must not consume disk forever,
        // but a busy second process must not prevent the application starting.
        if let Ok(_lock) = storage::lock_store(&root) {
            if let Err(error) = storage::cleanup_locked(&root) { eprintln!("OCR model cleanup: {error}"); }
        }
        Ok(())
    }
    pub(super) fn cache_dir() -> Option<PathBuf> { ROOT.get().cloned() }

    fn root() -> Result<&'static Path, String> {
        let path = ROOT.get().ok_or("The private OCR model store is unavailable. Restart the application and try again.")?;
        let meta = path.symlink_metadata().map_err(|error| format!("Inspect OCR model store: {error}"))?;
        if meta.file_type().is_symlink() || !meta.is_dir()
            || path.canonicalize().ok().as_ref() != Some(path) {
            return Err("The OCR model store no longer resolves to its private directory.".into());
        }
        Ok(path)
    }

    fn validate_artifact(file: &Artifact) -> Result<(), String> {
        let name = &file.filename;
        if name.is_empty() || name.len() > 240 || name.starts_with('.')
            || name.chars().any(|value| value.is_control() || matches!(value, '/' | '\\' | ':' | '\0'))
            || Path::new(name).components().count() != 1 {
            return Err("The model registry contains an unsafe artifact filename.".into());
        }
        if file.url.is_empty() {
            return Err(format!("No download is registered for {}. Select an existing local GGUF file or choose a downloadable model.", file.filename));
        }
        let url = reqwest::Url::parse(&file.url).map_err(|error| format!("Invalid model URL: {error}"))?;
        if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty()
            || url.password().is_some() || url.fragment().is_some() {
            return Err("Model downloads require an HTTPS registry URL without credentials or a fragment.".into());
        }
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|value| value.is_ascii_hexdigit()) {
            return Err(format!("No immutable SHA-256 pin is available for {}. Choose a pinned model or use a trusted local GGUF file.", file.filename));
        }
        Ok(())
    }

    fn validate_set(files: &[Artifact]) -> Result<(), String> {
        if files.is_empty() || files.len() > 9 || !files[0].primary
            || files.iter().skip(1).any(|file| file.primary) {
            return Err("The model registry declares an invalid artifact set.".into());
        }
        let mut seen = HashSet::new();
        for file in files {
            validate_artifact(file)?;
            if !seen.insert(file.filename.clone()) {
                return Err("The model registry repeats an artifact filename.".into());
            }
        }
        Ok(())
    }

    #[cfg(feature = "ocr-mobile")]
    fn artifacts(name: &str) -> Result<Vec<Artifact>, String> {
        crate::index::license_consent::ensure_registry_download_allowed(name).map_err(|error| error.to_string())?;
        let info = crispembed::CrispEmbed::model_download_info(name)?
            .ok_or_else(|| format!("'{name}' is not an exact registered model name. Select a model from Settings or use an existing local GGUF path."))?;
        if info.requires_license_acceptance && !crate::index::license_consent::is_accepted(&info.name) {
            return Err(format!("Model '{}' requires acceptance of its {} licence. Review the model in the CrispEmbed registry in Settings before downloading.", info.name, info.license));
        }
        let mut files = vec![Artifact {
            filename: info.primary.filename, url: info.primary.url,
            sha256: info.primary.sha256.unwrap_or_default(), primary: true,
        }];
        files.extend(info.companions.into_iter().map(|file| Artifact {
            filename: file.filename, url: file.url,
            sha256: file.sha256.unwrap_or_default(), primary: false,
        }));
        validate_set(&files)?;
        Ok(files)
    }

    fn model_directory(root: &Path, name: &str, create: bool) -> Result<Option<PathBuf>, String> {
        // Registry names are never interpreted as paths, including future names
        // containing punctuation. Each model owns its companion filenames.
        let key = format!("{:x}", Sha256::digest(name.as_bytes()));
        directory(root, &key, create)
    }

    fn regular_file(path: &Path, limit: u64) -> Result<Option<std::fs::Metadata>, String> {
        let meta = match path.symlink_metadata() {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("Inspect model artifact: {error}")),
        };
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err("The model artifact is not a regular file in app storage.".into());
        }
        if meta.len() == 0 || meta.len() > limit { return Ok(None); }
        Ok(Some(meta))
    }

    fn verify_file(dir: &Path, file: &Artifact) -> Result<bool, String> {
        let path = dir.join(&file.filename);
        let Some(meta) = regular_file(&path, MAX_MODEL_BYTES)? else { return Ok(false); };
        let stamp = (meta.len(), meta.modified().map_err(|error| format!("Inspect model timestamp: {error}"))?, file.sha256.to_ascii_lowercase());
        let cache = VERIFIED.get_or_init(|| Mutex::new(HashMap::new()));
        if cache.lock().map_err(|_| "Model verification cache is unavailable.")?.get(&path) == Some(&stamp) {
            return Ok(true);
        }
        let mut reader = std::fs::File::open(&path).map_err(|error| format!("Read model: {error}"))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        let mut header = Vec::new();
        loop {
            let count = reader.read(&mut buffer).map_err(|error| format!("Verify model: {error}"))?;
            if count == 0 { break; }
            total = total.checked_add(count as u64).ok_or("Model size overflow.")?;
            if total > MAX_MODEL_BYTES { return Ok(false); }
            if header.len() < 4 { header.extend_from_slice(&buffer[..count.min(4 - header.len())]); }
            hasher.update(&buffer[..count]);
        }
        if total != stamp.0 || (file.primary && header.as_slice() != b"GGUF")
            || format!("{:x}", hasher.finalize()) != stamp.2 { return Ok(false); }
        cache.lock().map_err(|_| "Model verification cache is unavailable.")?.insert(path, stamp);
        Ok(true)
    }

    fn installed(root: &Path, name: &str, files: &[Artifact]) -> Result<Option<PathBuf>, String> {
        validate_set(files)?;
        let Some(model_dir) = model_directory(root, name, false)? else { return Ok(None); };
        let pointer = model_dir.join("current.json");
        if regular_file(&pointer, MAX_MANIFEST_BYTES)?.is_none() { return Ok(None); }
        let mut bytes = Vec::new();
        std::fs::File::open(&pointer).map_err(|error| format!("Read model manifest: {error}"))?
            .take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)
            .map_err(|error| format!("Read model manifest: {error}"))?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES { return Ok(None); }
        let manifest: Manifest = match serde_json::from_slice(&bytes) {
            Ok(manifest) => manifest, Err(_) => return Ok(None),
        };
        if manifest.schema_version != 1 || manifest.model_name != name || manifest.files != files
            || uuid::Uuid::parse_str(&manifest.version).is_err()
            || manifest.version.len() != 36 { return Ok(None); }
        let Some(version) = directory(&model_dir, &manifest.version, false)? else { return Ok(None); };
        for file in files {
            if !verify_file(&version, file)? { return Ok(None); }
        }
        Ok(Some(version.join(&files[0].filename)))
    }

    #[cfg(feature = "ocr-mobile")]
    pub(super) fn resolve_installed(name: &str, retain: bool) -> Result<String, String> {
        let files = artifacts(name)?;
        let _install = INSTALL.lock().map_err(|_| "The model installer is unavailable.")?;
        let root = root()?;
        let _store = storage::lock_store(root)?;
        let path = installed(root, name, &files)?
            .ok_or_else(|| format!("Model '{name}' is not installed or failed its integrity check. Use Install basic OCR models for Tesseract, or the CrispEmbed model registry in Settings for other engines. Mobile recognition does not download models automatically."))?;
        // Registry readiness checks must not pin every installed model.
        if retain { storage::retain_path_locked(root, &path)?; }
        Ok(path.to_string_lossy().into_owned())
    }

    #[cfg(feature = "ocr-mobile")]
    pub(super) fn resolve_local(name: &str) -> Result<String, String> {
        let path = Path::new(name);
        // Preserve local-file support even before the private store is ready.
        let Some(root) = ROOT.get() else { return Ok(name.to_owned()); };
        let _install = INSTALL.lock().map_err(|_| "The model installer is unavailable.")?;
        let _store = storage::lock_store(root)?;
        storage::retain_path_locked(root, path)?;
        Ok(name.to_owned())
    }

    #[cfg(feature = "ocr-mobile")]
    pub(super) fn storage_report() -> Result<ModelStorageReport, String> {
        let _install = INSTALL.lock().map_err(|_| "The model installer is unavailable.")?;
        let root = root()?; let _store = storage::lock_store(root)?;
        storage::report_locked(root)
    }

    #[cfg(feature = "ocr-mobile")]
    pub(super) fn clean_storage() -> Result<ModelStorageCleanup, String> {
        let _install = INSTALL.lock().map_err(|_| "The model installer is unavailable.")?;
        let root = root()?; let _store = storage::lock_store(root)?;
        let result = storage::cleanup_locked(root)?;
        if let Some(cache) = VERIFIED.get() { if let Ok(mut cache) = cache.lock() { cache.retain(|path, _| path.is_file()); } }
        Ok(result)
    }

    #[cfg(feature = "ocr-mobile")]
    pub(super) fn remove_model(name: &str, version: &str) -> Result<ModelStorageCleanup, String> {
        let _install = INSTALL.lock().map_err(|_| "The model installer is unavailable.")?;
        let root = root()?; let _store = storage::lock_store(root)?;
        let result = storage::remove_model_locked(root, name, version)?;
        if let Some(cache) = VERIFIED.get() { if let Ok(mut cache) = cache.lock() { cache.retain(|path, _| path.is_file()); } }
        Ok(result)
    }

    /// Writes only inside a fresh staging directory. The published model set is
    /// unchanged on read, size, digest, disk, or format failure.
    fn write_stream(
        dir: &Path, file: &Artifact, mut reader: impl Read,
        expected_len: Option<u64>, limit: u64, mut progress: impl FnMut(u64),
    ) -> Result<u64, String> {
        validate_artifact(file)?;
        if expected_len.is_some_and(|length| length == 0 || length > limit) {
            return Err(format!("{} is empty or exceeds the mobile limit of 2 GiB per file. Choose a smaller model or use a local GGUF path.", file.filename));
        }
        let path = dir.join(&file.filename);
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|error| format!("Create model staging file: {error}"))?;
        let mut buffer = [0u8; 64 * 1024];
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut header = Vec::new();
        loop {
            let count = reader.read(&mut buffer).map_err(|error| format!("Model download interrupted: {error}"))?;
            if count == 0 { break; }
            total = total.checked_add(count as u64).ok_or("Model size overflow.")?;
            if total > limit { return Err("The model download exceeds the mobile limit of 2 GiB per file.".into()); }
            if header.len() < 4 { header.extend_from_slice(&buffer[..count.min(4 - header.len())]); }
            hasher.update(&buffer[..count]);
            output.write_all(&buffer[..count]).map_err(|error| format!("Write model download: {error}"))?;
            progress(total);
        }
        if total == 0 || expected_len.is_some_and(|length| length != total) {
            return Err("The model download is incomplete. Retry the download.".into());
        }
        if file.primary && header.as_slice() != b"GGUF" { return Err("The downloaded model is not a GGUF file.".into()); }
        if format!("{:x}", hasher.finalize()) != file.sha256.to_ascii_lowercase() {
            return Err(format!("SHA-256 verification failed for {}. The previous installation was preserved; retry or report the registry mismatch.", file.filename));
        }
        output.flush().map_err(|error| format!("Flush model: {error}"))?;
        output.sync_all().map_err(|error| format!("Sync model: {error}"))?;
        Ok(total)
    }

    fn commit_set(root: &Path, name: &str, files: &[Artifact], staging: tempfile::TempDir) -> Result<PathBuf, String> {
        validate_set(files)?;
        for file in files {
            if !verify_file(staging.path(), file)? { return Err("The staged model set is incomplete or corrupt.".into()); }
        }
        let model_dir = model_directory(root, name, true)?.ok_or("The model directory is unavailable.")?;
        let version = uuid::Uuid::new_v4().to_string();
        let destination = model_dir.join(&version);
        #[cfg(unix)]
        std::fs::File::open(staging.path()).and_then(|dir| dir.sync_all()).map_err(|error| format!("Sync staged models: {error}"))?;
        std::fs::rename(staging.path(), &destination).map_err(|error| format!("Commit model directory: {error}"))?;
        // The pointer is the only mutable record. Every version is immutable;
        // previous complete versions remain available if this commit fails.
        let pointer = model_dir.join("current.json");
        let _ = regular_file(&pointer, MAX_MANIFEST_BYTES)?;
        let manifest = Manifest { schema_version: 1, model_name: name.to_owned(), version, files: files.to_vec() };
        let mut output = tempfile::NamedTempFile::new_in(&model_dir).map_err(|error| format!("Create model manifest: {error}"))?;
        serde_json::to_writer(&mut output, &manifest).map_err(|error| format!("Write model manifest: {error}"))?;
        output.flush().map_err(|error| format!("Flush model manifest: {error}"))?;
        output.as_file().sync_all().map_err(|error| format!("Sync model manifest: {error}"))?;
        output.persist(&pointer).map_err(|error| format!("Commit model manifest: {}", error.error))?;
        #[cfg(unix)]
        {
            std::fs::File::open(&model_dir).and_then(|dir| dir.sync_all()).map_err(|error| format!("Sync model directory: {error}"))?;
            std::fs::File::open(root).and_then(|dir| dir.sync_all()).map_err(|error| format!("Sync model store: {error}"))?;
        }
        Ok(destination.join(&files[0].filename))
    }

    #[cfg(feature = "ocr-mobile")]
    fn offline() -> bool {
        ["CRISPEMBED_OFFLINE", "HF_HUB_OFFLINE"].iter().any(|key| {
            std::env::var(key).ok().is_some_and(|value| {
                matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
            })
        })
    }

    fn download_client_builder() -> reqwest::blocking::ClientBuilder {
        reqwest::blocking::Client::builder()
            .https_only(true).redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(Duration::from_secs(30)).timeout(Duration::from_secs(30 * 60))
    }

    #[cfg(any(target_os = "android", test))]
    fn with_android_roots(builder: reqwest::blocking::ClientBuilder) -> Result<reqwest::blocking::ClientBuilder, String> {
        // reqwest 0.13 otherwise uses rustls-platform-verifier, which needs a
        // separately initialized Android JVM/Kotlin component. The installer
        // uses the pinned Mozilla public root bundle instead. Rustls still
        // verifies the certificate chain, validity period, and server name;
        // no custom verifier or dangerous validation override is installed.
        // User/private Android trust anchors are not included in this client.
        let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter()
            .map(|certificate| reqwest::Certificate::from_der(certificate.as_ref()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Load bundled model-download trust roots: {error}"))?;
        if roots.is_empty() { return Err("The bundled model-download trust store is empty.".into()); }
        Ok(builder.tls_backend_rustls().tls_certs_only(roots))
    }

    #[cfg(feature = "ocr-mobile")]
    fn download_client() -> Result<reqwest::blocking::Client, String> {
        let builder = download_client_builder();
        #[cfg(target_os = "android")]
        let builder = with_android_roots(builder)?;
        builder.build().map_err(|error| format!("Initialize model download: {error}"))
    }

    #[cfg(feature = "ocr-mobile")]
    pub(super) fn download(name: &str, progress: impl Fn(DownloadProgress)) -> Result<String, String> {
        let files = artifacts(name)?;
        let _install = INSTALL.lock().map_err(|_| "The model installer is unavailable.")?;
        let root = root()?;
        // Shared with other app processes. It also makes incoming-* directories
        // safe to reclaim: no active download can coexist with cleanup.
        let _store = storage::lock_store(root)?;
        progress(DownloadProgress { name: name.to_owned(), filename: files[0].filename.clone(), phase: "checking", downloaded: 0, total: None, request_id: None });
        if let Some(path) = installed(root, name, &files)? {
            progress(DownloadProgress { name: name.to_owned(), filename: files[0].filename.clone(), phase: "complete", downloaded: 0, total: None, request_id: None });
            return Ok(path.to_string_lossy().into_owned());
        }
        if offline() { return Err(format!("Offline mode is enabled and '{name}' is not installed. Turn off offline mode to download, or use an existing local GGUF file.")); }
        let model_dir = model_directory(root, name, true)?.ok_or("The model directory is unavailable.")?;
        let staging = tempfile::Builder::new().prefix("incoming-").tempdir_in(&model_dir)
            .map_err(|error| format!("Create model staging directory: {error}"))?;
        let client = download_client()?;
        for file in &files {
            let emit = |phase, downloaded, total| progress(DownloadProgress {
                name: name.to_owned(), filename: file.filename.clone(), phase, downloaded, total, request_id: None,
            });
            let response = client.get(&file.url).header(reqwest::header::ACCEPT_ENCODING, "identity")
                .send().and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|error| format!("Download {}: {error}", file.filename))?;
            let total = response.content_length();
            emit("downloading", 0, total);
            let mut last = Instant::now();
            let bytes = write_stream(staging.path(), file, response, total, MAX_MODEL_BYTES, |downloaded| {
                if last.elapsed() >= Duration::from_millis(200) {
                    emit("downloading", downloaded, total); last = Instant::now();
                }
            })?;
            emit("verified", bytes, Some(bytes));
        }
        let path = commit_set(root, name, &files, staging)?;
        // Publishing a verified install remains successful if optional cleanup
        // cannot run. Installed and in-use generations are always protected.
        if let Err(error) = storage::cleanup_locked(root) { eprintln!("OCR model cleanup: {error}"); }
        progress(DownloadProgress { name: name.to_owned(), filename: files[0].filename.clone(), phase: "complete", downloaded: 0, total: None, request_id: None });
        Ok(path.to_string_lossy().into_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn android_root_bundle_builds_and_http_is_rejected_before_io() {
            let client = with_android_roots(download_client_builder())
                .unwrap().timeout(Duration::from_millis(200)).build().unwrap();
            // The real reqwest HTTPS-only gate runs before DNS or a socket.
            // A missing gate would instead return a connection error and fail
            // this assertion; the short timeout also bounds that regression.
            let error = client.get("http://127.0.0.1:9/model.gguf").send().unwrap_err();
            assert!(error.is_builder(), "HTTP was not rejected before I/O: {error}");
            assert_eq!(error.url().map(|url| url.scheme()), Some("http"));
        }
        fn artifact(bytes: &[u8]) -> Artifact {
            Artifact { filename: "ocr-test.gguf".into(), url: "https://example.com/ocr-test.gguf".into(),
                sha256: format!("{:x}", Sha256::digest(bytes)), primary: true }
        }
        fn stage(root: &Path, file: &Artifact, bytes: &[u8]) -> tempfile::TempDir {
            let dir = tempfile::tempdir_in(root).unwrap();
            write_stream(dir.path(), file, bytes, Some(bytes.len() as u64), 1024, |_| {}).unwrap();
            dir
        }
        #[test]
        fn complete_sets_publish_atomically_and_bad_replacements_preserve_previous_version() {
            let container = tempfile::tempdir().unwrap();
            let root = container.path().canonicalize().unwrap();
            let bytes = b"GGUF small fixture";
            let file = artifact(bytes);
            let files = vec![file.clone()];
            let initial = commit_set(&root, "test", &files, stage(&root, &file, bytes)).unwrap();
            assert_eq!(installed(&root, "test", &files).unwrap(), Some(initial.clone()));
            let bad = tempfile::tempdir_in(&root).unwrap();
            assert!(write_stream(bad.path(), &file, &b"GGUF corrupt"[..], None, 1024, |_| {}).unwrap_err().contains("SHA-256"));
            drop(bad);
            assert_eq!(installed(&root, "test", &files).unwrap(), Some(initial.clone()));
            assert_eq!(std::fs::read(initial).unwrap(), bytes);
        }
        #[test]
        fn missing_or_failed_companions_never_publish_a_model() {
            let container = tempfile::tempdir().unwrap();
            let root = container.path().canonicalize().unwrap();
            let bytes = b"GGUF primary";
            let primary = artifact(bytes);
            let companion_bytes = br#"{"tokenizer":"fixture"}"#;
            let mut companion = artifact(companion_bytes);
            companion.filename = "tokenizer.json".into(); companion.primary = false;
            let files = vec![primary.clone(), companion.clone()];
            let incomplete = stage(&root, &primary, bytes);
            assert!(commit_set(&root, "test", &files, incomplete).is_err());
            assert!(installed(&root, "test", &files).unwrap().is_none());
            let complete = stage(&root, &primary, bytes);
            write_stream(complete.path(), &companion, &companion_bytes[..], None, 1024, |_| {}).unwrap();
            commit_set(&root, "test", &files, complete).unwrap();
            assert!(installed(&root, "test", &files).unwrap().is_some());
            assert!(installed(&root, "another-model", &files).unwrap().is_none());
        }
        #[test]
        fn interrupted_oversized_incomplete_and_non_gguf_streams_fail() {
            struct Broken(bool);
            impl Read for Broken {
                fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                    if self.0 { return Err(std::io::Error::other("connection closed")); }
                    self.0 = true; buffer[..4].copy_from_slice(b"GGUF"); Ok(4)
                }
            }
            let bytes = b"GGUF fixture";
            let file = artifact(bytes);
            for kind in 0..4 {
                let dir = tempfile::tempdir().unwrap();
                let result = match kind {
                    0 => write_stream(dir.path(), &file, Broken(false), None, 1024, |_| {}),
                    1 => write_stream(dir.path(), &file, &bytes[..], None, 4, |_| {}),
                    2 => write_stream(dir.path(), &file, &bytes[..], Some(999), 1024, |_| {}),
                    _ => write_stream(dir.path(), &artifact(b"HTML error"), &b"HTML error"[..], None, 1024, |_| {}),
                };
                assert!(result.is_err());
            }
        }
        #[test]
        fn model_metadata_requires_pins_safe_names_and_https() {
            let original = artifact(b"GGUF fixture");
            for name in ["../outside.gguf", "a/b.gguf", "a\\b.gguf", ".hidden", "C:outside.gguf", ""] {
                let mut file = original.clone(); file.filename = name.into(); assert!(validate_artifact(&file).is_err());
            }
            for url in ["http://example.com/model", "file:///tmp/model", "https://user:pass@example.com/model", "https://example.com/model#part"] {
                let mut file = original.clone(); file.url = url.into(); assert!(validate_artifact(&file).is_err());
            }
            let mut unpinned = original; unpinned.sha256.clear();
            assert!(validate_artifact(&unpinned).unwrap_err().contains("immutable SHA-256"));
        }
        #[cfg(unix)]
        #[test]
        fn model_and_companion_symlinks_are_rejected_without_touching_targets() {
            let container = tempfile::tempdir().unwrap();
            let root = container.path().canonicalize().unwrap();
            let bytes = b"GGUF original";
            let file = artifact(bytes);
            let files = vec![file.clone()];
            let path = commit_set(&root, "test", &files, stage(&root, &file, bytes)).unwrap();
            let outside = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(outside.path(), bytes).unwrap();
            std::fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(outside.path(), &path).unwrap();
            assert!(installed(&root, "test", &files).unwrap_err().contains("regular file"));
            assert_eq!(std::fs::read(outside.path()).unwrap(), bytes);
            let dir = model_directory(&root, "other", true).unwrap().unwrap();
            std::fs::remove_dir(&dir).unwrap();
            std::os::unix::fs::symlink(outside.path().parent().unwrap(), &dir).unwrap();
            assert!(installed(&root, "other", &files).is_err());
        }
    }
}
