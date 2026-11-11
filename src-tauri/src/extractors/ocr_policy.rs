//! One OCR selection policy for review, files, containers and background jobs.
//! Resolution is pure: capability checks and model loading happen at execution.

use super::{ExtractOptions, OcrPipelineConfig, OcrRecLang, OcrStageSpec, OcrTier};

pub const EMBEDDED_ENGINES: &[&str] = &[
    "dbnet_trocr", "surya", "got", "glm", "qwen2vl", "internvl2", "tesseract",
    "parseq", "deepseek_ocr2", "pix2struct", "granite_vision", "lightonocr", "qwen3vl",
    "unlimited_ocr", "unlimited_ocr2", "minicpm_ocr", "unified", "tesseract_fraktur",
    "ppocrv6", "easyocr", "olmocr", "lfm2_vl",
];
pub const LEGACY_ENGINES: &[&str] = &["system_tesseract", "ocrs", "paddle"];
pub const VISION_ENGINES: &[&str] = &[
    "got", "glm", "qwen2vl", "internvl2", "deepseek_ocr2", "pix2struct",
    "granite_vision", "lightonocr", "qwen3vl", "unlimited_ocr", "unlimited_ocr2",
    "minicpm_ocr", "unified", "olmocr", "lfm2_vl",
];

fn selection(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty() && *s != "auto")
}

fn has_flat_models(config: &OcrPipelineConfig) -> bool {
    [&config.rec_model, &config.vlm_ocr_model].iter()
        .any(|model| model.as_deref().is_some_and(|s| !s.trim().is_empty()))
}

/// An explicit picker value overrides the saved value, including `auto`.
/// Automatic preserves advanced chains and custom models. Only an unconfigured
/// embedded pipeline receives the workbench's multilingual Tesseract default.
pub fn recognition_config(
    config: OcrPipelineConfig,
    engine: Option<&str>,
    language: Option<&str>,
) -> Result<(OcrPipelineConfig, Vec<String>), String> {
    recognition_config_for_build(config, engine, language, cfg!(feature = "crispembed"))
}

