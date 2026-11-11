//! Isolated OCR with a process-scoped CPU pixel budget.
//!
//! Environment overrides belong to the child, never the multithreaded GUI.
//! Modes preserve either native review provenance or the pipeline's complete
//! document text (including punctuation, truecasing and detected language).
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use super::{ExtractedDocument, OcrPipelineConfig, ocr_crispembed::{self, OcrReviewResult, RegionConf}};
use crate::operation_log::OperationLog;
use serde_json::{json, Value};

pub const CPU_PIXEL_BUDGET: u32 = 262_144;
pub const CPU_BUDGET_AVAILABLE: bool =
    cfg!(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))));

#[derive(Default, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutputMode { #[default] Detailed, Review, Document }

#[derive(serde::Serialize, serde::Deserialize)]
struct Request {
    path: PathBuf,
    config: OcrPipelineConfig,
    #[serde(default)]
    mode: OutputMode,
    #[serde(default)]
    logging: Option<WorkerLogging>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct WorkerLogging { operation_id: String, path: PathBuf }

#[derive(serde::Serialize, serde::Deserialize)]
struct DocumentOutput {
    full_text: String,
    headings: Vec<String>,
    language: Option<String>,
    ext: String,
}

/// Entry point invoked before the desktop GUI or CLI is initialized.
pub fn execute(request: &Path, response: &Path) -> Result<()> {
    if std::fs::metadata(request).context("inspect OCR worker request")?.len() > 1_048_576 {
        anyhow::bail!("OCR worker request is too large");
    }
    let input = std::fs::read(request).context("read OCR worker request")?;
    let mut request: Request = serde_json::from_slice(&input).context("parse OCR worker request")?;
    if let Some(logging) = request.logging.take() {
        crate::operation_log::with_worker_transport(&logging.path, &logging.operation_id,
            || execute_request(request, response)).context("open OCR worker event channel")?
    } else { execute_request(request, response) }
}

fn execute_request(mut request: Request, response: &Path) -> Result<()> {
    let log = OperationLog::start("ocr.worker_child", json!({"surface":"worker","engine":request.config.engine}));
    let result = (|| {
    log.step("validating", Value::Null);
    super::ocr_policy::validate_config(&request.config).map_err(anyhow::Error::msg)?;
    // Already running in the isolated process; never spawn a second worker.
    request.config.cpu_pixel_budget = false;
    super::ocr_policy::validate_runtime(&request.config).map_err(anyhow::Error::msg)?;
    log.step("recognizing", json!({"mode":request.mode}));
    let data = match request.mode {
        OutputMode::Detailed => serde_json::to_vec(&ocr_crispembed::ocr_regions_detailed(&request.path, &request.config)?)?,
        OutputMode::Review => serde_json::to_vec(&ocr_crispembed::ocr_regions_for_review(&request.path, &request.config)?)?,
        OutputMode::Document => {
            let mut opts = super::ExtractOptions::default();
            opts.ocr_pipeline = request.config;
            let document = super::ocr_image_page(&request.path, &opts)?;
            serde_json::to_vec(&DocumentOutput { full_text: document.full_text, headings: document.headings,
                language: document.language, ext: document.ext })?
        }
    };
    log.step("writing_response", json!({"bytes":data.len()}));
    std::fs::write(response, &data).context("write OCR worker response")?;
    log.succeed(json!({"bytes":data.len()}));
    Ok(())
    })();
    if result.is_err() { log.fail("worker_execution_failed"); }
    result
}

#[cfg(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))))]
fn run_worker<T: serde::de::DeserializeOwned>(path: &Path, config: &OcrPipelineConfig, mode: OutputMode) -> Result<T> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let log = OperationLog::start("ocr.worker", json!({"engine":config.engine,"mode":mode,"cpu_budget":true}));
    let result = (|| {
    log.step("validating", Value::Null);
    super::ocr_policy::validate_cpu_budget(config).map_err(anyhow::Error::msg)?;
    log.step("releasing_models", Value::Null);
    ocr_crispembed::with_released_pipeline(|| {
        let mut request = tempfile::NamedTempFile::new().context("create OCR request")?;
        let response = tempfile::NamedTempFile::new().context("create OCR response")?;
        let events = tempfile::NamedTempFile::new().context("create OCR worker event channel")?;
        serde_json::to_writer(&mut request, &Request { path: path.to_owned(), config: config.clone(), mode,
            logging: Some(WorkerLogging { operation_id:log.id().into(), path:events.path().to_owned() }) })?;
        request.flush()?;
        log.step("spawning", Value::Null);
        let output = Command::new(std::env::current_exe()?)
            .arg("--crispsorter-ocr-worker").arg(request.path()).arg(response.path())
            .env("CRISPEMBED_MAX_PIXELS", CPU_PIXEL_BUDGET.to_string())
            .env("QWEN2VL_OCR_FORCE_CPU", "1")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped())
            .output().context("start isolated OCR worker")?;
        match crate::operation_log::replay_worker_events(events.path(), log.id()) {
            Ok(count) => log.step("worker_events", json!({"event_count":count})),
            Err(_) => log.warn("worker_events_unavailable", Value::Null),
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let diagnostics = stderr.lines().filter(|line| !line.trim().is_empty()
            && !["[info] operation ", "[warn] operation ", "[error] operation ", "[debug] operation "]
                .iter().any(|prefix| line.starts_with(prefix))).count();
        if diagnostics > 0 { log.warn("native_worker_diagnostics", json!({"diagnostic_lines":diagnostics})); }
        if !output.status.success() {
            let tail: String = stderr.chars().rev().take(2000).collect::<String>().chars().rev().collect();
            anyhow::bail!("isolated OCR worker failed ({}): {}", output.status, tail.trim());
        }
        if response.as_file().metadata()?.len() > 32 * 1024 * 1024 {
            anyhow::bail!("OCR worker response is too large");
        }
        let data = std::fs::read(response.path()).context("read OCR worker response")?;
        log.step("reading_response", json!({"bytes":data.len()}));
        serde_json::from_slice(&data).context("parse OCR worker response")
    })
    })();
    match &result { Ok(_) => log.succeed(Value::Null), Err(_) => log.fail("worker_failed") }
    result
}

