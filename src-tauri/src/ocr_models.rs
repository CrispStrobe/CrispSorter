//! Desktop OCR model manager: what a task needs, and an explicit installer.
//!
//! * [`plan`] lists every model a resolved OCR configuration loads, with its
//!   registry size, licence and install state. It never downloads.
//! * Recognition never downloads implicitly. Commands that run OCR call
//!   [`gate`] and return `MODEL_DOWNLOAD_REQUIRED:` + JSON while a
//!   downloadable model is missing; the UI asks, installs, and retries.
//! * [`install_models`] is the only desktop download path for OCR models:
//!   pinned SHA-256, a `<file>.part` that resumes over HTTP Range, a cancel
//!   flag checked in the stream loop, a free-space check, one lock per model
//!   (concurrent tasks share one download), the licence-consent gate, and an
//!   atomic rename into the shared cache ([`crate::model_cache::dir`]).
//!   Companions are committed before the primary file, so a model is never
//!   visible as installed without them.
//!
//! The mobile profile keeps its private versioned store
//! (`model_provisioning`), which this module delegates to.
#![cfg_attr(not(feature = "crispembed"), allow(dead_code))]

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::extractors::ocr_crispembed::ModelRequirement;
use crate::extractors::OcrPipelineConfig;
use crate::operation_log::OperationLog;

/// Progress events for [`install_models`].
pub const PROGRESS_EVENT: &str = "ocr-model-download";
/// Error marker of a download the user cancelled (the `.part` is kept).
pub const CANCELLED: &str = "OCR_MODEL_DOWNLOAD_CANCELLED";
/// Same prefix as preparation; the UI turns it into the download dialog.
pub use crate::ocr_workspace::MODEL_DOWNLOAD_REQUIRED;

/// Desktop downloads are bounded; the largest OCR VLMs are below this.
const MAX_MODEL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Head room kept free on the cache volume after a download.
const DISK_MARGIN: u64 = 256 * 1024 * 1024;

// ── Plan ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PlannedModel {
    pub name: String,
    pub purpose: String,
    pub engine: String,
    /// Registry estimate (or the local file size); `None` when unknown.
    pub size_bytes: Option<u64>,
    pub license: Option<String>,
    pub requires_consent: bool,
    pub consent_accepted: bool,
    /// A pinned HTTPS download is registered and the licence is reviewed.
    pub downloadable: bool,
    pub installed: bool,
    /// `false` for weights another engine fetches or the user places itself.
    pub managed: bool,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelPlan {
    pub engine: String,
    pub models: Vec<PlannedModel>,
    /// Names of managed, downloadable models that are not installed.
    pub missing: Vec<String>,
    pub missing_bytes: u64,
    /// Free bytes on the cache volume, when it can be measured.
    pub free_bytes: Option<u64>,
    pub offline: bool,
}

/// What the registry knows about one exact model name.
#[derive(Clone, Debug, Default)]
pub(crate) struct RegistryFacts {
    pub size_bytes: Option<u64>,
    pub license: Option<String>,
    pub downloadable: bool,
    pub note: Option<String>,
}

/// Parse CrispEmbed's free-text registry size ("13 MB", "2.3 GB", "976 KB").
/// Decimal units, as the registry is written from Hugging Face file sizes.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim().trim_start_matches(['~', '≈', '<', '>']).trim();
    let split = text.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = text.split_at(split);
    let value: f64 = number.trim().replace(',', ".").parse().ok()?;
    let scale = match unit.trim().to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "K" | "KB" => 1e3,
        "M" | "MB" => 1e6,
        "G" | "GB" => 1e9,
        "T" | "TB" => 1e12,
        "KIB" => 1024.0,
        "MIB" => 1024.0 * 1024.0,
        "GIB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    (value.is_finite() && value >= 0.0).then(|| (value * scale).round() as u64)
}

/// Pure plan builder: the facts and install state are injected so the
/// engine-to-model mapping is testable without the native registry.
pub(crate) fn plan_models(
    requirements: &[ModelRequirement],
    facts: &dyn Fn(&str) -> Option<RegistryFacts>,
    installed: &dyn Fn(&str) -> bool,
) -> Vec<PlannedModel> {
    use crate::index::license_consent as consent;
    requirements.iter().map(|req| {
        let base = PlannedModel {
            name: req.name.clone(), purpose: req.purpose.to_owned(), engine: req.engine.clone(),
            size_bytes: None, license: None, requires_consent: false, consent_accepted: false,
            downloadable: false, installed: false, managed: true, note: None,
        };
        if !consent::is_registry_name(&req.name) {
            let meta = std::fs::metadata(&req.name).ok().filter(|m| m.is_file());
            return PlannedModel {
                size_bytes: meta.as_ref().map(|m| m.len()), installed: meta.is_some(), managed: false,
                note: Some(if meta.is_some() { "Local model file.".into() } else { "The configured local model file does not exist.".into() }),
                ..base
            };
        }
        let licence = consent::license_for_registry_name(&req.name);
        let requires_consent = licence.requires_consent();
        let facts = facts(&req.name);
        let mut model = PlannedModel {
            installed: installed(&req.name),
            requires_consent,
            consent_accepted: requires_consent && consent::is_accepted(&req.name),
            ..base
        };
        match facts {
            Some(facts) => {
                model.size_bytes = facts.size_bytes;
                model.license = facts.license.or_else(|| Some(licence.label().to_owned()));
                model.downloadable = facts.downloadable && licence != consent::ModelLicense::Unknown;
                model.note = if licence == consent::ModelLicense::Unknown {
                    Some("This model's licence has not been reviewed, so it cannot be downloaded here.".into())
                } else { facts.note };
            }
            None => model.note = Some("This name is not in the model registry.".into()),
        }
        model
    }).collect()
}

/// Pinned non-registry models (PaddleOCR, ocrs) are managed on desktop; the
/// mobile profile keeps them out of the gate (its private store is registry
/// only), so they are listed but never block there.
fn builtins_managed() -> bool { !crate::model_provisioning::enabled() }

/// The planned entry of a pinned non-registry model.
pub(crate) fn planned_builtin(model: &crate::ocr_builtin_models::BuiltinModel, installed: bool) -> PlannedModel {
    PlannedModel {
        name: model.name.into(), purpose: model.purpose.into(), engine: model.engine.into(),
        size_bytes: Some(model.size()), license: Some(model.license.into()),
        requires_consent: false, consent_accepted: false, downloadable: true, installed,
        managed: builtins_managed(),
        note: (!builtins_managed()).then(|| "Install these weights in the engine's model folder on this device.".to_owned()),
    }
}

/// Models of engines outside the CrispEmbed registry (PaddleOCR, ocrs).
fn builtin_requirements(config: &OcrPipelineConfig, extras: &[String]) -> Vec<ModelRequirement> {
    use crate::ocr_builtin_models as builtin;
    let requirement = |name: &str| {
        let model = builtin::find(name).expect("builtin model name");
        ModelRequirement { name: model.name.into(), purpose: model.purpose, engine: model.engine.into() }
    };
    let mut list = Vec::new();
    match config.engine.as_deref().unwrap_or("auto") {
        "ocrs" => { list.push(requirement(builtin::OCRS_DET)); list.push(requirement(builtin::OCRS_REC)); }
        "paddle" if crate::extractors::ocr_paddle::is_paddle_ocr_available() => {
            let all = extras.iter().any(|e| e == "paddle_all");
            let cjk = extras.iter().any(|e| e == "paddle_cjk")
                || crate::extractors::ocr_policy::paddle_language(config, crate::extractors::OcrRecLang::Latin) == crate::extractors::OcrRecLang::Cjk;
            list.push(requirement(builtin::PADDLE_DET));
            if all { list.push(requirement(builtin::PADDLE_REC_EN)); list.push(requirement(builtin::PADDLE_REC_CH)); }
            else { list.push(requirement(builtin::paddle_recognizer(cjk))); }
            if all || extras.iter().any(|e| e == "paddle_tables") { list.push(requirement(builtin::PADDLE_SLANET)); }
        }
        _ => {}
    }
    list
}

/// Every model a resolved OCR configuration needs, plus explicit extras
/// (`kie`, `math`, `layout`) for specialist tools.
pub fn requirements(config: &OcrPipelineConfig, extras: &[String]) -> Vec<ModelRequirement> {
    #[cfg(feature = "crispembed")]
    let mut list = crate::extractors::ocr_crispembed::required_models(config);
    #[cfg(not(feature = "crispembed"))]
    let mut list: Vec<ModelRequirement> = { let _ = config; Vec::new() };
    for extra in extra_requirements(extras) {
        if !list.iter().any(|m| m.name == extra.name) { list.push(extra); }
    }
    if !cfg!(feature = "crispembed") { list.clear(); }
    // Not CrispEmbed weights: planned in every build that has the engine.
    list.extend(builtin_requirements(config, extras));
    list
}

fn extra_requirements(extras: &[String]) -> Vec<ModelRequirement> {
    let mut list: Vec<ModelRequirement> = Vec::new();
    let mut add = |name: &str, purpose: &'static str, engine: &str| {
        if !list.iter().any(|m| m.name == name) {
            list.push(ModelRequirement { name: name.into(), purpose, engine: engine.into() });
        }
    };
    for extra in extras {
        match extra.as_str() {
            "kie" => { add("dbnet-det", "text detection", "kie"); add("trocr-printed", "text recognition", "kie"); add("lilt-funsd", "key information extraction", "kie"); }
            "math" => add("ppformulanet-l", "formula recognition", "math"),
            "layout" => add("layout-heron", "layout detection", "layout"),
            _ => {}
        }
    }
    list
}

fn registry_facts(name: &str) -> Option<RegistryFacts> {
    if let Some(model) = crate::ocr_builtin_models::find(name) {
        return Some(RegistryFacts { size_bytes: Some(model.size()), license: Some(model.license.into()), downloadable: true, note: None });
    }
    native_registry_facts(name)
}

