//! Task presets shared by workbench callers; advanced persisted config is retained.
use super::{OcrPipelineConfig, OcrStageSpec};

pub fn apply(base: &OcrPipelineConfig, preset: &str) -> Result<OcrPipelineConfig, String> {
    let mut cfg = base.clone();
    cfg.enabled = true;
    if preset == "custom" { return Ok(cfg); }
    cfg.router = false;
    cfg.stages.clear();
    cfg.denoise = false;
    cfg.sr = false;
    cfg.restore = false;
    cfg.dewarp = false;
    cfg.layout = false;
    cfg.vlm_ocr_model = None;
    let mut stage: OcrStageSpec = serde_json::from_value(serde_json::json!({})).map_err(|e| e.to_string())?;
    match preset {
        "quick" => {
            stage.engine = "dbnet_trocr".into();
            stage.det_model = Some("dbnet-det".into());
            stage.rec_model = Some("trocr-printed".into());
        }
        "multilingual" => {
            // Qwen is a complete VLM, not a TrOCR line recognizer.
            stage.engine = "qwen2vl".into();
            stage.det_model = Some("qwen2vl-3b".into());
            stage.vlm_max_tokens = 4096;
            cfg.drop_headers_footers = true;
        }
        "tables" => {
            stage.engine = "qwen2vl".into();
            stage.det_model = Some("qwen2vl-3b".into());
            stage.vlm_max_tokens = 4096;
            stage.vlm_prompt = "Transcribe this page faithfully. Preserve all table rows and columns as Markdown tables. Do not summarize or invent missing values.".into();
        }
        "difficult" => {
            stage.engine = "qwen2vl".into();
            stage.det_model = Some("qwen2vl-3b".into());
            stage.vlm_max_tokens = 4096;
            stage.cleanup.enabled = true;
            stage.cleanup.deskew = true;
            stage.cleanup.crop_borders = true;
            stage.cleanup.whiten_background = true;
            stage.cleanup.binarize = true;
            stage.cleanup.binarize_method = 1; // Sauvola for uneven backgrounds.
        }
        _ => return Err(format!("Unknown OCR preset: {preset}")),
    }
    cfg.stages.push(stage);
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quick_does_not_inherit_expensive_custom_processing() {
        let base = OcrPipelineConfig { sr: true, restore: true, dewarp: true, ..Default::default() };
        let quick = apply(&base, "quick").unwrap();
        assert!(!quick.sr && !quick.restore && !quick.dewarp);
        assert!(base.sr && base.restore && base.dewarp);
        assert_eq!(quick.stages[0].engine, "dbnet_trocr");
    }
    #[test]
    fn table_preset_keeps_tables_in_the_recognition_input() {
        let cfg = apply(&Default::default(), "tables").unwrap();
        assert!(!cfg.layout); // Existing layout pass skips table regions.
        assert!(cfg.stages[0].vlm_prompt.contains("all table rows"));
        assert!(apply(&cfg, "unqualified-model").is_err());
    }
    #[test]
    fn multilingual_vlm_is_not_loaded_as_a_trocr_line_model() {
        let cfg = apply(&Default::default(), "multilingual").unwrap();
        assert_eq!(cfg.stages[0].engine, "qwen2vl");
        assert!(cfg.stages[0].rec_model.is_none());
    }
}