#[cfg(not(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas")))))]
fn run_worker<T: serde::de::DeserializeOwned>(_path: &Path, _config: &OcrPipelineConfig, _mode: OutputMode) -> Result<T> {
    let log = OperationLog::start("ocr.worker", json!({"cpu_budget":true}));
    log.fail("unsupported_build");
    anyhow::bail!("The isolated CPU OCR profile requires a desktop build with sidecars enabled")
}

pub fn run_cpu_budget(path: &Path, config: &OcrPipelineConfig) -> Result<Vec<RegionConf>> {
    run_worker(path, config, OutputMode::Detailed)
}

pub fn run_cpu_budget_review(path: &Path, config: &OcrPipelineConfig) -> Result<OcrReviewResult> {
    run_worker(path, config, OutputMode::Review)
}

pub fn run_cpu_budget_document(path: &Path, config: &OcrPipelineConfig) -> Result<ExtractedDocument> {
    let document: DocumentOutput = run_worker(path, config, OutputMode::Document)?;
    Ok(ExtractedDocument { full_text: document.full_text, headings: document.headings,
        language: document.language, ext: document.ext, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_worker_wire_preserves_provenance_word_evidence_and_unicode() {
        let payload = serde_json::json!({"image_path":"page.png","width":100,"height":80,
            "regions":[{"text":"Grüße","x":1,"y":2,"w":40,"h":10,"confidence":0.9,
                "confidence_basis":"recognition","char_conf":[0.9,0.8],"char_conf_unit":"token",
                "char_conf_aligned":false,"kind":"text","word_evidence":{
                    "version":1,"offset_unit":"utf16","source":"system_tesseract",
                    "words":[{"text":"Grüße","start":0,"end":5,"x":1,"y":2,"w":40,"h":10,
                        "confidence":0.9,"confidence_basis":"recognition"}]
                }}],"warnings":["Unaligned tokenizer scores"],"diagnostics":{"engine":"qwen2vl","schema_version":1}});
        let review: OcrReviewResult = serde_json::from_value(payload).unwrap();
        let serialized = serde_json::to_vec(&review).unwrap();
        let restored: OcrReviewResult = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(restored.regions[0].text, "Grüße");
        assert_eq!(restored.regions[0].confidence_basis, "recognition");
        assert_eq!(restored.regions[0].char_conf_unit, "token");
        assert!(!restored.regions[0].char_conf_aligned);
        assert_eq!(restored.regions[0].word_evidence.as_ref().unwrap().words[0].end, 5);
        assert_eq!(restored.warnings, vec!["Unaligned tokenizer scores"]);
        assert_eq!(restored.diagnostics["engine"], "qwen2vl");
    }

    #[test]
    fn worker_request_keeps_configured_stage_and_language_and_defaults_old_mode() {
        let request: Request = serde_json::from_value(serde_json::json!({"path":"page.png","config":{
            "engine":"qwen2vl","language":"de","cpu_pixel_budget":true,
            "stages":[{"engine":"qwen2vl","det_model":"custom.gguf","vlm_max_tokens":512}]
        }})).unwrap();
        assert!(matches!(request.mode, OutputMode::Detailed));
        assert_eq!(request.config.language.as_deref(), Some("de"));
        assert_eq!(request.config.stages[0].det_model.as_deref(), Some("custom.gguf"));
        assert_eq!(request.config.stages[0].vlm_max_tokens, 512);
        assert!(request.config.cpu_pixel_budget);
        assert!(request.logging.is_none());
    }

    #[test]
    fn worker_validation_failure_writes_safe_terminal_events_before_returning() {
        let request=tempfile::NamedTempFile::new().unwrap();
        let response=tempfile::NamedTempFile::new().unwrap();
        let events=tempfile::NamedTempFile::new().unwrap();
        let operation_id=uuid::Uuid::new_v4().to_string();
        std::fs::write(request.path(),serde_json::to_vec(&json!({
            "path":"private-document.png","config":{"engine":"unavailable-engine"},
            "logging":{"operation_id":operation_id,"path":events.path()}
        })).unwrap()).unwrap();
        assert!(execute(request.path(),response.path()).is_err());
        let logged=std::fs::read_to_string(events.path()).unwrap();
        let events:Vec<crate::operation_log::OperationEvent>=logged.lines()
            .map(|line|serde_json::from_str(line).unwrap()).collect();
        assert_eq!(events.first().unwrap().phase,"started");
        assert_eq!(events.last().unwrap().phase,"failed");
        assert!(events.iter().all(|event|event.parent_operation_id.as_deref()==Some(operation_id.as_str())));
        assert!(!logged.contains("private-document") && !logged.contains("unavailable-engine"));
        assert_eq!(response.as_file().metadata().unwrap().len(),0);
    }
    #[cfg(not(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas")))))]
    #[test]
    fn cpu_budget_is_unavailable_without_a_spawn_capable_build() {
        let error = run_cpu_budget(Path::new("unused"), &OcrPipelineConfig::default()).unwrap_err();
        assert!(error.to_string().contains("sidecars enabled"));
    }
    #[test]
    fn worker_rejects_malformed_requests_before_starting_a_model() {
        let request = tempfile::NamedTempFile::new().unwrap();
        let response = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(request.path(), b"not json").unwrap();
        assert!(execute(request.path(), response.path()).unwrap_err().to_string().contains("parse OCR worker request"));
        assert_eq!(std::fs::metadata(response.path()).unwrap().len(), 0);
    }

    #[test]
    fn worker_rejects_oversized_requests_before_reading_or_inference() {
        let request = tempfile::NamedTempFile::new().unwrap();
        let response = tempfile::NamedTempFile::new().unwrap();
        request.as_file().set_len(1_048_577).unwrap();
        assert!(execute(request.path(), response.path()).unwrap_err().to_string().contains("request is too large"));
        assert_eq!(std::fs::metadata(response.path()).unwrap().len(), 0);
    }
}