#[cfg(feature = "crispembed")]
fn native_registry_facts(name: &str) -> Option<RegistryFacts> {
    let row = crispembed::CrispEmbed::list_models().into_iter().find(|m| m.name == name)?;
    let info = crispembed::CrispEmbed::model_download_info(name).ok().flatten();
    let pinned = |file: &crispembed::ModelDownloadFile| !file.url.is_empty()
        && file.sha256.as_deref().is_some_and(|sha| sha.len() == 64);
    let (downloadable, note) = match &info {
        Some(info) if pinned(&info.primary) && info.companions.iter().all(pinned) => (true, None),
        Some(info) if info.primary.url.is_empty() => (false, Some(no_download_note(name))),
        Some(_) => (false, Some("No checksum pin is registered for this model, so it cannot be downloaded safely.".to_owned())),
        None => (false, None),
    };
    Some(RegistryFacts {
        size_bytes: parse_size(&row.size),
        license: Some(row.license).filter(|l| !l.is_empty()),
        downloadable, note,
    })
}
#[cfg(not(feature = "crispembed"))]
fn native_registry_facts(_name: &str) -> Option<RegistryFacts> { None }

/// Why a registered model cannot be downloaded, in words the dialog shows.
fn no_download_note(name: &str) -> String {
    if name.starts_with("easyocr-") {
        // No published GGUF exists (checked: CrispEmbed registry and the
        // `cstr/` Hugging Face repositories, 2026-11-12).
        return "EasyOCR's recognizer is a local conversion with no published download. Place easyocr-english-g2-f16.gguf in the model folder, or choose PP-OCRv6 or Tesseract.".into();
    }
    "No download is registered for this model. Place its GGUF file in the model folder, or choose another engine.".into()
}

pub fn is_installed(name: &str) -> bool {
    if crate::ocr_builtin_models::is_builtin(name) { return crate::ocr_builtin_models::locate(name).is_some(); }
    #[cfg(feature = "ocr-mobile")]
    { crate::model_provisioning::is_installed(name) }
    #[cfg(all(feature = "crispembed", not(feature = "ocr-mobile")))]
    { crate::model_cache::locate_registry(name).is_some() }
    #[cfg(not(feature = "crispembed"))]
    { let _ = name; false }
}

/// Free bytes on the volume holding `path` (or its nearest existing parent).
pub fn free_bytes(path: &Path) -> Option<u64> {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let mut probe = path.to_path_buf();
        while !probe.exists() { if !probe.pop() { return None; } }
        let target = probe.canonicalize().unwrap_or(probe);
        let disks = sysinfo::Disks::new_with_refreshed_list();
        disks.list().iter()
            .filter(|disk| target.starts_with(disk.mount_point()))
            .max_by_key(|disk| disk.mount_point().as_os_str().len())
            .map(|disk| disk.available_space())
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    { let _ = path; None }
}

fn summarize(engine: String, models: Vec<PlannedModel>) -> ModelPlan {
    let missing: Vec<&PlannedModel> = models.iter().filter(|m| needs_download(m)).collect();
    let missing_bytes = missing.iter().filter_map(|m| m.size_bytes).sum();
    let missing_names = missing.iter().map(|m| m.name.clone()).collect();
    let plan = ModelPlan {
        engine, missing: missing_names, missing_bytes,
        free_bytes: free_bytes(&crate::model_cache::dir()),
        offline: crate::model_cache::offline(), models,
    };
    let log = OperationLog::start("models.plan", json!({"engine": plan.engine}));
    log.succeed(json!({"models": plan.models.len(), "pending": plan.missing.len(), "bytes": plan.missing_bytes}));
    plan
}

fn needs_download(model: &PlannedModel) -> bool { model.managed && model.downloadable && !model.installed }

/// The plan for a policy-resolved configuration.
pub fn plan(config: &OcrPipelineConfig, extras: &[String]) -> ModelPlan {
    let engine = config.engine.clone().unwrap_or_else(|| "auto".into());
    let mut models = plan_models(&requirements(config, extras), &registry_facts, &is_installed);
    for model in &mut models {
        if crate::ocr_builtin_models::is_builtin(&model.name) { model.managed = builtins_managed(); }
    }
    summarize(engine, models)
}

/// The plan of specialist models alone, without a recognizer.
pub fn plan_extras(extras: &[String]) -> ModelPlan {
    let list = if cfg!(feature = "crispembed") { extra_requirements(extras) } else { Vec::new() };
    let engine = extras.first().cloned().unwrap_or_else(|| "tools".into());
    summarize(engine, plan_models(&list, &registry_facts, &is_installed))
}

/// `Err(MODEL_DOWNLOAD_REQUIRED + JSON)` while a downloadable model is
/// missing. Non-downloadable gaps are left to the engine's own error.
pub fn gate(models: &[PlannedModel]) -> Result<(), String> {
    let missing: Vec<&PlannedModel> = models.iter().filter(|m| needs_download(m)).collect();
    if missing.is_empty() { return Ok(()); }
    Err(format!("{MODEL_DOWNLOAD_REQUIRED}{}", serde_json::to_string(&missing).unwrap_or_else(|_| "[]".into())))
}

/// Gate a run, or (after the user agreed) install what is missing first.
/// Used by recognition, preparation and image extraction.
pub fn ensure_or_gate(models: &[PlannedModel], allow_downloads: bool, log: &OperationLog) -> Result<(), String> {
    let missing: Vec<String> = models.iter().filter(|m| needs_download(m)).map(|m| m.name.clone()).collect();
    if missing.is_empty() { return Ok(()); }
    if !allow_downloads {
        log.warn("model_consent_required", json!({"models": missing.len()}));
        return gate(models);
    }
    install_models(&missing, None, &|_| {}).map(|_| ())
}

// ── Installer ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize)]
pub struct InstallProgress {
    pub request_id: Option<String>,
    pub model: String,
    pub filename: String,
    /// 0-based position of `model` in the request, and the request size.
    pub index: usize,
    pub count: usize,
    pub downloaded: u64,
    pub total: Option<u64>,
    /// checking | waiting | downloading | verified | complete | failed | cancelled
    pub phase: &'static str,
    pub message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Artifact {
    pub filename: String,
    pub url: String,
    pub sha256: String,
    /// Committed last: its presence marks the model installed.
    pub primary: bool,
    /// Check the GGUF magic (CrispEmbed weights); ONNX/text files rely on SHA-256.
    pub gguf: bool,
}

/// One HTTP(S) response body, opened at a byte offset.
pub(crate) struct FetchResponse {
    pub body: Box<dyn Read + Send>,
    /// The server honoured the range request (206).
    pub resumed: bool,
    /// Complete file length when known.
    pub total: Option<u64>,
    /// 416: the offset is at or beyond the end of the file.
    pub unsatisfiable: bool,
}

pub(crate) trait Fetch: Sync {
    fn open(&self, url: &str, offset: u64) -> Result<FetchResponse, String>;
}

fn validate_artifact(file: &Artifact) -> Result<(), String> {
    let name = &file.filename;
    if name.is_empty() || name.len() > 240 || name.starts_with('.')
        || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '\0'))
        || Path::new(name).components().count() != 1 {
        return Err("The model registry contains an unsafe file name.".into());
    }
    if file.url.is_empty() {
        return Err(format!("No download is registered for {name}."));
    }
    let url = reqwest::Url::parse(&file.url).map_err(|_| format!("The registered download address of {name} is invalid."))?;
    if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err("Model downloads require an HTTPS address without credentials.".into());
    }
    if file.sha256.len() != 64 || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("No SHA-256 pin is registered for {name}, so it cannot be downloaded safely."));
    }
    Ok(())
}

fn part_path(dir: &Path, file: &Artifact) -> PathBuf { dir.join(format!("{}.part", file.filename)) }

fn hash_prefix(path: &Path, length: u64, hasher: &mut Sha256) -> Result<(), String> {
    let mut reader = std::fs::File::open(path).map_err(|e| format!("Read partial download: {e}"))?.take(length);
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|e| format!("Read partial download: {e}"))?;
        if count == 0 { return Ok(()); }
        hasher.update(&buffer[..count]);
    }
}