fn recognition_config_for_build(
    mut config: OcrPipelineConfig,
    engine: Option<&str>,
    language: Option<&str>,
    embedded: bool,
) -> Result<(OcrPipelineConfig, Vec<String>), String> {
    let requested_engine = engine.or(config.engine.as_deref()).unwrap_or("auto").trim().to_owned();
    let language = selection(language.or(config.language.as_deref())).map(str::to_owned);
    let mut engine = selection(Some(&requested_engine)).unwrap_or("auto").to_owned();
    if engine != "auto" && !EMBEDDED_ENGINES.contains(&engine.as_str()) && !LEGACY_ENGINES.contains(&engine.as_str()) {
        return Err(format!("Unsupported OCR engine: {engine}"));
    }
    if engine == "auto" && !config.enabled {
        config = OcrPipelineConfig { engine:config.engine, language:config.language,
            cpu_pixel_budget:config.cpu_pixel_budget, ..Default::default() };
    }
    let automatic_default = engine == "auto" && config.stages.is_empty() && !has_flat_models(&config) && embedded;
    let mut warnings = Vec::new();
    if automatic_default {
        engine = "tesseract".into();
        warnings.push(format!("Automatic recognition used Tesseract ({}). Choose another engine to compare.", language.as_deref().unwrap_or("eng")));
    }
    validate_language(language.as_deref(), &engine)?;
    if EMBEDDED_ENGINES.contains(&engine.as_str()) {
        let matching = config.stages.iter().find(|s| s.engine == engine).cloned();
        let mut stage = if let Some(stage) = matching {
            // Per-stage model, cleanup, detector, prompt and accept-gate fields
            // belong to the configured stage, not the simple-mode defaults.
            stage
        } else {
            let mut stage: OcrStageSpec = serde_json::from_value(serde_json::json!({"engine":engine}))
                .map_err(|e| e.to_string())?;
            if ["dbnet_trocr", "tesseract", "tesseract_fraktur", "parseq", "easyocr"].contains(&engine.as_str()) {
                stage.det_model = config.det_model.clone();
                if engine == "dbnet_trocr" { stage.rec_model = config.rec_model.clone(); }
            } else if config.vlm_ocr_engine == engine {
                stage.det_model = config.vlm_ocr_model.clone();
            }
            stage.min_confidence = config.min_confidence;
            stage.min_chars = config.min_chars;
            stage.cleanup.enabled = config.cleanup_enabled;
            stage.cleanup.denoise = config.denoise;
            stage
        };
        // An explicit engine is a single recipe for this input, not a source
        // classifier whose other branches still contain different engines.
        stage.source_type = "auto".into();
        if config.enabled && engine == "tesseract" && language.is_none() && stage.rec_model.is_none()
            && config.lid_model.as_deref().is_some_and(|m| !m.trim().is_empty())
            && config.tess_model_dir.as_deref().is_some_and(|m| !m.trim().is_empty()) {
            stage.rec_model = Some("auto".into());
        }
        apply_language(&mut stage, language.as_deref(), &mut warnings)?;
        if stage.rec_model.is_none() {
            stage.rec_model = match engine.as_str() {
                "dbnet_trocr" => Some("trocr-printed".into()),
                "parseq" => Some("parseq".into()),
                _ => None,
            };
        }
        config.stages = vec![stage];
        config.router = false;
    } else if engine == "auto" {
        // Apply an explicit language to capable stages without throwing away
        // the user's chain, source routing, cleanup or custom prompts.
        for stage in &mut config.stages {
            apply_language(stage, language.as_deref(), &mut warnings)?;
        }
        if config.stages.is_empty() && language.is_some() {
            warnings.push("The configured recognizer uses its model's trained languages; select Tesseract or a vision engine to apply a language hint.".into());
        }
    } else if engine == "ocrs" && language.is_some() {
        warnings.push("ocrs uses its installed Latin-script recognition model; the language hint does not change its weights.".into());
    }
    if LEGACY_ENGINES.contains(&engine.as_str()) {
        config.stages.clear();
    }
    config.engine = Some(engine);
    config.language = language;
    if !config.enabled {
        // Engine selection activates recognition, not the disabled advanced
        // processing switch. Keep its weights/engine parameters without
        // resurrecting hidden restoration, cleanup or postprocessing options.
        config.cleanup_enabled = false;
        config.denoise = false;
        config.layout = false;
        config.dewarp = false;
        config.restore = false;
        config.sr = false;
        config.punct_model = None;
        config.truecase_model = None;
        config.lid_model = None;
        config.tess_model_dir = None;
        for stage in &mut config.stages {
            stage.cleanup.enabled = false;
            stage.cleanup.denoise = false;
        }
    }
    warnings.dedup();
    validate_config(&config)?;
    Ok((config, warnings))
}

fn validate_language(language: Option<&str>, engine: &str) -> Result<(), String> {
    let Some(language) = language else { return Ok(()); };
    if engine == "system_tesseract" {
        if language.len() > 128 || language.split('+').any(|part| {
            let code = part.strip_prefix("script/").unwrap_or(part);
            code.is_empty() || code.starts_with('-') || !code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        }) {
            return Err("Choose installed System Tesseract language codes such as eng or eng+deu.".into());
        }
    } else if language.len() > 32 || !language.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
        return Err("Choose a language code, for example en, de, eng or deu.".into());
    }
    Ok(())
}

fn apply_language(stage: &mut OcrStageSpec, language: Option<&str>, warnings: &mut Vec<String>) -> Result<(), String> {
    if stage.engine == "tesseract" && (language.is_some() || stage.rec_model.is_none()) {
        let lang = match language.unwrap_or("eng") {
            "en" => "eng", "de" => "deu", "fr" => "fra", "es" => "spa", "it" => "ita",
            "pt" => "por", "nl" => "nld", "ru" => "rus", "ar" => "ara", "zh" | "chi_sim" => "chi-sim",
            "ja" => "jpn", "ko" => "kor", lang => lang,
        };
        if !["eng", "deu", "fra", "spa", "ita", "por", "nld", "rus", "ara", "chi-sim", "jpn", "kor"].contains(&lang) {
            return Err(format!("No registered Tesseract model is available for '{lang}'. Configure a matching custom language model in an OCR stage and use Automatic language, or select a vision engine."));
        }
        stage.rec_model = Some(format!("tesseract-{lang}"));
    } else if let Some(language) = language {
        if ["qwen2vl", "internvl2", "granite_vision", "qwen3vl"].contains(&stage.engine.as_str()) {
            let hint = format!("The expected language is {language}. Preserve reading order, numbers, punctuation and line breaks. Do not summarize or translate.");
            if stage.vlm_prompt.is_empty() {
                stage.vlm_prompt = format!("Transcribe the document exactly. {hint}");
            } else if !stage.vlm_prompt.contains(&hint) {
                stage.vlm_prompt.push('\n');
                stage.vlm_prompt.push_str(&hint);
            }
        } else {
            warnings.push("This recognizer uses its model's trained languages; the language selector does not change that model.".into());
        }
    }
    Ok(())
}

