//! Isolated OCR with a process-scoped CPU pixel budget.
//!
//! Environment overrides belong to the child, never the multithreaded GUI.
//! Modes preserve either native review provenance or the pipeline's complete
//! document text (including punctuation, truecasing and detected language).
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use super::{ExtractedDocument, OcrPipelineConfig, ocr_crispembed::{self, OcrReviewResult, RegionConf}};

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
}

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
    super::ocr_policy::validate_config(&request.config).map_err(anyhow::Error::msg)?;
    // Already running in the isolated process; never spawn a second worker.
    request.config.cpu_pixel_budget = false;
    super::ocr_policy::validate_runtime(&request.config).map_err(anyhow::Error::msg)?;
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
    std::fs::write(response, data).context("write OCR worker response")?;
    Ok(())
}

#[cfg(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))))]
fn run_worker<T: serde::de::DeserializeOwned>(path: &Path, config: &OcrPipelineConfig, mode: OutputMode) -> Result<T> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    super::ocr_policy::validate_cpu_budget(config).map_err(anyhow::Error::msg)?;
    ocr_crispembed::with_released_pipeline(|| {
        let mut request = tempfile::NamedTempFile::new().context("create OCR request")?;
        let response = tempfile::NamedTempFile::new().context("create OCR response")?;
        serde_json::to_writer(&mut request, &Request { path: path.to_owned(), config: config.clone(), mode })?;
        request.flush()?;
        let output = Command::new(std::env::current_exe()?)
            .arg("--crispsorter-ocr-worker").arg(request.path()).arg(response.path())
            .env("CRISPEMBED_MAX_PIXELS", CPU_PIXEL_BUDGET.to_string())
            .env("QWEN2VL_OCR_FORCE_CPU", "1")
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped())
            .output().context("start isolated OCR worker")?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            let tail: String = error.chars().rev().take(2000).collect::<String>().chars().rev().collect();
            anyhow::bail!("isolated OCR worker failed ({}): {}", output.status, tail.trim());
        }
        if response.as_file().metadata()?.len() > 32 * 1024 * 1024 {
            anyhow::bail!("OCR worker response is too large");
        }
        let data = std::fs::read(response.path()).context("read OCR worker response")?;
        serde_json::from_slice(&data).context("parse OCR worker response")
    })
}

#[cfg(not(all(feature = "sidecars", not(any(target_os = "android", target_os = "ios", feature = "desktop-mas")))))]
fn run_worker<T: serde::de::DeserializeOwned>(_path: &Path, _config: &OcrPipelineConfig, _mode: OutputMode) -> Result<T> {
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