/// Download one artifact into `<dir>/<filename>.part`, resuming from an
/// existing partial file, and verify it. The caller renames it into place.
/// Network failures and cancellation keep the `.part` for the next attempt;
/// a checksum mismatch deletes it.
pub(crate) fn download_artifact(
    fetch: &dyn Fetch, dir: &Path, file: &Artifact, cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<u64, String> {
    download_artifact_logged(fetch, dir, file, cancel, progress, None)
}

fn download_artifact_logged(
    fetch: &dyn Fetch, dir: &Path, file: &Artifact, cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>), log: Option<&OperationLog>,
) -> Result<u64, String> {
    validate_artifact(file)?;
    let part = part_path(dir, file);
    let mut offset = match part.symlink_metadata() {
        Ok(meta) if meta.is_file() => meta.len(),
        Ok(_) => return Err("A partial model download is not a regular file. Remove it and retry.".into()),
        Err(_) => 0,
    };
    if offset > MAX_MODEL_BYTES { offset = 0; }
    let mut hasher = Sha256::new();
    if offset > 0 { hash_prefix(&part, offset, &mut hasher)?; }
    if cancel.load(Ordering::SeqCst) { return Err(CANCELLED.into()); }
    let mut response = fetch.open(&file.url, offset)?;
    let mut already_complete = false;
    if response.unsatisfiable {
        if offset > 0 && format!("{:x}", hasher.clone().finalize()) == file.sha256.to_ascii_lowercase() {
            already_complete = true;
        } else {
            if let Some(log) = log { log.warn("resume_restarted", json!({"bytes": offset})); }
            offset = 0; hasher = Sha256::new();
            response = fetch.open(&file.url, 0)?;
        }
    } else if offset > 0 && !response.resumed {
        // The server ignored the range: it is sending the whole file.
        if let Some(log) = log { log.warn("resume_restarted", json!({"bytes": offset})); }
        offset = 0; hasher = Sha256::new();
    } else if offset > 0 {
        if let Some(log) = log { log.step("resuming", json!({"bytes": offset})); }
    }
    if response.total.is_some_and(|total| total == 0 || total > MAX_MODEL_BYTES) {
        return Err(format!("{} is empty or larger than the 16 GiB download limit.", file.filename));
    }
    let mut output = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&part)
        .map_err(|e| format!("Create model download: {e}"))?;
    output.set_len(offset).map_err(|e| format!("Prepare model download: {e}"))?;
    output.seek(SeekFrom::Start(offset)).map_err(|e| format!("Prepare model download: {e}"))?;
    let mut done = offset;
    let total = response.total;
    progress(done, total);
    if !already_complete {
        let mut buffer = vec![0u8; 256 * 1024];
        loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = output.flush();
                return Err(CANCELLED.into());
            }
            let count = response.body.read(&mut buffer)
                .map_err(|e| format!("The model download was interrupted ({e}). Retry to resume it."))?;
            if count == 0 { break; }
            done = done.checked_add(count as u64).ok_or("Model size overflow.")?;
            if done > MAX_MODEL_BYTES || total.is_some_and(|t| done > t) {
                drop(output); let _ = std::fs::remove_file(&part);
                return Err(format!("{} is larger than announced; the download was discarded.", file.filename));
            }
            hasher.update(&buffer[..count]);
            output.write_all(&buffer[..count]).map_err(|e| format!("Write model download: {e}"))?;
            progress(done, total);
        }
    }
    output.flush().map_err(|e| format!("Flush model download: {e}"))?;
    if done == 0 || total.is_some_and(|t| t != done) {
        return Err(format!("The download of {} is incomplete. Retry to resume it.", file.filename));
    }
    if format!("{:x}", hasher.finalize()) != file.sha256.to_ascii_lowercase() {
        drop(output); let _ = std::fs::remove_file(&part);
        return Err(format!("SHA-256 verification failed for {}. The download was discarded; retry, or report the registry mismatch.", file.filename));
    }
    if file.primary && file.gguf {
        let mut magic = [0u8; 4];
        let ok = std::fs::File::open(&part).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"GGUF";
        if !ok { drop(output); let _ = std::fs::remove_file(&part); return Err(format!("{} is not a GGUF model.", file.filename)); }
    }
    output.sync_all().map_err(|e| format!("Sync model download: {e}"))?;
    Ok(done)
}

/// Move a verified `.part` into place. Same directory, so the rename is atomic.
fn commit_artifact(dir: &Path, file: &Artifact) -> Result<PathBuf, String> {
    let destination = dir.join(&file.filename);
    #[cfg(windows)]
    if destination.exists() { std::fs::remove_file(&destination).map_err(|e| format!("Replace model: {e}"))?; }
    std::fs::rename(part_path(dir, file), &destination).map_err(|e| format!("Install model: {e}"))?;
    #[cfg(unix)]
    { let _ = std::fs::File::open(dir).and_then(|d| d.sync_all()); }
    Ok(destination)
}

fn model_lock(name: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    locks.entry(name.to_owned()).or_default().clone()
}

/// How often a process waiting for another process's download re-checks.
const LOCK_POLL: Duration = Duration::from_millis(250);

/// Cross-process lock on one model's files: `<primary>.lock` next to the
/// target, so the app and the CLI never write the same `.part`. The lock is
/// released when the returned file is dropped (or the process dies). While
/// another process holds it, this one waits (cancellable) and is told so.
fn lock_model_files(dir: &Path, primary: &Artifact, cancel: &AtomicBool, waiting: &mut dyn FnMut()) -> Result<std::fs::File, String> {
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(dir.join(format!("{}.lock", primary.filename)))
        .map_err(|e| format!("Open the model download lock: {e}"))?;
    let mut announced = false;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                if !announced { waiting(); announced = true; }
                if cancel.load(Ordering::SeqCst) { return Err(CANCELLED.into()); }
                std::thread::sleep(LOCK_POLL);
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(format!("Lock the model download: {error}")),
        }
    }
}

fn cancels() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static CANCELS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    CANCELS.get_or_init(Default::default)
}

/// Request cancellation of an install. `true` when the request was running.
pub fn cancel(request_id: &str) -> bool {
    let cancels = cancels().lock().unwrap_or_else(|e| e.into_inner());
    let found = cancels.get(request_id).map(|flag| flag.store(true, Ordering::SeqCst)).is_some();
    let log = OperationLog::start("models.cancel", json!({}));
    if found { log.succeed(json!({"accepted": true})); } else { log.succeed(json!({"accepted": false})); }
    found
}

/// A model to install: its exact name, registry estimate and artifacts
/// (primary first).
pub(crate) struct InstallItem {
    pub name: String,
    pub estimate: Option<u64>,
    pub files: Vec<Artifact>,
}

fn present(dir: &Path, files: &[Artifact]) -> bool {
    files.iter().all(|file| crate::model_cache::usable_file(&dir.join(&file.filename)))
}

/// Bytes still to fetch, net of resumable partial files.
fn remaining_bytes(dir: &Path, items: &[InstallItem]) -> u64 {
    items.iter().filter(|item| !present(dir, &item.files)).map(|item| {
        let partial: u64 = item.files.iter()
            .filter_map(|file| std::fs::metadata(part_path(dir, file)).ok().map(|m| m.len())).sum();
        item.estimate.unwrap_or(0).saturating_sub(partial)
    }).sum()
}

/// Core installer, independent of the registry and the network for tests.
/// Every step is recorded on `log` with fixed codes and counts only.
pub(crate) fn install_with(
    fetch: &dyn Fetch, dir: &Path, items: &[InstallItem], cancel: &AtomicBool,
    free: Option<u64>, request_id: Option<&str>, emit: &dyn Fn(InstallProgress), log: &OperationLog,
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Create the model folder: {e}"))?;
    let needed = remaining_bytes(dir, items);
    if let Some(free) = free {
        if needed.saturating_add(DISK_MARGIN) > free {
            log.warn("insufficient_space", json!({"bytes": needed}));
            return Err(format!("Not enough disk space for the OCR models: {} MB needed, {} MB free. Free some space and retry.",
                needed.div_ceil(1_000_000), free / 1_000_000));
        }
    }
    let count = items.len();
    let mut installed = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let primary = item.files.iter().find(|f| f.primary).or(item.files.first()).ok_or("The model registry declares no file.")?;
        let event = |phase: &'static str, filename: &str, downloaded: u64, total: Option<u64>, message: Option<String>| emit(InstallProgress {
            request_id: request_id.map(str::to_owned), model: item.name.clone(), filename: filename.to_owned(),
            index, count, downloaded, total, phase, message,
        });
        event("checking", &primary.filename, 0, item.estimate, None);
        log.step("checking", json!({"count": index + 1, "total": count}));
        // Same process: concurrent tasks wait here, then find it installed.
        let lock = model_lock(&item.name);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        // Other processes (app + CLI): a lock file next to the target.
        let _file_lock = lock_model_files(dir, primary, cancel, &mut || {
            log.step("lock_wait", json!({"count": index + 1}));
            event("waiting", &primary.filename, 0, item.estimate, Some("Another CrispSorter process is downloading this model.".into()));
        }).inspect_err(|error| {
            if error == CANCELLED { event("cancelled", &primary.filename, 0, None, None); }
        })?;
        if present(dir, &item.files) {
            event("complete", &primary.filename, item.estimate.unwrap_or(0), item.estimate, None);
            log.step("already_installed", json!({"count": index + 1}));
            installed.push(dir.join(&primary.filename));
            continue;
        }
        // Companions first: the primary file is what marks a model installed.
        let mut order: Vec<&Artifact> = item.files.iter().filter(|f| !f.primary).collect();
        order.extend(item.files.iter().filter(|f| f.primary));
        let mut model_bytes = 0u64;
        for file in order {
            if crate::model_cache::usable_file(&dir.join(&file.filename)) && !file.primary { continue; }
            let mut last = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
            log.step("downloading", json!({"count": index + 1}));
            let result = download_artifact_logged(fetch, dir, file, cancel, &mut |downloaded, total| {
                if last.elapsed() >= Duration::from_millis(150) {
                    event("downloading", &file.filename, downloaded, total, None);
                    last = Instant::now();
                }
            }, Some(log));
            match result {
                Ok(bytes) => {
                    model_bytes += bytes;
                    event("verified", &file.filename, bytes, Some(bytes), None);
                    log.step("verified", json!({"bytes": bytes}));
                    commit_artifact(dir, file)?;
                    log.step("committed", json!({"count": index + 1}));
                }
                Err(error) => {
                    let phase = if error == CANCELLED { "cancelled" } else { "failed" };
                    event(phase, &file.filename, 0, None, (phase == "failed").then(|| error.clone()));
                    return Err(error);
                }
            }
        }
        event("complete", &primary.filename, model_bytes, Some(model_bytes), None);
        log.step("model_complete", json!({"count": index + 1, "bytes": model_bytes}));
        installed.push(dir.join(&primary.filename));
    }
    Ok(installed)
}

struct HttpFetch(reqwest::blocking::Client);

impl HttpFetch {
    fn new() -> Result<Self, String> {
        reqwest::blocking::Client::builder()
            .https_only(true).redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(Duration::from_secs(30)).timeout(Duration::from_secs(6 * 60 * 60))
            .build().map(Self).map_err(|e| format!("Initialize model download: {e}"))
    }
}

fn content_range_total(value: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    value?.to_str().ok()?.rsplit('/').next()?.trim().parse().ok()
}