/// File paths share the same policy as review. An explicit legacy tier still
/// matters when the advanced switch is off; an explicit engine always wins.
pub fn resolve_extraction_config(opts: &ExtractOptions) -> Result<(OcrPipelineConfig, Vec<String>), String> {
    let mut config = opts.ocr_pipeline.clone();
    if selection(config.engine.as_deref()).is_none() && !config.enabled {
        // Settings retains the last advanced recipe when its master switch is
        // off. Only an explicit engine can deliberately reuse a matching stage.
        config = OcrPipelineConfig { engine: config.engine, language: config.language,
            cpu_pixel_budget: config.cpu_pixel_budget, ..Default::default() };
        let engine = match opts.ocr_tier {
            OcrTier::Tier1 => Some("system_tesseract"),
            OcrTier::Tier2 => Some("ocrs"),
            OcrTier::Tier3 => Some("paddle"),
            OcrTier::Tier4 => Some("dbnet_trocr"),
            OcrTier::Auto if cfg!(feature = "crispembed") => None,
            OcrTier::Auto if super::ocr_paddle::is_paddle_ocr_available() => Some("paddle"),
            OcrTier::Auto if super::ocr_ocrs::is_ocrs_available() => Some("ocrs"),
            OcrTier::Auto => Some("system_tesseract"),
        };
        if let Some(engine) = engine { config.engine = Some(engine.into()); }
    }
    recognition_config(config, None, None)
}

/// Automatic LiLT selects its compatible internal recognizer only when no
/// active engine/tier/model chain was chosen. Explicit choices are preserved.
pub fn resolve_lilt_config(opts: &ExtractOptions) -> Result<(OcrPipelineConfig, Vec<String>), String> {
    let config = &opts.ocr_pipeline;
    let unconfigured_auto = selection(config.engine.as_deref()).is_none() && opts.ocr_tier == OcrTier::Auto
        && (!config.enabled || (config.stages.is_empty() && !has_flat_models(config)));
    let (config, mut warnings) = if unconfigured_auto {
        let mut config = if opts.ocr_pipeline.enabled { opts.ocr_pipeline.clone() } else {
            OcrPipelineConfig { language:config.language.clone(), cpu_pixel_budget:config.cpu_pixel_budget, ..Default::default() }
        };
        config.engine = Some("dbnet_trocr".into());
        recognition_config(config, None, None)?
    } else { resolve_extraction_config(opts)? };
    validate_lilt_config(&config)?;
    if unconfigured_auto {
        warnings.push("Automatic LiLT uses its compatible internal DBNet + TrOCR recognizer.".into());
    }
    Ok((config, warnings))
}

pub fn resolve_table_config(opts: &ExtractOptions) -> Result<(OcrPipelineConfig, Vec<String>), String> {
    let config = &opts.ocr_pipeline;
    let unconfigured_auto = selection(config.engine.as_deref()).is_none() && opts.ocr_tier == OcrTier::Auto
        && (!config.enabled || (config.stages.is_empty() && !has_flat_models(config)));
    let (config, mut warnings) = if unconfigured_auto {
        let mut config = if config.enabled { config.clone() } else {
            OcrPipelineConfig { language:config.language.clone(), cpu_pixel_budget:config.cpu_pixel_budget, ..Default::default() }
        };
        config.engine = Some("tesseract".into());
        recognition_config(config, None, None)?
    } else { resolve_extraction_config(opts)? };
    validate_table_config(&config)?;
    if unconfigured_auto { warnings.push("Automatic table extraction uses its compatible internal Tesseract cell recognizer.".into()); }
    Ok((config, warnings))
}

