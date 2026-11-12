//! Pinned model files for OCR engines outside the CrispEmbed registry:
//! PaddleOCR (`usls` ONNX models) and ocrs (`rten` ONNX models).
//!
//! These used to be fetched by the engines themselves on first use (Paddle,
//! through `usls`' GitHub/HF hub) or placed by hand (ocrs). They now go
//! through the same manager as every other OCR model (`ocr_models`): planned,
//! confirmed, downloaded with progress, SHA-256 verified, stored in the shared
//! cache (`model_cache`) and loaded from there. The engines are only ever
//! given local paths, so they cannot download anything themselves.
//!
//! Sizes and SHA-256 pins were measured from the URLs below on 2026-11-12.
//! - PaddleOCR v4: the `jamjamjon/assets` release files `usls 0.2.0-alpha.3`
//!   resolves for `Config::ppocr_det_v4_ch`, `ppocr_rec_v4_{en,ch}` and
//!   `slanet_lcnet_v2_mobile_ch` (model + vocabulary). Apache-2.0.
//! - ocrs: the ONNX models upstream `ocrs-cli` downloads (the former `.rten`
//!   objects are gone); `rten` 0.24 loads ONNX directly. CC-BY-SA-4.0.

use std::path::PathBuf;

#[derive(Debug)]
pub struct BuiltinFile {
    pub filename: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

#[derive(Debug)]
pub struct BuiltinModel {
    pub name: &'static str,
    pub purpose: &'static str,
    pub engine: &'static str,
    pub license: &'static str,
    /// The model file first, then companions (vocabularies).
    pub files: &'static [BuiltinFile],
}

impl BuiltinModel {
    pub fn size(&self) -> u64 { self.files.iter().map(|f| f.size).sum() }
}

macro_rules! asset { ($path:literal) => { concat!("https://github.com/jamjamjon/assets/releases/download/", $path) }; }

pub const PADDLE_DET: &str = "paddleocr-det-v4";
pub const PADDLE_REC_EN: &str = "paddleocr-rec-v4-en";
pub const PADDLE_REC_CH: &str = "paddleocr-rec-v4-ch";
pub const PADDLE_SLANET: &str = "paddleocr-slanet-v2";
pub const OCRS_DET: &str = "ocrs-detection";
pub const OCRS_REC: &str = "ocrs-recognition";

pub const MODELS: &[BuiltinModel] = &[
    BuiltinModel { name: PADDLE_DET, purpose: "text detection", engine: "paddle", license: "apache-2.0", files: &[
        BuiltinFile { filename: "paddleocr-det-v4-ch.onnx", url: asset!("db/ppocr-v4-ch.onnx"),
            sha256: "69ce850fec741a2a4568c7c924bb025c9d4f1129e5f96ab428c799ccc5ef2275", size: 4_729_474 },
    ] },
    BuiltinModel { name: PADDLE_REC_EN, purpose: "text recognition (Latin)", engine: "paddle", license: "apache-2.0", files: &[
        BuiltinFile { filename: "paddleocr-rec-v4-en.onnx", url: asset!("svtr/ppocr-v4-en.onnx"),
            sha256: "02672d591fee561a95a2491bd5504d66c812b145d1043026c5658ecc165c7e85", size: 7_652_836 },
        BuiltinFile { filename: "paddleocr-rec-v4-en-vocab.txt", url: asset!("svtr/vocab-v1-ppocr-rec-en.txt"),
            sha256: "c2711945715a1578f8f5dd0e532774294c1c061636d9a458aa0d7b8f01adbc6d", size: 196 },
    ] },
    BuiltinModel { name: PADDLE_REC_CH, purpose: "text recognition (CJK)", engine: "paddle", license: "apache-2.0", files: &[
        BuiltinFile { filename: "paddleocr-rec-v4-ch.onnx", url: asset!("svtr/ppocr-v4-ch.onnx"),
            sha256: "06b3e6af6c59a1ba5d53790ed8c2e4b2de389870b6cf5a97f349f3412cb269c0", size: 10_822_323 },
        BuiltinFile { filename: "paddleocr-rec-v4-ch-vocab.txt", url: asset!("svtr/vocab-v1-ppocr-rec-ch.txt"),
            sha256: "18a5433cb90c93ca8c862217dd47b91a003a9efcead85f09aaf6b7806eb59b82", size: 26_257 },
    ] },
    BuiltinModel { name: PADDLE_SLANET, purpose: "table structure", engine: "paddle", license: "apache-2.0", files: &[
        BuiltinFile { filename: "paddleocr-slanet-v2-mobile-ch.onnx", url: asset!("slanet/v2-mobile-ch.onnx"),
            sha256: "5de7b5e35ac83cfa5b646e1a932d29c6cd8a3c397d1adf59757f42190ce0ceec", size: 7_789_898 },
        BuiltinFile { filename: "paddleocr-slanet-v2-vocab.txt", url: asset!("slanet/vocab-sla-v2.txt"),
            sha256: "01e644a1161a8c969b34ad72fcdc5fd0b3e775524163ed9991b82e9e2b851dd2", size: 590 },
    ] },
    BuiltinModel { name: OCRS_DET, purpose: "text detection", engine: "ocrs", license: "cc-by-sa-4.0", files: &[
        BuiltinFile { filename: "ocrs-text-detection.onnx", url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx",
            sha256: "a917b23dbd9524b465df7e922641b3ff2981623df4ded5a0234004ef2fee7cfe", size: 2_499_479 },
    ] },
    BuiltinModel { name: OCRS_REC, purpose: "text recognition", engine: "ocrs", license: "cc-by-sa-4.0", files: &[
        BuiltinFile { filename: "ocrs-text-recognition.onnx", url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx",
            sha256: "86c145c2edb96c8caed5b1ebb8f44d706408922211451c309c625157dd6061c5", size: 9_713_177 },
    ] },
];

pub fn find(name: &str) -> Option<&'static BuiltinModel> { MODELS.iter().find(|m| m.name == name) }

pub fn is_builtin(name: &str) -> bool { find(name).is_some() }

/// Installed file paths (model first), all from one cache directory.
pub fn locate(name: &str) -> Option<Vec<PathBuf>> {
    let model = find(name)?;
    let names: Vec<&str> = model.files.iter().map(|f| f.filename).collect();
    let primary = crate::model_cache::locate_files(&names)?;
    let dir = primary.parent()?.to_path_buf();
    Some(names.iter().map(|n| dir.join(n)).collect())
}

/// Installed paths, or the `MODEL_DOWNLOAD_REQUIRED:` error the UI turns into
/// the download dialog (and the CLI into its plan). Never downloads.
pub fn require(name: &str) -> Result<Vec<PathBuf>, String> {
    if let Some(paths) = locate(name) { return Ok(paths); }
    let model = find(name).ok_or_else(|| format!("Unknown OCR model '{name}'."))?;
    let planned = crate::ocr_models::planned_builtin(model, false);
    match crate::ocr_models::gate(std::slice::from_ref(&planned)) {
        Err(error) => Err(error),
        Ok(()) => Err(format!("OCR model '{name}' is not installed. Install it in Settings > OCR models or with `crispsorter models pull {}`.", model.engine)),
    }
}

/// Paddle recognizer for the resolved script.
pub fn paddle_recognizer(cjk: bool) -> &'static str { if cjk { PADDLE_REC_CH } else { PADDLE_REC_EN } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_file_is_pinned_https_and_uniquely_named() {
        let mut names = std::collections::HashSet::new();
        for model in MODELS {
            assert!(!model.files.is_empty());
            assert!(crate::index::license_consent::license_for_registry_name(model.name) == crate::index::license_consent::ModelLicense::Permissive,
                "{} needs a reviewed licence entry", model.name);
            for file in model.files {
                assert!(file.url.starts_with("https://"), "{}", file.url);
                assert_eq!(file.sha256.len(), 64);
                assert!(file.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                assert!(file.size > 0);
                assert!(names.insert(file.filename), "duplicate {}", file.filename);
                assert!(!file.filename.contains('/') && !file.filename.starts_with('.'));
            }
        }
        assert!(MODELS.iter().all(|m| m.files[0].filename.ends_with(".onnx")));
    }

    #[test]
    fn missing_builtin_models_report_the_download_dialog_error() {
        // A developer machine may already have it; CI starts empty.
        if locate(OCRS_DET).is_some() { return; }
        let error = require(OCRS_DET).unwrap_err();
        if let Some(json) = error.strip_prefix(crate::ocr_models::MODEL_DOWNLOAD_REQUIRED) {
            let models: Vec<serde_json::Value> = serde_json::from_str(json).unwrap();
            assert_eq!(models[0]["name"], OCRS_DET);
            assert_eq!(models[0]["size_bytes"], 2_499_479);
        } else {
            // Mobile profile: builtins are not managed, so a plain error.
            assert!(error.contains("not installed"), "{error}");
        }
        assert_eq!(paddle_recognizer(true), PADDLE_REC_CH);
        assert_eq!(paddle_recognizer(false), PADDLE_REC_EN);
    }
}