impl Fetch for HttpFetch {
    fn open(&self, url: &str, offset: u64) -> Result<FetchResponse, String> {
        use reqwest::header::{ACCEPT_ENCODING, CONTENT_RANGE, RANGE};
        use reqwest::StatusCode;
        let mut request = self.0.get(url).header(ACCEPT_ENCODING, "identity");
        if offset > 0 { request = request.header(RANGE, format!("bytes={offset}-")); }
        let response = request.send().map_err(|e| format!("Model download failed: {}", e.without_url()))?;
        let status = response.status();
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            return Ok(FetchResponse { body: Box::new(std::io::empty()), resumed: false, total: None, unsatisfiable: true });
        }
        if !status.is_success() {
            return Err(format!("Model download failed: the server answered HTTP {}.", status.as_u16()));
        }
        let resumed = offset > 0 && status == StatusCode::PARTIAL_CONTENT;
        let total = if resumed { content_range_total(response.headers().get(CONTENT_RANGE)) } else { response.content_length() };
        Ok(FetchResponse { body: Box::new(response), resumed, total, unsatisfiable: false })
    }
}

/// The artifacts of one model: pinned builtins, or the CrispEmbed registry.
fn install_item(name: &str) -> Result<InstallItem, String> {
    crate::index::license_consent::ensure_registry_download_allowed(name).map_err(|e| e.to_string())?;
    if let Some(model) = crate::ocr_builtin_models::find(name) {
        let files: Vec<Artifact> = model.files.iter().enumerate().map(|(i, file)| Artifact {
            filename: file.filename.into(), url: file.url.into(), sha256: file.sha256.into(), primary: i == 0, gguf: false,
        }).collect();
        for file in &files { validate_artifact(file)?; }
        return Ok(InstallItem { name: name.to_owned(), estimate: Some(model.size()), files });
    }
    registry_install_item(name)
}

#[cfg(feature = "crispembed")]
fn registry_install_item(name: &str) -> Result<InstallItem, String> {
    let info = crispembed::CrispEmbed::model_download_info(name)?
        .ok_or_else(|| format!("'{name}' is not a registered model name."))?;
    let artifact = |file: crispembed::ModelDownloadFile, primary: bool| Artifact {
        filename: file.filename, url: file.url, sha256: file.sha256.unwrap_or_default().to_ascii_lowercase(), primary, gguf: true,
    };
    let mut files = vec![artifact(info.primary, true)];
    files.extend(info.companions.into_iter().map(|file| artifact(file, false)));
    if files.iter().any(|f| f.url.is_empty()) { return Err(no_download_note(name)); }
    for file in &files { validate_artifact(file)?; }
    Ok(InstallItem { name: name.to_owned(), estimate: registry_facts(name).and_then(|f| f.size_bytes), files })
}
#[cfg(not(feature = "crispembed"))]
fn registry_install_item(name: &str) -> Result<InstallItem, String> {
    Err(format!("'{name}' needs a build with CrispEmbed."))
}

/// Stable failure code of an installer error (logs carry codes, not text).
pub(crate) fn failure_code(error: &str) -> &'static str {
    if error == CANCELLED { "cancelled" }
    else if error.starts_with("Not enough disk space") { "insufficient_space" }
    else if error.contains("SHA-256 verification") { "checksum_mismatch" }
    else if error.starts_with("Offline mode") { "offline" }
    else if error.contains("licen") { "consent_required" }
    else if error.contains("not a GGUF") { "format_mismatch" }
    else if error.contains("interrupted") || error.contains("incomplete") { "interrupted" }
    else { "download_failed" }
}

/// Install models explicitly (after the user agreed). Already installed
/// models are skipped. Emits [`InstallProgress`] through `emit`.
pub fn install_models(names: &[String], request_id: Option<&str>, emit: &(dyn Fn(InstallProgress) + Sync)) -> Result<Vec<String>, String> {
    let log = OperationLog::start("models.download", json!({"models": names.len()}));
    let flag = Arc::new(AtomicBool::new(false));
    if let Some(id) = request_id {
        cancels().lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_owned(), flag.clone());
    }
    let result = install_models_inner(names, request_id, &flag, emit, &log);
    if let Some(id) = request_id { cancels().lock().unwrap_or_else(|e| e.into_inner()).remove(id); }
    finish_install(&log, &result);
    result
}

/// The terminal receipt of an install: completed, cancelled, or a fixed code.
fn finish_install<T: AsRef<[U]>, U>(log: &OperationLog, result: &Result<T, String>) {
    match result {
        Ok(paths) => log.succeed(json!({"models": paths.as_ref().len()})),
        Err(error) if error == CANCELLED => log.cancel(),
        Err(error) => log.fail(failure_code(error)),
    }
}

fn install_models_inner(
    names: &[String], request_id: Option<&str>, flag: &AtomicBool,
    emit: &(dyn Fn(InstallProgress) + Sync), log: &OperationLog,
) -> Result<Vec<String>, String> {
    if names.is_empty() || names.len() > 32 { return Err("Choose between 1 and 32 models to install.".into()); }
    #[cfg(feature = "ocr-mobile")]
    {
        let _ = (flag, log);
        let count = names.len();
        let mut paths = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let path = crate::model_provisioning::download(name, |update| emit(InstallProgress {
                request_id: request_id.map(str::to_owned), model: update.name, filename: update.filename,
                index, count, downloaded: update.downloaded, total: update.total, phase: update.phase, message: None,
            }))?;
            paths.push(path);
        }
        return Ok(paths);
    }
    #[cfg(not(feature = "ocr-mobile"))]
    {
        let dir = crate::model_cache::dir();
        let mut items = Vec::new();
        for name in names {
            if is_installed(name) {
                emit(InstallProgress { request_id: request_id.map(str::to_owned), model: name.clone(), filename: String::new(),
                    index: 0, count: names.len(), downloaded: 0, total: None, phase: "complete", message: None });
                continue;
            }
            items.push(install_item(name)?);
        }
        log.step("planned", json!({"models": items.len(), "bytes": remaining_bytes(&dir, &items)}));
        if items.is_empty() { return Ok(Vec::new()); }
        if crate::model_cache::offline() {
            return Err(format!("Offline mode is enabled and {} model(s) are not installed: {}. Turn off offline mode, or copy the model files into the model folder.",
                items.len(), items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>().join(", ")));
        }
        #[cfg(feature = "crispembed")]
        {
            if let Some(problem) = crate::extractors::ocr_crispembed::cache_problem() { return Err(problem); }
        }
        let fetch = HttpFetch::new()?;
        let paths = install_with(&fetch, &dir, &items, flag, free_bytes(&dir), request_id, emit, log)?;
        Ok(paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect())
    }
}

/// Remove an installed model from every cache directory.
pub fn remove_model(name: &str) -> Result<u64, String> {
    let log = OperationLog::start("models.remove", json!({"models": 1}));
    let result = remove_model_inner(name);
    match &result { Ok(freed) => log.succeed(json!({"bytes": freed})), Err(_) => log.fail("remove_refused") }
    result
}

fn remove_model_inner(name: &str) -> Result<u64, String> {
    if !crate::index::license_consent::is_registry_name(name) { return Err("Only registry models can be removed here.".into()); }
    if crate::model_provisioning::enabled() { return Err("Use model storage to remove models in this build.".into()); }
    let files: Vec<String> = match crate::ocr_builtin_models::find(name) {
        Some(model) => model.files.iter().map(|f| f.filename.to_owned()).collect(),
        None => registry_file_names(name).ok_or("This name is not in the model registry.")?,
    };
    let lock = model_lock(name);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut freed = 0u64;
    for dir in crate::model_cache::search_dirs() {
        for file in &files {
            for path in [dir.join(file), dir.join(format!("{file}.part"))] {
                if let Ok(meta) = path.symlink_metadata() {
                    if meta.is_file() && std::fs::remove_file(&path).is_ok() { freed += meta.len(); }
                }
            }
        }
    }
    Ok(freed)
}

#[cfg(feature = "crispembed")]
fn registry_file_names(name: &str) -> Option<Vec<String>> { crate::model_cache::registry_files(name) }
#[cfg(not(feature = "crispembed"))]
fn registry_file_names(_name: &str) -> Option<Vec<String>> { None }

// ── Tauri commands ───────────────────────────────────────────────────────────

/// A plan request: the same recognition options the run will send.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlanRequest {
    /// `workspace` (OCR workbench: an explicit config replaces saved options)
    /// or `extract` (image/PDF extraction: overlays the saved options).
    pub surface: Option<String>,
    pub config: Option<OcrPipelineConfig>,
    pub engine: Option<String>,
    pub language: Option<String>,
    pub cpu_pixel_budget: Option<bool>,
    pub ocr_tier: Option<String>,
    pub ocr_rec_lang: Option<String>,
    /// Specialist tools: `kie`, `math`, `layout`.
    pub extras: Vec<String>,
    /// Plan only the specialist models (e.g. formula OCR), no recognizer.
    pub extras_only: bool,
}

fn validate_extras(extras: &[String]) -> Result<(), String> {
    if extras.len() > 8 || extras.iter().any(|e| !["kie", "math", "layout"].contains(&e.as_str())) {
        return Err("Unknown OCR model extra.".into());
    }
    Ok(())
}

pub(crate) fn resolve_request(saved: crate::extractors::ExtractOptions, request: PlanRequest) -> Result<(OcrPipelineConfig, Vec<String>), String> {
    validate_extras(&request.extras)?;
    let workspace = request.surface.as_deref() == Some("workspace");
    let mut options = if workspace && request.config.is_some() { crate::extractors::ExtractOptions::default() } else { saved };
    let overrides = crate::ocr_extract::RecognitionOptions {
        engine: request.engine, language: request.language, config: request.config,
        cpu_pixel_budget: request.cpu_pixel_budget, ocr_tier: request.ocr_tier, ocr_rec_lang: request.ocr_rec_lang,
        allow_downloads: None,
    };
    options = crate::ocr_extract::extraction_options(options, overrides, "")?;
    let (config, _) = crate::extractors::ocr_policy::resolve_extraction_config(&options)?;
    let mut extras = request.extras;
    // PaddleOCR's script choice lives in the extraction options, not the config.
    if options.ocr_rec_lang == crate::extractors::OcrRecLang::Cjk { extras.push("paddle_cjk".into()); }
    Ok((config, extras))
}