pub fn validate_table_config(config: &OcrPipelineConfig) -> Result<(), String> {
    let stage = config.stages.first();
    if config.stages.len() != 1 || stage.is_none_or(|s| !["tesseract", "tesseract_fraktur"].contains(&s.engine.as_str()))
        || ![Some("tesseract"), Some("tesseract_fraktur"), Some("auto")].contains(&config.engine.as_deref()) {
        return Err("The native table parser requires one embedded Tesseract cell recognizer. Choose Tesseract or use normal OCR with the selected engine; its weights were not substituted.".into());
    }
    if config.cpu_pixel_budget || config.layout || stage.is_some_and(|s| (config.enabled && s.cleanup.enabled && s.cleanup.page_split) || s.rec_model.as_deref()==Some("auto")
        || s.det_prob_threshold != 0.3 || s.det_box_threshold != 0.5 || s.det_target_short != 736
        || s.det_model.as_deref().is_some_and(|model| !model.is_empty() && model != "dbnet-det"))
        || [&config.punct_model, &config.truecase_model, &config.lid_model, &config.tess_model_dir].iter()
            .any(|m| m.as_deref().is_some_and(|m| !m.trim().is_empty())) {
        return Err("The native table parser uses rule-based cell geometry and cannot apply custom detectors, detector tuning, CPU budgeting, layout routing, page splitting or text/language-model postprocessing. Use OCR Workbench to prepare/extract the page with those options.".into());
    }
    Ok(())
}

