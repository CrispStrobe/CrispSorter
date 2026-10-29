//! Isolated workbench OCR for a process-scoped CPU pixel budget.
//!
//! The released Qwen runtime reads CRISPEMBED_MAX_PIXELS from the process
//! environment. Never mutate the GUI process environment while other threads
//! are running: pass the override to a short-lived child process instead.
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use super::{OcrPipelineConfig, ocr_crispembed::{self, RegionConf}};

pub const CPU_PIXEL_BUDGET: u32 = 262_144;

#[derive(serde::Serialize, serde::Deserialize)]
struct Request { path: PathBuf, config: OcrPipelineConfig }

/// Entry point invoked before the desktop GUI or CLI is initialized.
pub fn execute(request: &Path, response: &Path) -> Result<()> {
    if std::fs::metadata(request).context("inspect OCR worker request")?.len() > 1_048_576 {
        anyhow::bail!("OCR worker request is too large");
    }
    let input = std::fs::read(request).context("read OCR worker request")?;
    let request: Request = serde_json::from_slice(&input).context("parse OCR worker request")?;
    let regions = ocr_crispembed::ocr_regions_detailed(&request.path, &request.config)?;
    std::fs::write(response, serde_json::to_vec(&regions)?).context("write OCR worker response")?;
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios", feature = "desktop-mas")))]
pub fn run_cpu_budget(path: &Path, config: &OcrPipelineConfig) -> Result<Vec<RegionConf>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    ocr_crispembed::with_released_pipeline(|| {
        let mut request = tempfile::NamedTempFile::new().context("create OCR request")?;
        let response = tempfile::NamedTempFile::new().context("create OCR response")?;
        serde_json::to_writer(&mut request, &Request { path: path.to_owned(), config: config.clone() })?;
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

#[cfg(any(target_os = "android", target_os = "ios", feature = "desktop-mas"))]
pub fn run_cpu_budget(_path: &Path, _config: &OcrPipelineConfig) -> Result<Vec<RegionConf>> {
    anyhow::bail!("The isolated CPU OCR profile is available on desktop builds")
}

#[cfg(test)]
mod tests {
    use super::*;
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