#[tauri::command]
pub async fn ocr_models_required(state: tauri::State<'_, crate::AppState>, request: Option<PlanRequest>) -> Result<ModelPlan, String> {
    let request = request.unwrap_or_default();
    if request.extras_only {
        validate_extras(&request.extras)?;
        let extras = request.extras;
        return tokio::task::spawn_blocking(move || plan_extras(&extras)).await.map_err(|e| format!("Model plan: {e}"));
    }
    let saved = state.bg_ingest.lock().await.ocr_options();
    let (config, extras) = resolve_request(saved, request)?;
    // `plan` records its own `models.plan` receipt.
    tokio::task::spawn_blocking(move || plan(&config, &extras)).await.map_err(|e| format!("Model plan: {e}"))
}

#[derive(Clone, Debug, Serialize)]
pub struct EngineModels {
    pub engine: String,
    pub models: Vec<PlannedModel>,
}

/// Per engine, the default models it needs (Settings > OCR models).
#[tauri::command]
pub async fn ocr_models_catalog() -> Result<Vec<EngineModels>, String> {
    tokio::task::spawn_blocking(|| {
        use crate::extractors::ocr_policy::{EMBEDDED_ENGINES, LEGACY_ENGINES};
        let mut out = Vec::new();
        for engine in EMBEDDED_ENGINES.iter().chain(LEGACY_ENGINES.iter()) {
            if ["unified", "unlimited_ocr2", "minicpm_ocr", "system_tesseract"].contains(engine) { continue; }
            let Ok((config, _)) = crate::extractors::ocr_policy::recognition_config(OcrPipelineConfig::default(), Some(*engine), None) else { continue };
            // Settings offers every PaddleOCR file: both scripts and tables.
            let extras: Vec<String> = if *engine == "paddle" { vec!["paddle_all".into()] } else { Vec::new() };
            let plan = plan(&config, &extras);
            if !plan.models.is_empty() { out.push(EngineModels { engine: (*engine).to_owned(), models: plan.models }); }
        }
        out
    }).await.map_err(|e| format!("Model catalog: {e}"))
}

#[tauri::command]
pub async fn ocr_models_install(
    app: tauri::AppHandle, names: Vec<String>, request_id: String, accept_licenses: Option<Vec<String>>,
) -> Result<Vec<String>, String> {
    use tauri::Emitter;
    uuid::Uuid::parse_str(&request_id).map_err(|_| "The model installation request ID is invalid.")?;
    if names.iter().any(|name| !crate::index::license_consent::is_registry_name(name)) {
        return Err("Only registry models can be downloaded.".into());
    }
    // Consent given in the download dialog, for exactly these models.
    for name in accept_licenses.unwrap_or_default() {
        if names.contains(&name) { crate::index::license_consent::accept(&name); }
    }
    tokio::task::spawn_blocking(move || {
        install_models(&names, Some(request_id.as_str()), &|update| { let _ = app.emit(PROGRESS_EVENT, update); })
    }).await.map_err(|e| format!("Model installer: {e}"))?
}

#[tauri::command]
pub fn ocr_models_cancel(request_id: String) -> bool { cancel(&request_id) }

#[tauri::command]
pub async fn ocr_models_remove(name: String) -> Result<u64, String> {
    tokio::task::spawn_blocking(move || remove_model(&name)).await.map_err(|e| format!("Remove model: {e}"))?
}

// ── CLI ──────────────────────────────────────────────────────────────────────

static CLI_ASSUME_YES: AtomicBool = AtomicBool::new(false);

/// `--yes` / `--download-models`: install missing OCR models without asking.
pub fn set_cli_assume_yes(yes: bool) { CLI_ASSUME_YES.store(yes, Ordering::SeqCst); }

fn human(bytes: u64) -> String {
    if bytes >= 1_000_000_000 { format!("{:.1} GB", bytes as f64 / 1e9) }
    else if bytes >= 1_000_000 { format!("{:.0} MB", bytes as f64 / 1e6) }
    else { format!("{:.0} KB", (bytes as f64 / 1e3).max(1.0)) }
}

/// Text table of a plan for stderr.
pub fn describe_plan(plan: &ModelPlan) -> String {
    let mut text = String::new();
    for model in &plan.models {
        let state = if model.installed { "installed" } else if needs_download(model) { "missing" } else { "unavailable" };
        text.push_str(&format!("  {:<28} {:<28} {:>9}  {:<12} {}{}\n", model.name, model.purpose,
            model.size_bytes.map(human).unwrap_or_else(|| "?".into()), state,
            model.license.as_deref().unwrap_or("?"), if model.requires_consent { " (licence consent required)" } else { "" }));
    }
    text
}

fn cli_progress(update: InstallProgress) {
    let total = update.total.map(human).unwrap_or_else(|| "?".into());
    match update.phase {
        "downloading" => eprint!("\r  [{}/{}] {} {} / {}        ", update.index + 1, update.count, update.model, human(update.downloaded), total),
        "complete" => eprintln!("\r  [{}/{}] {} ready                              ", update.index + 1, update.count, update.model),
        "failed" => eprintln!("\r  [{}/{}] {} failed: {}", update.index + 1, update.count, update.model, update.message.unwrap_or_default()),
        "cancelled" => eprintln!("\r  [{}/{}] {} cancelled", update.index + 1, update.count, update.model),
        "waiting" => eprintln!("\r  [{}/{}] {} is being downloaded by another CrispSorter process; waiting", update.index + 1, update.count, update.model),
        _ => {}
    }
}

/// Before CLI OCR: print the plan; install after `--yes` or a TTY `[y/N]`;
/// otherwise fail listing the missing models.
pub fn cli_ensure(config: &OcrPipelineConfig, extras: &[String]) -> Result<(), String> {
    cli_ensure_plan(plan(config, extras))
}

/// Specialist tools that load only their own models (`math`, `kie`, ...).
pub fn cli_ensure_extras(extras: &[String]) -> Result<(), String> {
    validate_extras(extras)?;
    cli_ensure_plan(plan_extras(extras))
}

fn cli_ensure_plan(plan: ModelPlan) -> Result<(), String> {
    use std::io::IsTerminal;
    if plan.missing.is_empty() { return Ok(()); }
    let log = OperationLog::start("models.cli_check", json!({"surface": "cli", "models": plan.missing.len(), "bytes": plan.missing_bytes}));
    eprintln!("OCR with '{}' needs {} model(s) that are not installed ({}):", plan.engine, plan.missing.len(), human(plan.missing_bytes));
    eprint!("{}", describe_plan(&plan));
    let decision = cli_decision(&plan, CLI_ASSUME_YES.load(Ordering::SeqCst),
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal(), &mut || {
            eprint!("Download them now? [y/N] ");
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer).ok()?;
            Some(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
        });
    match decision {
        Ok(()) => {
            log.step("downloading", json!({"models": plan.missing.len()}));
            let result = install_models(&plan.missing, None, &cli_progress).map(|_| ());
            match &result { Ok(()) => log.succeed(json!({"models": plan.missing.len()})), Err(error) => log.fail(failure_code(error)) }
            result
        }
        Err((code, message)) => { log.fail(code); Err(message) }
    }
}

/// The CLI's answer to missing models, with a fixed log code on refusal.
fn cli_decision(plan: &ModelPlan, assume_yes: bool, interactive: bool, ask: &mut dyn FnMut() -> Option<bool>) -> Result<(), (&'static str, String)> {
    let list = plan.missing.join(", ");
    if plan.offline {
        return Err(("offline", format!("offline mode: missing OCR models {list}. Install them first with `crispsorter models pull` while online.")));
    }
    let unconsented: Vec<&str> = plan.models.iter()
        .filter(|m| needs_download(m) && m.requires_consent && !m.consent_accepted).map(|m| m.name.as_str()).collect();
    if !unconsented.is_empty() {
        return Err(("consent_required", format!("{} require(s) licence consent; pass --accept-license to download.", unconsented.join(", "))));
    }
    if assume_yes { return Ok(()); }
    if !interactive {
        return Err(("confirmation_required", format!("missing OCR models: {list}. Re-run with --yes (or --download-models), or install them with `crispsorter models pull <engine>`.")));
    }
    match ask() {
        Some(true) => Ok(()),
        _ => Err(("declined", format!("download declined; missing OCR models: {list}."))),
    }
}