pub fn paddle_language(config: &OcrPipelineConfig, fallback: OcrRecLang) -> OcrRecLang {
    match selection(config.language.as_deref()) {
        Some("zh" | "chi_sim" | "chi-sim" | "chi_tra" | "chi-tra" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant" | "ja" | "jpn" | "ko" | "kor") => OcrRecLang::Cjk,
        Some(_) => OcrRecLang::Latin,
        None => fallback,
    }
}

/// Runtime checks are separate so settings can be validated without loading
/// models or probing an external executable.
pub fn validate_runtime(config: &OcrPipelineConfig) -> Result<(), String> {
    validate_config(config)?;
    let engine = config.engine.as_deref().unwrap_or("auto");
    if (engine == "auto" || EMBEDDED_ENGINES.contains(&engine)) && !cfg!(feature = "crispembed") {
        return Err(format!("OCR engine '{engine}' requires a build with CrispEmbed; the selected engine was not substituted."));
    }
    if engine == "paddle" && !super::ocr_paddle::is_paddle_ocr_available() {
        return Err("PaddleOCR requires a build with the paddle-ocr feature; the selected engine was not substituted.".into());
    }
    if engine == "system_tesseract" && !crate::ocr_word_evidence::SYSTEM_TESSERACT_AVAILABLE {
        return Err("System Tesseract requires a desktop build with sidecars enabled; the selected engine was not substituted.".into());
    }
    if config.enabled && !cfg!(feature = "crispembed") && (config.cleanup_enabled || config.layout || config.dewarp || config.restore || config.sr || config.denoise) {
        return Err("The selected layout/restoration options require a build with CrispEmbed.".into());
    }
    let embedded = engine == "auto" || EMBEDDED_ENGINES.contains(&engine);
    if !embedded && config.enabled && [&config.punct_model, &config.truecase_model, &config.lid_model, &config.tess_model_dir]
        .iter().any(|model| model.as_deref().is_some_and(|m| !m.trim().is_empty())) {
        return Err("The configured punctuation, truecasing and language-model postprocessing require an embedded OCR pipeline; select an embedded engine or disable these models.".into());
    }
    for stage in config.stages.iter().filter(|_| embedded) {
        if stage.engine == "unified" && stage.det_model.as_deref().is_none_or(|m| m.trim().is_empty()) {
            return Err("Unified OCR requires a custom GGUF model in the stage's detection/single-model field.".into());
        }
        if stage.engine == "surya" && [&stage.det_model, &stage.rec_model].iter().any(|m| m.as_deref().is_none_or(|m| m.trim().is_empty())) {
            return Err("Surya requires compatible detection and recognition models in a matching advanced stage; DBNet/TrOCR weights were not substituted.".into());
        }
        let gates: &[&str] = match stage.engine.as_str() {
            "unlimited_ocr2" => &["CRISPEMBED_UNLIMITED_OCR2"],
            "minicpm_ocr" => &["CRISPEMBED_MINICPM_OCR", "CRISPEMBED_QWEN35_OCR"],
            _ => &[],
        };
        for gate in gates {
            if std::env::var(gate).ok().as_deref() != Some("1") {
                return Err(format!("Experimental OCR engine '{}' requires {gate}=1 in the application environment.", stage.engine));
            }
        }
    }
    if config.cpu_pixel_budget { validate_cpu_budget(config)?; }
    Ok(())
}

pub fn validate_cpu_budget(config: &OcrPipelineConfig) -> Result<(), String> {
    if !super::ocr_worker::CPU_BUDGET_AVAILABLE {
        return Err("The isolated CPU OCR profile requires a desktop build with sidecars enabled.".into());
    }
    if !cfg!(feature = "crispembed") {
        return Err("The isolated CPU OCR profile requires CrispEmbed.".into());
    }
    if config.stages.len() != 1 || config.stages[0].engine != "qwen2vl"
        || ![None, Some("auto"), Some("qwen2vl")].contains(&config.engine.as_deref()) {
        return Err("The CPU pixel budget is supported by the Qwen2 VL engine only, with a single stage; select one Qwen2-VL stage or disable the CPU budget.".into());
    }
    Ok(())
}

/// The pinned LiLT binding owns a DBNet/TrOCR recognizer internally. It cannot
/// accept another engine's output or expose that engine's detector controls.
pub fn validate_lilt_config(config: &OcrPipelineConfig) -> Result<(), String> {
    let stage = config.stages.first();
    if config.stages.len() != 1 || stage.is_none_or(|s| s.engine != "dbnet_trocr")
        || ![Some("dbnet_trocr"), Some("auto")].contains(&config.engine.as_deref()) {
        return Err("LiLT KIE requires one DBNet + TrOCR stage because its native binding performs OCR internally. Select that engine or use text-based field extraction; the chosen OCR engine was not substituted.".into());
    }
    let stage = stage.unwrap();
    if config.cpu_pixel_budget || config.layout || stage.det_prob_threshold != 0.3
        || stage.det_box_threshold != 0.5 || stage.det_target_short != 736
        || (config.enabled && stage.cleanup.enabled && stage.cleanup.page_split)
        || [&config.punct_model, &config.truecase_model, &config.lid_model]
            .iter().any(|m| m.as_deref().is_some_and(|m| !m.trim().is_empty())) {
        return Err("LiLT's internal OCR cannot apply the selected CPU budget, layout, detector tuning, page splitting or text postprocessing. Use text-based field extraction to retain those options.".into());
    }
    Ok(())
}

pub fn validate_config(config: &OcrPipelineConfig) -> Result<(), String> {
    if let Some(engine) = selection(config.engine.as_deref()) {
        if !EMBEDDED_ENGINES.contains(&engine) && !LEGACY_ENGINES.contains(&engine) {
            return Err(format!("Unsupported OCR engine: {engine}"));
        }
    }
    validate_language(selection(config.language.as_deref()), config.engine.as_deref().unwrap_or("auto"))?;
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
        || !VISION_ENGINES.contains(&config.vlm_ocr_engine.as_str()) {
        return Err("Unknown layout or escalation engine.".into());
    }
    if config.restore_engine == "instructir" && !["denoise", "deblur", "dehaze", "derain", "super_resolution", "low_light", "enhance"].contains(&config.restore_task.as_str()) {
        return Err("Unknown restoration task.".into());
    }
    for stage in &config.stages {
        if !EMBEDDED_ENGINES.contains(&stage.engine.as_str()) || !["auto", "screenshot", "scanned_doc", "photo"].contains(&stage.source_type.as_str()) {
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
    let json = serde_json::to_string(config).map_err(|e| e.to_string())?;
    if json.contains("\\u0000") { return Err("OCR model paths cannot contain null characters.".into()); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_engine_preserves_stage_recipe_models_and_custom_prompt() {
        let config: OcrPipelineConfig = serde_json::from_value(json!({
            "stages":[{"engine":"qwen2vl","det_model":"custom.gguf","min_chars":0,"min_confidence":0,
                "vlm_prompt":"Read only the invoice number.","vlm_max_tokens":512,"det_target_short":960,
                "cleanup":{"enabled":false,"denoise":false,"sauvola_k":0.4}}]
        })).unwrap();
        let (out, _) = recognition_config_for_build(config, Some("qwen2vl"), Some("de"), true).unwrap();
        let stage = &out.stages[0];
        assert_eq!(stage.det_model.as_deref(), Some("custom.gguf"));
        assert_eq!((stage.min_chars, stage.min_confidence, stage.vlm_max_tokens, stage.det_target_short), (0, 0.0, 512, 960));
        assert!(!stage.cleanup.enabled);
        assert_eq!(stage.cleanup.sauvola_k, 0.4);
        assert!(stage.vlm_prompt.starts_with("Read only the invoice number.\n"));
        assert!(stage.vlm_prompt.contains("expected language is de"));
    }

    #[test]
    fn automatic_preserves_advanced_chains_custom_tesseract_and_flat_models() {
        let config: OcrPipelineConfig = serde_json::from_value(json!({"enabled":true,"stages":[
            {"engine":"tesseract","rec_model":"custom-pol.gguf","source_type":"scanned_doc"},
            {"engine":"qwen3vl","det_model":"custom-vlm.gguf","min_chars":0}
        ]})).unwrap();
        let (out, _) = recognition_config_for_build(config, Some("auto"), Some("auto"), true).unwrap();
        assert_eq!(out.engine.as_deref(), Some("auto"));
        assert!(out.router);
        assert_eq!(out.stages.len(), 2);
        assert_eq!(out.stages[0].rec_model.as_deref(), Some("custom-pol.gguf"));
        assert_eq!(out.stages[0].source_type, "scanned_doc");
        let flat = OcrPipelineConfig { enabled:true, rec_model: Some("custom-trocr.gguf".into()), ..Default::default() };
        let (out, _) = recognition_config_for_build(flat, Some("auto"), None, true).unwrap();
        assert!(out.stages.is_empty());
        assert_eq!(out.rec_model.as_deref(), Some("custom-trocr.gguf"));
    }

    #[test]
    fn saved_selection_and_explicit_auto_have_distinct_precedence() {
        let config = OcrPipelineConfig { engine: Some("tesseract".into()), language: Some("de".into()), ..Default::default() };
        let (out, _) = recognition_config_for_build(config.clone(), None, None, true).unwrap();
        assert_eq!(out.stages[0].rec_model.as_deref(), Some("tesseract-deu"));
        let (out, _) = recognition_config_for_build(config, Some("auto"), Some("auto"), true).unwrap();
        assert_eq!(out.stages[0].rec_model.as_deref(), Some("tesseract-eng"));
        assert_eq!(out.language, None);
    }

    #[test]
    fn explicit_legacy_tiers_and_language_never_turn_into_other_engines() {
        for (tier, expected) in [(OcrTier::Tier1,"system_tesseract"),(OcrTier::Tier2,"ocrs"),(OcrTier::Tier3,"paddle"),(OcrTier::Tier4,"dbnet_trocr")] {
            let mut opts = ExtractOptions::default();
            opts.ocr_tier = tier;
            opts.ocr_pipeline.language = Some("de".into());
            let (out, _) = resolve_extraction_config(&opts).unwrap();
            assert_eq!(out.engine.as_deref(), Some(expected));
        }
        let mut opts = ExtractOptions::default();
        opts.ocr_tier = OcrTier::Tier1;
        opts.ocr_pipeline.engine = Some("parseq".into());
        assert_eq!(resolve_extraction_config(&opts).unwrap().0.engine.as_deref(), Some("parseq"));
    }

    #[test]
    fn paddle_traditional_chinese_overrides_latin_script_fallback() {
        for language in ["chi_tra", "chi-tra", "zh-tw", "zh-hant"] {
            let (config, _) = recognition_config(
                OcrPipelineConfig::default(), Some("paddle"), Some(language),
            ).unwrap();
            assert_eq!(paddle_language(&config, OcrRecLang::Latin), OcrRecLang::Cjk, "{language}");
        }
        let (config, _) = recognition_config(
            OcrPipelineConfig::default(), Some("paddle"), Some("eng"),
        ).unwrap();
        assert_eq!(paddle_language(&config, OcrRecLang::Cjk), OcrRecLang::Latin);
        let (config, _) = recognition_config(
            OcrPipelineConfig::default(), Some("paddle"), Some("auto"),
        ).unwrap();
        assert_eq!(paddle_language(&config, OcrRecLang::Cjk), OcrRecLang::Cjk);
    }

    #[test]
    fn disabled_advanced_recipe_does_not_run_when_selection_is_automatic() {
        let mut opts = ExtractOptions::default();
        opts.ocr_pipeline = serde_json::from_value(json!({"enabled":false,"layout":true,"restore":true,
            "rec_model":"old-recognizer.gguf","stages":[{"engine":"qwen2vl","det_model":"old-qwen.gguf"}]})).unwrap();
        opts.ocr_tier = OcrTier::Tier1;
        let (config, _) = resolve_extraction_config(&opts).unwrap();
        assert_eq!(config.engine.as_deref(), Some("system_tesseract"));
        assert!(!config.enabled && !config.layout && !config.restore);
        assert!(config.stages.is_empty());
        opts.ocr_pipeline.engine = Some("qwen2vl".into());
        let (config, _) = resolve_extraction_config(&opts).unwrap();
        assert_eq!(config.stages[0].det_model.as_deref(), Some("old-qwen.gguf"));
        assert!(!config.enabled && !config.layout && !config.restore && !config.cleanup_enabled);
        assert!(!config.stages[0].cleanup.enabled);
    }

    #[test]
    fn tesseract_language_detection_retains_the_native_auto_sentinel() {
        let config = OcrPipelineConfig { enabled:true, lid_model:Some("lid.gguf".into()), tess_model_dir:Some("/models/tesseract".into()), ..Default::default() };
        let (config, _) = recognition_config_for_build(config, Some("tesseract"), None, true).unwrap();
        assert_eq!(config.stages[0].rec_model.as_deref(), Some("auto"));
    }

    #[test]
    fn system_language_lists_are_preserved_and_embedded_lists_are_rejected() {
        let (out, _) = recognition_config(Default::default(), Some("system_tesseract"), Some("eng+deu")).unwrap();
        assert_eq!(out.language.as_deref(), Some("eng+deu"));
        assert!(recognition_config(Default::default(), Some("tesseract"), Some("eng+deu")).is_err());
        for bad in ["eng+", "../deu", "-l", "eng\0deu"] {
            assert!(recognition_config(Default::default(), Some("system_tesseract"), Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn serialization_preserves_selection_cpu_and_legal_zero_thresholds() {
        let config: OcrPipelineConfig = serde_json::from_value(json!({"engine":"qwen2vl","language":"de","cpu_pixel_budget":true,"min_chars":0,"min_confidence":0})).unwrap();
        let (out, _) = recognition_config(config, None, None).unwrap();
        let back: OcrPipelineConfig = serde_json::from_slice(&serde_json::to_vec(&out).unwrap()).unwrap();
        assert_eq!(back.engine.as_deref(), Some("qwen2vl"));
        assert_eq!(back.language.as_deref(), Some("de"));
        assert!(back.cpu_pixel_budget);
        assert_eq!(back.stages[0].min_chars, 0);
        assert_eq!(back.stages[0].min_confidence, 0.0);
    }

    #[test]
    fn unsupported_build_rejects_selected_engine_before_any_fallback() {
        if !cfg!(feature = "crispembed") {
            let (config, _) = recognition_config(Default::default(), Some("parseq"), None).unwrap();
            assert!(validate_runtime(&config).unwrap_err().contains("not substituted"));
        }
        if !cfg!(feature = "paddle-ocr") {
            let (config, _) = recognition_config(Default::default(), Some("paddle"), None).unwrap();
            assert!(validate_runtime(&config).unwrap_err().contains("paddle-ocr"));
        }
    }

    #[test]
    fn legacy_selection_does_not_load_an_inactive_experimental_model() {
        let config: OcrPipelineConfig = serde_json::from_value(json!({"enabled":false,"stages":[
            {"engine":"unified","cleanup":{"enabled":true,"denoise":true}}, {"engine":"minicpm_ocr"}
        ]})).unwrap();
        let (resolved, _) = recognition_config(config, Some("ocrs"), None).unwrap();
        assert!(resolved.stages.is_empty());
        assert!(!resolved.enabled);
        assert!(validate_runtime(&resolved).is_ok());
    }

    #[test]
    fn lilt_rejects_ocr_substitution_and_retains_custom_compatible_models() {
        let (vision, _) = recognition_config(Default::default(), Some("qwen2vl"), None).unwrap();
        assert!(validate_lilt_config(&vision).unwrap_err().contains("not substituted"));
        let config: OcrPipelineConfig = serde_json::from_value(json!({"stages":[
            {"engine":"dbnet_trocr","det_model":"custom-det.gguf","rec_model":"custom-rec.gguf"}
        ]})).unwrap();
        let (mut dbnet, _) = recognition_config(config, Some("dbnet_trocr"), None).unwrap();
        assert!(validate_lilt_config(&dbnet).is_ok());
        assert_eq!(dbnet.stages[0].rec_model.as_deref(), Some("custom-rec.gguf"));
        dbnet.stages[0].det_target_short = 1024;
        assert!(validate_lilt_config(&dbnet).unwrap_err().contains("detector tuning"));
    }

    #[test]
    fn specialist_automatic_defaults_never_replace_explicit_engine_or_tier() {
        let mut opts = ExtractOptions::default();
        let (lilt, warnings) = resolve_lilt_config(&opts).unwrap();
        assert_eq!(lilt.engine.as_deref(), Some("dbnet_trocr"));
        assert!(!warnings.is_empty());
        opts.ocr_pipeline.language = Some("de".into());
        let (table, _) = resolve_table_config(&opts).unwrap();
        assert_eq!(table.stages[0].rec_model.as_deref(), Some("tesseract-deu"));
        opts.ocr_pipeline.engine = Some("qwen2vl".into());
        assert!(resolve_lilt_config(&opts).unwrap_err().contains("not substituted"));
        assert!(resolve_table_config(&opts).unwrap_err().contains("not substituted"));
        opts.ocr_pipeline.engine = Some("auto".into());
        opts.ocr_tier = OcrTier::Tier1;
        assert!(resolve_table_config(&opts).is_err());
        assert!(resolve_lilt_config(&opts).is_err());
    }

    #[test]
    fn specialists_ignore_inactive_split_recipes_without_replacing_models() {
        for engine in ["tesseract", "dbnet_trocr"] {
            for (smart_enabled, cleanup_enabled) in [(false, true), (true, false), (true, true)] {
                let mut opts = ExtractOptions::default();
                opts.ocr_pipeline = serde_json::from_value(json!({
                    "enabled": smart_enabled, "engine": engine, "stages": [{
                        "engine": engine, "rec_model": "custom-recognizer.gguf",
                        "cleanup": {"enabled": cleanup_enabled, "page_split": true, "auto_crop": true}
                    }]
                })).unwrap();
                let result = if engine == "tesseract" { resolve_table_config(&opts) }
                    else { resolve_lilt_config(&opts) };
                if smart_enabled && cleanup_enabled {
                    assert!(result.unwrap_err().contains("page splitting"), "{engine}");
                } else {
                    let (resolved, _) = result.unwrap();
                    let stage = &resolved.stages[0];
                    assert_eq!(stage.rec_model.as_deref(), Some("custom-recognizer.gguf"));
                    assert!(!stage.cleanup.enabled);
                    assert!(stage.cleanup.page_split && stage.cleanup.auto_crop);
                    assert_eq!(opts.ocr_pipeline.stages[0].cleanup.enabled, cleanup_enabled,
                        "resolution must not edit the saved recipe");
                }
            }
        }
    }
}