/// `crispsorter models pull <engine> [--language L]`.
pub fn cli_pull(engine: &str, language: Option<&str>, extras: &[String]) -> Result<ModelPlan, String> {
    validate_extras(extras)?;
    let (config, _) = crate::extractors::ocr_policy::recognition_config(
        OcrPipelineConfig { enabled: true, ..Default::default() }, Some(engine), language)?;
    let plan = plan(&config, extras);
    eprint!("{}", describe_plan(&plan));
    if plan.missing.is_empty() { eprintln!("All models for '{engine}' are installed."); return Ok(plan); }
    if plan.offline { return Err("offline mode is enabled; nothing can be downloaded.".into()); }
    let unconsented: Vec<&str> = plan.models.iter()
        .filter(|m| needs_download(m) && m.requires_consent && !m.consent_accepted).map(|m| m.name.as_str()).collect();
    if !unconsented.is_empty() {
        return Err(format!("{} require(s) licence consent; pass --accept-license to download.", unconsented.join(", ")));
    }
    eprintln!("Downloading {} model(s), {}:", plan.missing.len(), human(plan.missing_bytes));
    install_models(&plan.missing, None, &cli_progress)?;
    Ok(crate::ocr_models::plan(&config, extras))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::ocr_crispembed::required_models;
    use crate::extractors::ocr_policy::recognition_config;

    fn names(config: &OcrPipelineConfig) -> Vec<String> { required_models(config).into_iter().map(|m| m.name).collect() }

    fn engine(engine: &str, language: Option<&str>) -> OcrPipelineConfig {
        recognition_config(OcrPipelineConfig::default(), Some(engine), language).unwrap().0
    }

    #[test]
    fn engines_map_to_their_registry_models() {
        assert_eq!(names(&engine("tesseract", Some("de"))), ["dbnet-det", "tesseract-deu"]);
        assert_eq!(names(&engine("tesseract", None)), ["dbnet-det", "tesseract-eng"]);
        assert_eq!(names(&engine("dbnet_trocr", None)), ["dbnet-det", "trocr-printed"]);
        assert_eq!(names(&engine("parseq", None)), ["dbnet-det", "parseq"]);
        assert_eq!(names(&engine("ppocrv6", None)), ["ppocrv6-tiny-det", "ppocrv6-tiny-rec"]);
        assert_eq!(names(&engine("easyocr", None)), ["dbnet-det", "easyocr-english-g2"]);
        assert_eq!(names(&engine("qwen2vl", None)), ["german-ocr-3.1"]);
        assert_eq!(names(&engine("glm", None)), ["glm-ocr"]);
        assert!(names(&engine("system_tesseract", None)).is_empty());
        assert!(names(&engine("ocrs", None)).is_empty(), "legacy adapters are listed separately");
    }

    #[test]
    fn preparation_layout_and_postprocessing_models_are_planned() {
        let mut config = OcrPipelineConfig { enabled: true, ..Default::default() };
        config.restore = true;
        config.sr = true;
        config.denoise = true;
        config.dewarp = true;
        config.dewarp_engine = "tps".into();
        config.layout = true;
        config.punct_model = Some("fireredpunc".into());
        let (config, _) = recognition_config(config, Some("tesseract"), Some("en")).unwrap();
        let planned = required_models(&config);
        let get = |name: &str| planned.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("{name} missing: {planned:?}"));
        assert_eq!(get("tps-loc").engine, "preparation");
        assert_eq!(get("restormer-denoise").purpose, "restoration");
        assert_eq!(get("pan-x4").purpose, "super-resolution");
        assert_eq!(get("nafnet-denoise").purpose, "learned denoise");
        assert_eq!(get("layout-heron").engine, "layout");
        assert_eq!(get("ppformulanet-l").engine, "layout");
        assert_eq!(get("fireredpunc").engine, "postprocessing");
        assert_eq!(get("tesseract-eng").engine, "tesseract");
        // The model-free layout engine needs no layout weights.
        let mut cc = config.clone();
        cc.layout_engine = "cc".into();
        assert!(!required_models(&cc).iter().any(|m| m.name == "layout-heron"));
    }

    #[test]
    fn extras_and_duplicates_are_planned_once() {
        let config = engine("dbnet_trocr", None);
        let list = requirements(&config, &["kie".into(), "math".into()]);
        if cfg!(feature = "crispembed") {
            let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
            assert_eq!(names, ["dbnet-det", "trocr-printed", "lilt-funsd", "ppformulanet-l"]);
        } else {
            assert!(list.is_empty(), "no native models in a build without CrispEmbed");
        }
        assert!(validate_extras(&["unknown".into()]).is_err());
    }

    #[test]
    fn registry_sizes_parse() {
        assert_eq!(parse_size("13 MB"), Some(13_000_000));
        assert_eq!(parse_size("2.3 GB"), Some(2_300_000_000));
        assert_eq!(parse_size("976 KB"), Some(976_000));
        assert_eq!(parse_size("0.9 MB"), Some(900_000));
        assert_eq!(parse_size("~1684 MB"), Some(1_684_000_000));
        assert_eq!(parse_size("1 GiB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("large"), None);
    }

    fn fake_facts(name: &str) -> Option<RegistryFacts> {
        match name {
            "dbnet-det" => Some(RegistryFacts { size_bytes: Some(13_000_000), license: Some("apache-2.0".into()), downloadable: true, note: None }),
            "tesseract-deu" => Some(RegistryFacts { size_bytes: Some(976_000), license: Some("apache-2.0".into()), downloadable: true, note: None }),
            "easyocr-english-g2" => Some(RegistryFacts { size_bytes: Some(16_000_000), license: Some("apache-2.0".into()), downloadable: false, note: Some("No download".into()) }),
            "got-ocr2" => Some(RegistryFacts { size_bytes: Some(700_000_000), license: Some("apache-2.0".into()), downloadable: true, note: None }),
            _ => None,
        }
    }

    #[test]
    fn plan_reports_size_licence_consent_and_install_state() {
        let reqs = required_models(&engine("tesseract", Some("de")));
        let plan = plan_models(&reqs, &fake_facts, &|name| name == "dbnet-det");
        assert_eq!(plan.len(), 2);
        assert!(plan[0].installed && plan[0].downloadable);
        assert!(!plan[1].installed);
        assert_eq!(plan[1].size_bytes, Some(976_000));
        assert_eq!(plan[1].license.as_deref(), Some("apache-2.0"));
        assert!(!plan[1].requires_consent);
        // The research-licensed GOT model is flagged for consent.
        let got = plan_models(&required_models(&engine("got", None)), &fake_facts, &|_| false);
        assert!(got[0].requires_consent);
        // An unregistered download cannot be offered.
        let easy = plan_models(&required_models(&engine("easyocr", None)), &fake_facts, &|_| false);
        assert!(!easy.iter().find(|m| m.name == "easyocr-english-g2").unwrap().downloadable);
    }

    #[test]
    fn missing_models_produce_the_download_required_error() {
        let reqs = required_models(&engine("tesseract", Some("de")));
        let plan = plan_models(&reqs, &fake_facts, &|name| name == "dbnet-det");
        let error = gate(&plan).unwrap_err();
        let json = error.strip_prefix(MODEL_DOWNLOAD_REQUIRED).expect("prefix");
        let missing: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0]["name"], "tesseract-deu");
        assert_eq!(missing[0]["purpose"], "text recognition");
        assert_eq!(missing[0]["size_bytes"], 976_000);
        // Installed, or only non-downloadable gaps: recognition proceeds.
        assert!(gate(&plan_models(&reqs, &fake_facts, &|_| true)).is_ok());
        let easy = plan_models(&required_models(&engine("easyocr", None)), &fake_facts, &|n| n == "dbnet-det");
        assert!(gate(&easy).is_ok());
        let log = OperationLog::start("models.plan", json!({}));
        assert!(ensure_or_gate(&plan, false, &log).unwrap_err().starts_with(MODEL_DOWNLOAD_REQUIRED));
    }

    // ── installer ──

    struct MemoryFetch {
        bytes: Vec<u8>,
        honour_range: bool,
        /// Stop the body after this many bytes (simulated connection drop).
        cut_at: Mutex<Option<usize>>,
        opens: Mutex<Vec<u64>>,
    }

    struct Cut { data: std::io::Cursor<Vec<u8>> }
    impl Read for Cut {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.read(&mut buf[..buf.len().min(7)])?;
            Ok(n)
        }
    }
    struct Broken { data: std::io::Cursor<Vec<u8>> }
    impl Read for Broken {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.data.read(&mut buf[..buf.len().min(7)])?;
            if n == 0 { return Err(std::io::Error::other("connection reset")); }
            Ok(n)
        }
    }

    impl MemoryFetch {
        fn new(bytes: &[u8]) -> Self {
            Self { bytes: bytes.to_vec(), honour_range: true, cut_at: Mutex::new(None), opens: Mutex::new(Vec::new()) }
        }
    }
    impl Fetch for MemoryFetch {
        fn open(&self, _url: &str, offset: u64) -> Result<FetchResponse, String> {
            self.opens.lock().unwrap().push(offset);
            let len = self.bytes.len() as u64;
            if offset >= len && offset > 0 && self.honour_range {
                return Ok(FetchResponse { body: Box::new(std::io::empty()), resumed: false, total: None, unsatisfiable: true });
            }
            let start = if self.honour_range { offset as usize } else { 0 };
            let resumed = self.honour_range && offset > 0;
            let mut body = self.bytes[start..].to_vec();
            let cut = self.cut_at.lock().unwrap().take();
            if let Some(cut) = cut {
                body.truncate(cut);
                return Ok(FetchResponse { body: Box::new(Broken { data: std::io::Cursor::new(body) }), resumed, total: Some(len), unsatisfiable: false });
            }
            Ok(FetchResponse { body: Box::new(Cut { data: std::io::Cursor::new(body) }), resumed, total: Some(len), unsatisfiable: false })
        }
    }

    fn artifact(bytes: &[u8]) -> Artifact {
        Artifact { filename: "ocr-test.gguf".into(), url: "https://example.invalid/ocr-test.gguf".into(),
            sha256: format!("{:x}", Sha256::digest(bytes)), primary: true, gguf: true }
    }

    fn quiet() -> OperationLog { OperationLog::start("models.download", json!({})) }

    fn payload() -> Vec<u8> { let mut b = b"GGUF".to_vec(); b.extend((0..400u32).map(|i| (i % 251) as u8)); b }

    #[test]
    fn interrupted_download_resumes_from_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let file = artifact(&bytes);
        let fetch = MemoryFetch::new(&bytes);
        *fetch.cut_at.lock().unwrap() = Some(150);
        let cancel = AtomicBool::new(false);
        let error = download_artifact(&fetch, dir.path(), &file, &cancel, &mut |_, _| {}).unwrap_err();
        assert!(error.contains("interrupted"), "{error}");
        assert_eq!(std::fs::metadata(part_path(dir.path(), &file)).unwrap().len(), 150);
        let mut seen = Vec::new();
        let size = download_artifact(&fetch, dir.path(), &file, &cancel, &mut |done, total| seen.push((done, total))).unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(*fetch.opens.lock().unwrap(), [0, 150], "the second request resumes at the partial size");
        assert_eq!(seen.first(), Some(&(150, Some(bytes.len() as u64))));
        commit_artifact(dir.path(), &file).unwrap();
        assert_eq!(std::fs::read(dir.path().join("ocr-test.gguf")).unwrap(), bytes);
        assert!(!part_path(dir.path(), &file).exists());
    }

    #[test]
    fn a_server_ignoring_ranges_restarts_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let file = artifact(&bytes);
        std::fs::write(part_path(dir.path(), &file), &bytes[..100]).unwrap();
        let mut fetch = MemoryFetch::new(&bytes);
        fetch.honour_range = false;
        download_artifact(&fetch, dir.path(), &file, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert_eq!(std::fs::read(part_path(dir.path(), &file)).unwrap(), bytes);
    }

    #[test]
    fn a_complete_partial_file_is_verified_without_downloading_again() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let file = artifact(&bytes);
        std::fs::write(part_path(dir.path(), &file), &bytes).unwrap();
        let fetch = MemoryFetch::new(&bytes);
        assert_eq!(download_artifact(&fetch, dir.path(), &file, &AtomicBool::new(false), &mut |_, _| {}).unwrap(), bytes.len() as u64);
    }

    #[test]
    fn checksum_mismatch_discards_the_download() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let mut file = artifact(&bytes);
        file.sha256 = format!("{:x}", Sha256::digest(b"other"));
        let error = download_artifact(&MemoryFetch::new(&bytes), dir.path(), &file, &AtomicBool::new(false), &mut |_, _| {}).unwrap_err();
        assert!(error.contains("SHA-256"), "{error}");
        assert!(!part_path(dir.path(), &file).exists());
        assert!(!dir.path().join(&file.filename).exists());
        // Not a GGUF primary file.
        let html = b"<html>error</html>".to_vec();
        let error = download_artifact(&MemoryFetch::new(&html), dir.path(), &artifact(&html), &AtomicBool::new(false), &mut |_, _| {}).unwrap_err();
        assert!(error.contains("not a GGUF"), "{error}");
    }

    #[test]
    fn cancel_stops_the_stream_and_keeps_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let file = artifact(&bytes);
        let cancel = AtomicBool::new(false);
        let error = download_artifact(&MemoryFetch::new(&bytes), dir.path(), &file, &cancel, &mut |done, _| {
            if done >= 50 { cancel.store(true, Ordering::SeqCst); }
        }).unwrap_err();
        assert_eq!(error, CANCELLED);
        let kept = std::fs::metadata(part_path(dir.path(), &file)).unwrap().len();
        assert!(kept > 0 && kept < bytes.len() as u64);
        // The next attempt resumes and completes.
        download_artifact(&MemoryFetch::new(&bytes), dir.path(), &file, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    }

    #[test]
    fn installer_commits_companions_first_and_reports_progress() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let fetch = MemoryFetch::new(&bytes);
        let mut companion = artifact(&bytes);
        companion.filename = "mmproj-test.gguf".into(); companion.primary = false;
        let items = [InstallItem { name: "installer-test".into(), estimate: Some(800), files: vec![artifact(&bytes), companion] }];
        let events = Mutex::new(Vec::new());
        let paths = install_with(&fetch, dir.path(), &items, &AtomicBool::new(false), None, Some("req"),
            &|update| events.lock().unwrap().push((update.phase, update.filename)), &quiet()).unwrap();
        assert_eq!(paths, [dir.path().join("ocr-test.gguf")]);
        let events = events.into_inner().unwrap();
        let verified: Vec<&str> = events.iter().filter(|e| e.0 == "verified").map(|e| e.1.as_str()).collect();
        assert_eq!(verified, ["mmproj-test.gguf", "ocr-test.gguf"]);
        assert_eq!(events.first().unwrap().0, "checking");
        assert_eq!(events.last().unwrap().0, "complete");
        // A second request finds the model installed and downloads nothing.
        let opens = fetch.opens.lock().unwrap().len();
        install_with(&fetch, dir.path(), &items, &AtomicBool::new(false), None, None, &|_| {}, &quiet()).unwrap();
        assert_eq!(fetch.opens.lock().unwrap().len(), opens);
    }

    #[test]
    fn installer_refuses_when_the_disk_is_too_full() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let items = [InstallItem { name: "disk-test".into(), estimate: Some(5_000_000_000), files: vec![artifact(&bytes)] }];
        let error = install_with(&MemoryFetch::new(&bytes), dir.path(), &items, &AtomicBool::new(false), Some(1_000_000_000), None, &|_| {}, &quiet()).unwrap_err();
        assert!(error.starts_with("Not enough disk space"), "{error}");
    }

    #[test]
    fn concurrent_requests_share_one_download() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let fetch = MemoryFetch::new(&bytes);
        let items = [InstallItem { name: "shared-test".into(), estimate: None, files: vec![artifact(&bytes)] }];
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| install_with(&fetch, dir.path(), &items, &AtomicBool::new(false), None, None, &|_| {}, &quiet()).unwrap());
            }
        });
        assert_eq!(fetch.opens.lock().unwrap().len(), 1, "only the first request downloads");
    }

    #[test]
    fn cancel_registry_reaches_a_running_request() {
        let flag = Arc::new(AtomicBool::new(false));
        cancels().lock().unwrap().insert("cancel-test".into(), flag.clone());
        assert!(cancel("cancel-test"));
        assert!(flag.load(Ordering::SeqCst));
        assert!(!cancel("unknown-request"));
        cancels().lock().unwrap().remove("cancel-test");
    }

    #[test]
    fn unsafe_artifacts_are_rejected_before_io() {
        let original = artifact(b"GGUF");
        for name in ["../x.gguf", "a/b.gguf", ".hidden", ""] {
            let mut file = original.clone(); file.filename = name.into(); assert!(validate_artifact(&file).is_err());
        }
        for url in ["http://example.com/m", "https://user:pw@example.com/m", ""] {
            let mut file = original.clone(); file.url = url.into(); assert!(validate_artifact(&file).is_err());
        }
        let mut unpinned = original; unpinned.sha256.clear();
        assert!(validate_artifact(&unpinned).unwrap_err().contains("SHA-256"));
    }

    #[test]
    fn content_range_totals_parse() {
        let value = reqwest::header::HeaderValue::from_static("bytes 100-199/200");
        assert_eq!(content_range_total(Some(&value)), Some(200));
        let unknown = reqwest::header::HeaderValue::from_static("bytes 100-199/*");
        assert_eq!(content_range_total(Some(&unknown)), None);
    }

    #[test]
    fn a_download_held_by_another_process_waits_then_finds_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let fetch = MemoryFetch::new(&bytes);
        let file = artifact(&bytes);
        let items = [InstallItem { name: "lock-test".into(), estimate: None, files: vec![file.clone()] }];
        // The "other process": a separate open file description on the lock.
        let other = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(dir.path().join("ocr-test.gguf.lock")).unwrap();
        other.try_lock().unwrap();
        let events = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| install_with(&fetch, dir.path(), &items, &AtomicBool::new(false), None, None,
                &|update| events.lock().unwrap().push(update.phase), &quiet()));
            // It announces the wait and does not touch the network meanwhile.
            for _ in 0..200 {
                if events.lock().unwrap().contains(&"waiting") { break; }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(events.lock().unwrap().contains(&"waiting"));
            assert!(fetch.opens.lock().unwrap().is_empty());
            // The other process commits the model, then releases its lock.
            std::fs::write(dir.path().join("ocr-test.gguf"), &bytes).unwrap();
            drop(other);
            worker.join().unwrap().unwrap();
        });
        assert!(fetch.opens.lock().unwrap().is_empty(), "the committed file is reused, not downloaded again");
        assert_eq!(events.into_inner().unwrap().last(), Some(&"complete"));
    }

    #[test]
    fn waiting_for_another_process_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let items = [InstallItem { name: "lock-cancel-test".into(), estimate: None, files: vec![artifact(&bytes)] }];
        let other = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(dir.path().join("ocr-test.gguf.lock")).unwrap();
        other.try_lock().unwrap();
        let cancel = AtomicBool::new(true);
        let error = install_with(&MemoryFetch::new(&bytes), dir.path(), &items, &cancel, None, None, &|_| {}, &quiet()).unwrap_err();
        assert_eq!(error, CANCELLED);
    }

    fn receipts(events: &[crate::operation_log::OperationEvent]) -> Vec<(String, String, Option<String>, Option<String>)> {
        events.iter().map(|e| (e.operation.clone(), e.phase.clone(), e.stage.clone(), e.code.clone())).collect()
    }

    fn assert_no_leaks(events: &[crate::operation_log::OperationEvent], secrets: &[&str]) {
        let text = serde_json::to_string(events).unwrap();
        for secret in secrets { assert!(!text.contains(secret), "log leaked {secret}: {text}"); }
        assert!(!text.contains("https://") && !text.contains("example.invalid"), "log leaked a URL: {text}");
    }

    #[test]
    fn install_receipts_cover_every_step_without_paths_or_urls() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let fetch = MemoryFetch::new(&bytes);
        *fetch.cut_at.lock().unwrap() = Some(100);
        let items = [InstallItem { name: "receipt-test".into(), estimate: Some(400), files: vec![artifact(&bytes)] }];
        // Interrupted, then resumed from the partial file.
        let (first, events) = crate::operation_log::capture(|| {
            let log = quiet();
            let result = install_with(&fetch, dir.path(), &items, &AtomicBool::new(false), None, None, &|_| {}, &log);
            finish_install(&log, &result);
            result
        });
        assert!(first.is_err());
        let first_receipts = receipts(&events);
        assert!(first_receipts.contains(&("models.download".into(), "failed".into(), Some("downloading".into()), Some("interrupted".into()))), "{first_receipts:?}");
        let (second, events) = crate::operation_log::capture(|| {
            let log = quiet();
            let result = install_with(&fetch, dir.path(), &items, &AtomicBool::new(false), None, None, &|_| {}, &log);
            finish_install(&log, &result);
            result
        });
        second.unwrap();
        let stages: Vec<Option<String>> = events.iter().map(|e| e.stage.clone()).collect();
        for stage in ["checking", "downloading", "resuming", "verified", "committed", "model_complete"] {
            assert!(stages.contains(&Some(stage.into())), "missing {stage}: {stages:?}");
        }
        assert_eq!(events.last().map(|e| e.phase.as_str()), Some("completed"));
        let dir_text = dir.path().to_string_lossy().into_owned();
        assert_no_leaks(&events, &[&dir_text, "ocr-test.gguf"]);
    }

    #[test]
    fn failure_receipts_use_fixed_codes() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = payload();
        let dir_text = dir.path().to_string_lossy().into_owned();
        let run = |items: &[InstallItem], cancel: bool, free: Option<u64>| crate::operation_log::capture(|| {
            let log = quiet();
            let result = install_with(&MemoryFetch::new(&bytes), dir.path(), items, &AtomicBool::new(cancel), free, None, &|_| {}, &log);
            finish_install(&log, &result);
            result
        });
        let mut bad = artifact(&bytes);
        bad.sha256 = format!("{:x}", Sha256::digest(b"other"));
        let cases: [(Vec<InstallItem>, bool, Option<u64>, &str); 3] = [
            (vec![InstallItem { name: "sha-receipt".into(), estimate: None, files: vec![bad] }], false, None, "checksum_mismatch"),
            (vec![InstallItem { name: "space-receipt".into(), estimate: Some(9_000_000_000), files: vec![artifact(&bytes)] }], false, Some(1_000), "insufficient_space"),
            (vec![InstallItem { name: "cancel-receipt".into(), estimate: None, files: vec![artifact(&bytes)] }], true, None, "cancelled"),
        ];
        for (items, cancel, free, code) in cases {
            let (result, events) = run(&items, cancel, free);
            assert!(result.is_err());
            let last = events.last().expect("terminal event");
            if code == "cancelled" { assert_eq!(last.phase, "cancelled"); }
            else { assert_eq!((last.phase.as_str(), last.code.as_deref()), ("failed", Some(code))); }
            assert_no_leaks(&events, &[&dir_text]);
        }
        assert_eq!(failure_code("Offline mode is enabled and 1 model(s) are not installed: x."), "offline");
        assert_eq!(failure_code("Model \"x\" carries a CC licence that must be accepted"), "consent_required");
    }

    #[test]
    fn plan_and_remove_receipts_are_recorded() {
        let (plan, events) = crate::operation_log::capture(|| summarize("tesseract".into(),
            plan_models(&required_models(&engine("tesseract", Some("de"))), &fake_facts, &|n| n == "dbnet-det")));
        assert_eq!(plan.missing, ["tesseract-deu"]);
        let done = events.iter().find(|e| e.operation == "models.plan" && e.phase == "completed").expect("plan receipt");
        assert_eq!(done.context.get("pending").and_then(|v| v.as_u64()), Some(1));
        assert_eq!(done.context.get("bytes").and_then(|v| v.as_u64()), Some(976_000));
        let (removed, events) = crate::operation_log::capture(|| remove_model("../outside.gguf"));
        assert!(removed.is_err());
        let last = events.last().unwrap();
        assert_eq!((last.operation.as_str(), last.phase.as_str(), last.code.as_deref()), ("models.remove", "failed", Some("remove_refused")));
        assert_no_leaks(&events, &["outside"]);
    }

    #[test]
    fn builtin_models_are_planned_for_paddle_and_ocrs() {
        use crate::ocr_builtin_models as builtin;
        let ocrs: Vec<String> = builtin_requirements(&engine("ocrs", None), &[]).into_iter().map(|m| m.name).collect();
        assert_eq!(ocrs, [builtin::OCRS_DET, builtin::OCRS_REC]);
        let paddle = |language: Option<&str>, extras: &[&str]| -> Vec<String> {
            let extras: Vec<String> = extras.iter().map(|s| s.to_string()).collect();
            builtin_requirements(&engine("paddle", language), &extras).into_iter().map(|m| m.name).collect()
        };
        if crate::extractors::ocr_paddle::is_paddle_ocr_available() {
            assert_eq!(paddle(None, &[]), [builtin::PADDLE_DET, builtin::PADDLE_REC_EN]);
            assert_eq!(paddle(Some("ja"), &[]), [builtin::PADDLE_DET, builtin::PADDLE_REC_CH]);
            assert_eq!(paddle(None, &["paddle_cjk"]), [builtin::PADDLE_DET, builtin::PADDLE_REC_CH]);
            assert_eq!(paddle(None, &["paddle_all"]).len(), 4);
        } else {
            assert!(paddle(None, &[]).is_empty(), "no PaddleOCR models without the engine");
        }
        // Builtins are pinned, downloadable and gate like registry models.
        let planned = planned_builtin(builtin::find(builtin::OCRS_REC).unwrap(), false);
        assert_eq!(planned.size_bytes, Some(9_713_177));
        if builtins_managed() { assert!(gate(&[planned]).unwrap_err().starts_with(MODEL_DOWNLOAD_REQUIRED)); }
        let item = install_item(builtin::PADDLE_REC_EN).unwrap();
        assert_eq!(item.files.len(), 2);
        assert!(item.files[0].primary && !item.files[0].gguf && !item.files[1].primary);
    }

    #[test]
    fn onnx_and_text_files_skip_the_gguf_check_but_not_the_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let onnx = b"\x08\x07onnx-protobuf".to_vec();
        let mut file = artifact(&onnx);
        file.filename = "model.onnx".into();
        file.gguf = false;
        download_artifact(&MemoryFetch::new(&onnx), dir.path(), &file, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        file.sha256 = format!("{:x}", Sha256::digest(b"other"));
        let _ = std::fs::remove_file(part_path(dir.path(), &file));
        assert!(download_artifact(&MemoryFetch::new(&onnx), dir.path(), &file, &AtomicBool::new(false), &mut |_, _| {}).unwrap_err().contains("SHA-256"));
    }

    /// Live: downloads tesseract-eng + dbnet-det (a few MB) into a temporary
    /// cache, resumes a deliberately truncated `.part`, verifies the pins and
    /// recognises the quality fixture. Run alone (it pins the process cache):
    /// `cargo test --lib --features ... -- --ignored live_model_manager_installs_resumes_and_recognises`
    #[cfg(all(feature = "crispembed", not(feature = "ocr-mobile")))]
    #[test]
    #[ignore = "live: downloads OCR models"]
    fn live_model_manager_installs_resumes_and_recognises() {
        let cache = tempfile::tempdir().unwrap();
        // Before anything resolves the cache: this test owns the process.
        unsafe { std::env::set_var("CRISPEMBED_CACHE_DIR", cache.path()); }
        assert_eq!(crate::model_cache::dir(), cache.path(), "run this test on its own");
        let names = vec!["tesseract-eng".to_string(), "dbnet-det".to_string()];
        // Truncate a partial download of the detector to prove resume.
        let det = install_item("dbnet-det").unwrap();
        let pin = det.files[0].clone();
        let full = HttpFetch::new().unwrap();
        let mut head = Vec::new();
        full.open(&pin.url, 0).unwrap().body.take(4096).read_to_end(&mut head).unwrap();
        std::fs::write(part_path(cache.path(), &pin), &head).unwrap();
        let (installed, events) = crate::operation_log::capture(|| install_models(&names, None, &|_| {}));
        let installed = installed.expect("live install");
        assert_eq!(installed.len(), 2);
        assert!(events.iter().any(|e| e.stage.as_deref() == Some("resuming")), "the truncated part was resumed");
        for name in &names {
            let item = install_item(name).unwrap();
            for file in &item.files {
                let bytes = std::fs::read(cache.path().join(&file.filename)).unwrap();
                assert_eq!(format!("{:x}", Sha256::digest(&bytes)), file.sha256, "{name} pin");
            }
            assert!(is_installed(name));
        }
        let config = engine("tesseract", Some("eng"));
        assert!(plan(&config, &[]).missing.is_empty());
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/quality/ocr-task.png");
        let mut options = crate::extractors::ExtractOptions::default();
        options.ocr_pipeline = config;
        options.try_ocr = true;
        let doc = crate::extractors::extract_text_from_path_with_opts(&fixture, options).expect("recognition");
        let text = doc.full_text.to_lowercase();
        for word in ["cobalt", "warranty", "november", "2031"] {
            assert!(text.contains(word), "missing {word:?} in {text:?}");
        }
    }


    #[test]
    fn cli_decisions_refuse_with_fixed_codes() {
        let models = plan_models(&required_models(&engine("tesseract", Some("de"))), &fake_facts, &|n| n == "dbnet-det");
        let base = ModelPlan { engine: "tesseract".into(), missing: vec!["tesseract-deu".into()], missing_bytes: 976_000,
            free_bytes: None, offline: false, models };
        fn never() -> Option<bool> { panic!("must not prompt") }
        assert!(cli_decision(&base, true, false, &mut never).is_ok(), "--yes downloads without asking");
        let (code, message) = cli_decision(&base, false, false, &mut never).unwrap_err();
        assert_eq!(code, "confirmation_required");
        assert!(message.contains("tesseract-deu") && message.contains("--yes"));
        assert_eq!(cli_decision(&base, false, true, &mut || Some(false)).unwrap_err().0, "declined");
        assert!(cli_decision(&base, false, true, &mut || Some(true)).is_ok());
        let offline = ModelPlan { offline: true, ..base.clone() };
        let (code, message) = cli_decision(&offline, true, true, &mut never).unwrap_err();
        assert_eq!(code, "offline");
        assert!(message.contains("tesseract-deu"), "{message}");
        let got = plan_models(&required_models(&engine("got", None)), &fake_facts, &|_| false);
        let restricted = ModelPlan { missing: vec!["got-ocr2".into()], models: got, ..base };
        if !crate::index::license_consent::is_accepted("got-ocr2") {
            assert_eq!(cli_decision(&restricted, true, true, &mut never).unwrap_err().0, "consent_required");
        }
    }

}
