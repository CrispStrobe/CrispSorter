//! CLI overlays for the same OCR configuration used by the application.
//! Clap defaults never overwrite saved settings or an explicit JSON snapshot.

use crate::extractors::{ExtractOptions, OcrPipelineConfig, OcrStageSpec, OcrTier};
use std::{collections::HashSet, io::Read, path::Path};

const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const FIELDS: &[(&str, &str)] = &[
    ("engine", "engine"), ("language", "language"), ("cpu_pixel_budget", "cpu_pixel_budget"),
    ("cleanup", "cleanup_enabled"), ("denoise", "denoise"), ("det_model", "det_model"),
    ("rec_model", "rec_model"), ("nafnet_model", "nafnet_model"), ("layout", "layout"),
    ("layout_engine", "layout_engine"), ("layout_threshold", "layout_threshold"),
    ("drop_headers_footers", "drop_headers_footers"), ("punct_model", "punct_model"),
    ("min_chars", "min_chars"), ("min_confidence", "min_confidence"), ("sr", "sr"),
    ("sr_model", "sr_model"), ("sr_max_px", "sr_max_short_side"), ("sr_engine", "sr_engine"),
    ("restore", "restore"), ("restore_model", "restore_model"), ("restore_engine", "restore_engine"),
    ("restore_task", "restore_task"), ("dewarp", "dewarp"), ("dewarp_engine", "dewarp_engine"),
    ("vlm_ocr_model", "vlm_ocr_model"), ("vlm_ocr_engine", "vlm_ocr_engine"),
    ("truecase_model", "truecase_model"), ("lid_model", "lid_model"), ("tess_model_dir", "tess_model_dir"),
];

pub(super) fn explicit_options(matches: &clap::ArgMatches) -> HashSet<String> {
    matches.ids().filter(|id| matches.value_source(id.as_str()) == Some(clap::parser::ValueSource::CommandLine))
        .map(|id| id.as_str().to_owned()).collect()
}

pub(super) fn load(data_dir: &Path, snapshot: Option<&Path>) -> Result<ExtractOptions, String> {
    let operation = crate::operation_log::OperationLog::start("ocr.settings", serde_json::json!({
        "surface":"cli","mode":"read","saved":snapshot.is_none()}));
    let result = load_inner(data_dir, snapshot);
    match &result {
        Ok(options) => operation.succeed(serde_json::json!({"stages":options.ocr_pipeline.stages.len()})),
        Err(_) => operation.fail("settings_load_failed"),
    }
    result
}

fn load_inner(data_dir: &Path, snapshot: Option<&Path>) -> Result<ExtractOptions, String> {
    if let Some(path) = snapshot {
        let mut bytes = Vec::new();
        std::fs::File::open(path).map_err(|e| format!("Open OCR configuration: {e}"))?
            .take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)
            .map_err(|e| format!("Read OCR configuration: {e}"))?;
        let config = parse_snapshot(&bytes)?;
        // A supplied snapshot owns its defaults, just like a Workbench project.
        return Ok(ExtractOptions { try_ocr: true, ocr_pdf_min_chars: usize::MAX,
            ingest_image_level: "l3".into(), image_extraction_enabled: true,
            ocr_pipeline: config, ..Default::default() });
    }
    crate::bg_ingest::load_ocr_options(data_dir, true).map_err(|e| format!("Load OCR settings: {e:#}"))
}

fn parse_snapshot(bytes: &[u8]) -> Result<OcrPipelineConfig, String> {
    if bytes.len() as u64 > MAX_CONFIG_BYTES { return Err("OCR configuration exceeds 64 KiB.".into()); }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| format!("Invalid OCR configuration: {e}"))?;
    let mut known = serde_json::to_value(OcrPipelineConfig::default()).map_err(|e| e.to_string())?;
    // Use the actual stage/cleanup DTO fields, including defaults added later,
    // without relaxing strict validation of a user-supplied snapshot.
    let stage: OcrStageSpec = serde_json::from_value(serde_json::json!({})).map_err(|e| e.to_string())?;
    known["stages"] = serde_json::json!([stage]);
    validate_snapshot_keys(&value, &known, "")?;
    let config: OcrPipelineConfig = serde_json::from_value(value).map_err(|e| format!("Invalid OCR configuration: {e}"))?;
    crate::extractors::ocr_policy::validate_config(&config)?;
    Ok(config)
}

fn validate_snapshot_keys(value: &serde_json::Value, known: &serde_json::Value, path: &str) -> Result<(), String> {
    match known {
        serde_json::Value::Object(fields) => {
            let object = value.as_object().ok_or_else(|| format!("{} must be a JSON object.",
                if path.is_empty() { "OCR configuration" } else { path }))?;
            for (key, child) in object {
                let child_path = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
                let expected = fields.get(key).ok_or_else(|| format!("Unknown OCR configuration option: {child_path}"))?;
                validate_snapshot_keys(child, expected, &child_path)?;
            }
        }
        serde_json::Value::Array(items) if !items.is_empty() => {
            let values = value.as_array().ok_or_else(|| format!("OCR configuration option {path} must be an array."))?;
            for (index, child) in values.iter().enumerate() {
                validate_snapshot_keys(child, &items[0], &format!("{path}[{index}]"))?;
            }
        }
        _ => {} // serde validates scalar types and nullability next.
    }
    Ok(())
}

pub(super) fn overlay_options(
    mut saved: ExtractOptions,
    flags: OcrPipelineConfig,
    explicit: &HashSet<String>,
    source_type: &str,
) -> Result<ExtractOptions, String> {
    let automatic = saved.ocr_pipeline.engine.as_deref().map(str::trim)
        .is_none_or(|engine| engine.is_empty() || engine == "auto");
    // Explicit flat recognition weights create a new Automatic recipe. Only
    // preparation/control changes should pin Automatic's current runtime
    // default; an explicitly saved tier still retains its engine precedence.
    let flat_recipe_override = automatic && saved.ocr_tier == OcrTier::Auto
        && ["rec_model", "vlm_ocr_model", "vlm_ocr_engine"].iter().any(|arg| explicit.contains(*arg));
    if !explicit.contains("engine") && !saved.ocr_pipeline.enabled && automatic
        && has_processing_override(explicit) && !flat_recipe_override {
        // Preserve the current effective recognizer, including Automatic's
        // native fallback in builds without CrispEmbed, before enabled=true
        // changes precedence. Only copy resolved recognition state: disabled
        // old stages/weights must stay inactive, while preparation settings
        // remain available for explicitly requested processing steps.
        let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(&saved)?;
        saved.ocr_pipeline.engine = resolved.engine;
        saved.ocr_pipeline.stages = resolved.stages;
        saved.ocr_pipeline.det_model = resolved.det_model;
        saved.ocr_pipeline.rec_model = resolved.rec_model;
        saved.ocr_pipeline.vlm_ocr_model = resolved.vlm_ocr_model;
    }
    saved.ocr_pipeline = overlay(saved.ocr_pipeline, flags, explicit, source_type)?;
    // An explicit Automatic choice must not be reinterpreted as a saved
    // legacy tier when the shared resolver selects its runtime default.
    if explicit.contains("engine") { saved.ocr_tier = OcrTier::Auto; }
    Ok(saved)
}

fn has_processing_override(explicit: &HashSet<String>) -> bool {
    explicit.contains("source_type") || FIELDS.iter().any(|(arg, _)|
        !["engine", "language", "cpu_pixel_budget"].contains(arg) && explicit.contains(*arg))
}

pub(super) fn overlay(
    mut saved: OcrPipelineConfig,
    flags: OcrPipelineConfig,
    explicit: &HashSet<String>,
    source_type: &str,
) -> Result<OcrPipelineConfig, String> {
    if explicit.contains("source_type") && source_type != "auto" {
        return Err("--source-type cannot force a fixed source class through the shared OCR policy. Use --ocr-config with an enabled Automatic source-routed stage recipe, or omit --source-type for single-engine recognition.".into());
    }
    if !saved.enabled && has_processing_override(explicit) {
        // An explicit processing flag activates that request, not hidden
        // retained restoration/postprocessing from a disabled app recipe.
        let selected = if explicit.contains("engine") { flags.engine.clone() } else { saved.engine.clone() };
        let matching = selected.as_deref().filter(|engine| *engine != "auto")
            .and_then(|engine| saved.stages.iter().find(|stage| stage.engine == engine)).cloned();
        let preserve_models = selected.as_deref().is_some_and(|engine| engine != "auto");
        saved.engine = selected;
        saved.enabled = true;
        saved.cleanup_enabled = false;
        saved.denoise = false;
        saved.layout = false;
        saved.restore = false;
        saved.sr = false;
        saved.dewarp = false;
        // These model fields themselves enable postprocessing. Preparation
        // model paths/engines/tuning are inert while their switches are off,
        // so retain those for an explicitly enabled --restore/--sr/etc.
        saved.punct_model = None;
        saved.truecase_model = None;
        saved.lid_model = None;
        saved.tess_model_dir = None;
        if !preserve_models {
            saved.det_model = None; saved.rec_model = None; saved.vlm_ocr_model = None;
        }
        saved.stages = matching.into_iter().map(|mut stage| {
            stage.cleanup.enabled = false; stage.cleanup.denoise = false; stage
        }).collect();
    }
    let mut value = serde_json::to_value(&saved).map_err(|e| e.to_string())?;
    let provided = serde_json::to_value(&flags).map_err(|e| e.to_string())?;
    for (arg, field) in FIELDS {
        if explicit.contains(*arg) { value[*field] = provided[*field].clone(); }
    }
    let mut config: OcrPipelineConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if explicit.contains("source_type") { config.router = source_type == "auto"; }
    let engine = config.engine.as_deref().unwrap_or("auto").to_owned();
    let model_or_gate = ["det_model", "rec_model", "min_chars", "min_confidence"]
        .iter().any(|arg| explicit.contains(*arg));
    if crate::extractors::ocr_policy::LEGACY_ENGINES.contains(&engine.as_str()) && model_or_gate {
        return Err("Detection/recognition model and acceptance-gate flags require an embedded OCR engine.".into());
    }
    if crate::extractors::ocr_policy::VISION_ENGINES.contains(&engine.as_str()) && explicit.contains("rec_model") {
        return Err("This single-model OCR engine uses --model (or --det-model); --rec-model applies to detector/recognizer engines.".into());
    }
    if explicit.contains("rec_model") && !explicit.contains("language") {
        // A custom recognizer owns its language selection. Do not let a saved
        // language replace this request's weights in shared apply_language.
        config.language = None;
    }
    let stage_override = model_or_gate || explicit.contains("cleanup") || explicit.contains("denoise");
    if stage_override {
        let mut index = if engine == "auto" { (!config.stages.is_empty()).then_some(0) }
            else { config.stages.iter().position(|stage| stage.engine == engine) };
        if index.is_none() && crate::extractors::ocr_policy::EMBEDDED_ENGINES.contains(&engine.as_str()) {
            let (resolved, _) = crate::extractors::ocr_policy::recognition_config(config.clone(), Some(&engine), None)?;
            if let Some(stage) = resolved.stages.into_iter().next() {
                config.stages.push(stage); index = Some(config.stages.len() - 1);
            }
        }
        if let Some(index) = index {
            let stage = &mut config.stages[index];
            if explicit.contains("det_model") { stage.det_model = flags.det_model.clone(); }
            if explicit.contains("rec_model") { stage.rec_model = flags.rec_model.clone(); }
            if explicit.contains("cleanup") { stage.cleanup.enabled = flags.cleanup_enabled; }
            if explicit.contains("denoise") { stage.cleanup.denoise = flags.denoise; }
            if explicit.contains("min_chars") { stage.min_chars = flags.min_chars; }
            if explicit.contains("min_confidence") { stage.min_confidence = flags.min_confidence; }
        }
    }
    if explicit.contains("rec_model") && explicit.contains("language") {
        let (resolved, _) = crate::extractors::ocr_policy::recognition_config(config.clone(), None, None)?;
        let model = resolved.stages.first().map(|stage| stage.rec_model.as_deref())
            .unwrap_or(resolved.rec_model.as_deref());
        if model != flags.rec_model.as_deref() {
            return Err("The explicit --language would replace --rec-model. Use --language auto to preserve these weights, or omit --rec-model to select a registered language model.".into());
        }
    }
    if ["vlm_ocr_model", "vlm_ocr_engine"].iter().any(|arg| explicit.contains(*arg)) {
        let (resolved, _) = crate::extractors::ocr_policy::recognition_config(config.clone(), None, None)?;
        if resolved.engine.as_deref() != Some("auto") || !resolved.stages.is_empty() {
            return Err("--vlm-ocr-model/--vlm-ocr-engine require flat-mode Automatic OCR with no advanced stages. Edit the stages in --ocr-config, or use --engine with --model for a single vision recognizer; legacy adapters cannot apply these weights.".into());
        }
    }
    if explicit.contains("source_type") {
        let (resolved, _) = crate::extractors::ocr_policy::recognition_config(config.clone(), None, None)?;
        if resolved.engine.as_deref() != Some("auto") || !resolved.router {
            return Err("--source-type auto requires a configured Automatic OCR chain that retains source routing. Use --ocr-config with enabled source-routed stages, or omit --source-type for single-engine recognition.".into());
        }
    }
    crate::extractors::ocr_policy::validate_config(&config)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn config(value: serde_json::Value) -> OcrPipelineConfig { serde_json::from_value(value).unwrap() }
    fn names(values: &[&str]) -> HashSet<String> { values.iter().map(|v| (*v).into()).collect() }

    #[test]
    fn parser_defaults_never_overwrite_the_saved_pipeline() {
        let saved = config(json!({"enabled":true,"engine":"qwen2vl","language":"deu","cpu_pixel_budget":true,
            "stages":[{"engine":"qwen2vl","det_model":"custom.gguf","vlm_prompt":"Keep columns","cleanup":{"enabled":false}}]}));
        let actual = overlay(saved.clone(), Default::default(), &names(&[]), "auto").unwrap();
        assert_eq!(serde_json::to_value(actual).unwrap(), serde_json::to_value(saved).unwrap());
    }

    #[test]
    fn explicit_stage_controls_preserve_other_stages_and_custom_prompts() {
        let saved = config(json!({"enabled":true,"engine":"auto","stages":[
            {"engine":"tesseract","rec_model":"first.gguf","cleanup":{"enabled":true}},
            {"engine":"qwen2vl","det_model":"vision.gguf","vlm_prompt":"Keep columns"}]}));
        let flags = config(json!({"engine":"tesseract","rec_model":"replacement.gguf","cleanup_enabled":false,"min_confidence":0.0}));
        let actual = overlay(saved, flags, &names(&["engine","rec_model","cleanup","min_confidence"]), "auto").unwrap();
        assert_eq!(actual.stages.len(), 2);
        assert_eq!(actual.stages[0].rec_model.as_deref(), Some("replacement.gguf"));
        assert!(!actual.stages[0].cleanup.enabled);
        assert_eq!(actual.stages[0].min_confidence, 0.0);
        assert_eq!(actual.stages[1].vlm_prompt, "Keep columns");
    }

    #[test]
    fn activating_one_step_does_not_revive_hidden_disabled_processing() {
        let saved = config(json!({"enabled":false,"engine":"tesseract","restore":true,"sr":true,
            "punct_model":"hidden.gguf","stages":[{"engine":"tesseract","rec_model":"custom.gguf",
            "cleanup":{"enabled":true,"denoise":true,"auto_crop":true}}]}));
        let actual = overlay(saved, config(json!({"dewarp":true})), &names(&["dewarp"]), "auto").unwrap();
        assert!(actual.enabled && actual.dewarp);
        assert!(!actual.restore && !actual.sr && !actual.cleanup_enabled && !actual.denoise);
        assert!(actual.punct_model.is_none());
        assert_eq!(actual.stages[0].rec_model.as_deref(), Some("custom.gguf"));
        assert!(!actual.stages[0].cleanup.enabled && !actual.stages[0].cleanup.denoise);
    }

    #[test]
    fn explicit_cpu_false_and_automatic_engine_are_real_overrides() {
        let saved = config(json!({"engine":"qwen2vl","cpu_pixel_budget":true}));
        let actual = overlay(saved, config(json!({"engine":"auto","cpu_pixel_budget":false})),
            &names(&["engine","cpu_pixel_budget"]), "auto").unwrap();
        assert_eq!(actual.engine.as_deref(), Some("auto"));
        assert!(!actual.cpu_pixel_budget);
    }

    #[test]
    fn explicit_automatic_replaces_saved_tiers_through_final_resolution() {
        let (automatic, _) = crate::extractors::ocr_policy::resolve_extraction_config(&ExtractOptions::default()).unwrap();
        for tier in [OcrTier::Tier1, OcrTier::Tier2, OcrTier::Tier3, OcrTier::Tier4] {
            let saved = ExtractOptions { ocr_tier: tier,
                ocr_pipeline: config(json!({"enabled":false,"engine":"qwen2vl"})), ..Default::default() };
            let actual = overlay_options(saved, config(json!({"engine":"auto"})), &names(&["engine"]), "auto").unwrap();
            assert_eq!(actual.ocr_tier, OcrTier::Auto);
            let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(&actual).unwrap();
            assert_eq!(resolved.engine, automatic.engine);
        }
        // An omitted engine still uses the selected legacy tier.
        let saved = ExtractOptions { ocr_tier: OcrTier::Tier2, ..Default::default() };
        let actual = overlay_options(saved, Default::default(), &names(&[]), "auto").unwrap();
        let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(&actual).unwrap();
        assert_eq!(resolved.engine.as_deref(), Some("ocrs"));
    }

    #[test]
    fn non_engine_processing_overrides_preserve_effective_legacy_tiers() {
        for (tier, engine) in [(OcrTier::Tier1, "system_tesseract"), (OcrTier::Tier2, "ocrs"), (OcrTier::Tier3, "paddle")] {
            for (flags, explicit, restore) in [
                (config(json!({"restore":true})), names(&["restore"]), true),
                (config(json!({"sr":false})), names(&["sr"]), false),
            ] {
                let saved = ExtractOptions { ocr_tier: tier,
                    ocr_pipeline: config(json!({"enabled":false,"engine":"auto","sr":true})), ..Default::default() };
                let actual = overlay_options(saved, flags, &explicit, "auto").unwrap();
                let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(&actual).unwrap();
                assert_eq!(resolved.engine.as_deref(), Some(engine));
                assert_eq!(resolved.restore, restore);
                assert!(!resolved.sr);
            }
        }
    }

    #[test]
    fn explicit_false_preserves_automatic_runtime_engine_without_reviving_disabled_stages() {
        // This expectation comes from the shared runtime policy, so the same
        // regression covers embedded Linux and native-only Windows builds.
        for engine in [None, Some("auto")] {
            let saved = ExtractOptions { ocr_tier: OcrTier::Auto,
                ocr_pipeline: config(json!({"enabled":false,"engine":engine,"sr":true,
                    "det_model":"hidden-det.gguf","rec_model":"hidden-rec.gguf",
                    "vlm_ocr_model":"hidden-vision.gguf",
                    "sr_model":"custom-sr.gguf","sr_engine":"safmn",
                    "stages":[{"engine":"tesseract","rec_model":"hidden-language.gguf"},
                        {"engine":"qwen2vl","det_model":"hidden-stage-vision.gguf"}]})), ..Default::default() };
            let (before, _) = crate::extractors::ocr_policy::resolve_extraction_config(&saved).unwrap();
            let actual = overlay_options(saved, config(json!({"sr":false})), &names(&["sr"]), "auto").unwrap();
            let (after, _) = crate::extractors::ocr_policy::resolve_extraction_config(&actual).unwrap();
            assert_eq!(after.engine, before.engine);
            assert_eq!(serde_json::to_value(&after.stages).unwrap(), serde_json::to_value(&before.stages).unwrap());
            assert_eq!(after.det_model, before.det_model);
            assert_eq!(after.rec_model, before.rec_model);
            assert_eq!(after.vlm_ocr_model, before.vlm_ocr_model);
            assert!(!after.sr);
            assert_eq!(after.sr_model.as_deref(), Some("custom-sr.gguf"));
            assert_eq!(after.sr_engine, "safmn");
        }
    }

    #[test]
    fn explicit_flat_models_create_an_automatic_recipe_instead_of_pinning_the_runtime_default() {
        for engine in [None, Some("auto")] {
            for (flags, explicit) in [
                (config(json!({"rec_model":"custom-rec.gguf"})), names(&["rec_model"])),
                (config(json!({"vlm_ocr_model":"custom-vision.gguf"})), names(&["vlm_ocr_model"])),
                (config(json!({"vlm_ocr_model":"custom-qwen3.gguf","vlm_ocr_engine":"qwen3vl"})),
                    names(&["vlm_ocr_model","vlm_ocr_engine"])),
            ] {
                let saved = ExtractOptions { ocr_tier:OcrTier::Auto,
                    ocr_pipeline:config(json!({"enabled":false,"engine":engine})), ..Default::default() };
                let actual = overlay_options(saved, flags.clone(), &explicit, "auto").unwrap();
                let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(&actual).unwrap();
                assert!(resolved.enabled && resolved.stages.is_empty());
                assert_eq!(resolved.engine.as_deref(), Some("auto"));
                assert_eq!(resolved.rec_model, flags.rec_model);
                assert_eq!(resolved.vlm_ocr_model, flags.vlm_ocr_model);
                assert_eq!(resolved.vlm_ocr_engine, flags.vlm_ocr_engine);
            }
        }
        // A deliberate Tier4 choice remains DBNet+TrOCR when its weights are
        // overridden; only saved Automatic permits the flat-recipe exception.
        let saved = ExtractOptions { ocr_tier:OcrTier::Tier4, ..Default::default() };
        let actual = overlay_options(saved, config(json!({"rec_model":"tier4-rec.gguf"})),
            &names(&["rec_model"]), "auto").unwrap();
        let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(&actual).unwrap();
        assert_eq!(resolved.engine.as_deref(), Some("dbnet_trocr"));
        assert_eq!(resolved.stages[0].rec_model.as_deref(), Some("tier4-rec.gguf"));
    }

    #[test]
    fn activating_preparation_retains_its_saved_models_engines_and_tuning() {
        let saved = config(json!({"enabled":false,"engine":"tesseract",
            "stages":[{"engine":"tesseract","rec_model":"custom-language.gguf"}],
            "restore":true,"restore_engine":"instructir","restore_model":"custom-restore.gguf","restore_task":"deblur",
            "sr":true,"sr_engine":"safmn","sr_model":"custom-sr.gguf","sr_max_short_side":731,
            "denoise":true,"nafnet_model":"custom-denoise.gguf",
            "layout":true,"layout_engine":"rtdetr","layout_model":"custom-layout.gguf","layout_threshold":0.0,
            "dewarp":true,"dewarp_engine":"tps","punct_model":"inactive-punctuation.gguf"}));
        let flags = config(json!({"restore":true,"sr":true,"denoise":true,"layout":true,"dewarp":true}));
        for requested in ["restore", "sr", "denoise", "layout", "dewarp"] {
            let pipeline = overlay(saved.clone(), flags.clone(), &names(&[requested]), "auto").unwrap();
            let options = ExtractOptions { ocr_pipeline: pipeline, ..Default::default() };
            let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(&options).unwrap();
            assert_eq!(resolved.restore, requested == "restore");
            assert_eq!(resolved.sr, requested == "sr");
            assert_eq!(resolved.denoise, requested == "denoise");
            assert_eq!(resolved.layout, requested == "layout");
            assert_eq!(resolved.dewarp, requested == "dewarp");
            assert_eq!(resolved.stages[0].rec_model.as_deref(), Some("custom-language.gguf"));
            assert_eq!(resolved.stages[0].cleanup.denoise, requested == "denoise");
            assert_eq!(resolved.restore_engine, "instructir");
            assert_eq!(resolved.restore_model.as_deref(), Some("custom-restore.gguf"));
            assert_eq!(resolved.restore_task, "deblur");
            assert_eq!(resolved.sr_engine, "safmn");
            assert_eq!(resolved.sr_model.as_deref(), Some("custom-sr.gguf"));
            assert_eq!(resolved.sr_max_short_side, 731);
            assert_eq!(resolved.nafnet_model.as_deref(), Some("custom-denoise.gguf"));
            assert_eq!(resolved.layout_model.as_deref(), Some("custom-layout.gguf"));
            assert_eq!(resolved.layout_threshold, 0.0);
            assert_eq!(resolved.dewarp_engine, "tps");
            assert!(resolved.punct_model.is_none());
        }
    }

    #[test]
    fn enabling_preparation_preserves_the_selected_flat_custom_model() {
        let saved = config(json!({"enabled":false,"engine":"qwen2vl","vlm_ocr_engine":"qwen2vl",
            "vlm_ocr_model":"custom-vision.gguf","restore":true,"min_confidence":0.0}));
        let actual = overlay(saved, config(json!({"dewarp":true})), &names(&["dewarp"]), "auto").unwrap();
        assert!(actual.enabled && actual.dewarp && !actual.restore);
        let (resolved, _) = crate::extractors::ocr_policy::recognition_config(actual, None, None).unwrap();
        assert_eq!(resolved.stages[0].det_model.as_deref(), Some("custom-vision.gguf"));
        assert_eq!(resolved.stages[0].min_confidence, 0.0);
    }

    #[test]
    fn explicit_recognizer_weights_survive_saved_language_and_reject_conflicting_flags() {
        let saved = config(json!({"enabled":true,"engine":"tesseract","language":"deu",
            "stages":[{"engine":"tesseract","rec_model":"previous.gguf"}]}));
        for (flags, explicit, expected) in [
            (config(json!({"rec_model":"custom.gguf"})), names(&["rec_model"]), "custom.gguf"),
            (config(json!({"rec_model":"custom.gguf","language":"auto"})), names(&["rec_model","language"]), "custom.gguf"),
            (config(json!({"rec_model":"tesseract-deu","language":"deu"})), names(&["rec_model","language"]), "tesseract-deu"),
        ] {
            let pipeline = overlay(saved.clone(), flags, &explicit, "auto").unwrap();
            let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(
                &ExtractOptions { ocr_pipeline:pipeline, ..Default::default() }).unwrap();
            assert_eq!(resolved.stages[0].rec_model.as_deref(), Some(expected));
        }
        let error = overlay(saved, config(json!({"rec_model":"custom.gguf","language":"deu"})),
            &names(&["rec_model","language"]), "auto").unwrap_err();
        assert!(error.contains("--language auto"));
    }

    #[test]
    fn explicit_incompatible_model_flags_are_not_ignored() {
        assert!(overlay(config(json!({"engine":"system_tesseract"})), config(json!({"rec_model":"custom.gguf"})),
            &names(&["rec_model"]), "auto").unwrap_err().contains("embedded"));
        assert!(overlay(config(json!({"engine":"qwen2vl"})), config(json!({"rec_model":"custom.gguf"})),
            &names(&["rec_model"]), "auto").unwrap_err().contains("single-model"));
    }

    #[test]
    fn escalation_flags_reject_advanced_stages_but_preserve_flat_mode() {
        let saved = config(json!({"enabled":true,"engine":"auto","stages":[
            {"engine":"tesseract","rec_model":"custom-language.gguf"},
            {"engine":"qwen2vl","det_model":"custom-vision.gguf","vlm_prompt":"Keep columns"}]}));
        let flags = config(json!({"vlm_ocr_model":"replacement.gguf","vlm_ocr_engine":"qwen3vl"}));
        for flag in ["vlm_ocr_model", "vlm_ocr_engine"] {
            let error = overlay(saved.clone(), flags.clone(), &names(&[flag]), "auto").unwrap_err();
            assert!(error.contains("--ocr-config") && error.contains("--model"));
        }
        let saved = config(json!({"enabled":true,"engine":"auto","rec_model":"custom-fast.gguf"}));
        let pipeline = overlay(saved, flags, &names(&["vlm_ocr_model","vlm_ocr_engine"]), "auto").unwrap();
        let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(
            &ExtractOptions { ocr_pipeline:pipeline, ..Default::default() }).unwrap();
        assert_eq!(resolved.engine.as_deref(), Some("auto"));
        assert!(resolved.stages.is_empty());
        assert_eq!(resolved.rec_model.as_deref(), Some("custom-fast.gguf"));
        assert_eq!(resolved.vlm_ocr_model.as_deref(), Some("replacement.gguf"));
        assert_eq!(resolved.vlm_ocr_engine, "qwen3vl");
    }

    #[test]
    fn escalation_flags_cannot_be_ignored_by_saved_tiers_or_explicit_engines() {
        for tier in [OcrTier::Tier1, OcrTier::Tier2, OcrTier::Tier3] {
            let saved = ExtractOptions { ocr_tier:tier, ..Default::default() };
            let error = overlay_options(saved, config(json!({"vlm_ocr_model":"vision.gguf"})),
                &names(&["vlm_ocr_model"]), "auto").unwrap_err();
            assert!(error.contains("--ocr-config") && error.contains("--model"));
        }
        for engine in ["paddle", "system_tesseract", "ocrs", "tesseract", "qwen2vl"] {
            let saved = config(json!({"enabled":false,"engine":engine}));
            assert!(overlay(saved, config(json!({"vlm_ocr_model":"vision.gguf"})),
                &names(&["vlm_ocr_model"]), "auto").unwrap_err().contains("flat-mode Automatic"));
        }
    }

    #[test]
    fn source_routing_flags_cannot_silently_lose_the_requested_source() {
        for source in ["photo", "scanned_doc", "screenshot"] {
            assert!(overlay(Default::default(), Default::default(), &names(&["source_type"]), source)
                .unwrap_err().contains("--ocr-config"));
        }
        for saved in [OcrPipelineConfig::default(), config(json!({"enabled":true,"engine":"tesseract"}))] {
            assert!(overlay(saved, Default::default(), &names(&["source_type"]), "auto")
                .unwrap_err().contains("--ocr-config"));
        }
        let saved = config(json!({"enabled":true,"engine":"auto","router":false,"stages":[
            {"engine":"tesseract","source_type":"scanned_doc","rec_model":"custom-language.gguf"},
            {"engine":"qwen2vl","source_type":"photo","det_model":"custom-vision.gguf"}]}));
        let pipeline = overlay(saved, Default::default(), &names(&["source_type"]), "auto").unwrap();
        let (resolved, _) = crate::extractors::ocr_policy::resolve_extraction_config(
            &ExtractOptions { ocr_pipeline:pipeline, ..Default::default() }).unwrap();
        assert_eq!(resolved.engine.as_deref(), Some("auto"));
        assert!(resolved.router);
        assert_eq!(resolved.stages[0].source_type, "scanned_doc");
        assert_eq!(resolved.stages[1].source_type, "photo");
    }

    #[test]
    fn snapshots_validate_shape_engine_and_bounds() {
        let parsed = parse_snapshot(br#"{"enabled":true,"engine":"qwen2vl","stages":[{"engine":"qwen2vl","vlm_prompt":"Keep columns"}]}"#).unwrap();
        assert_eq!(parsed.stages[0].vlm_prompt, "Keep columns");
        for bad in [br#"[]"#.as_slice(), br#"{"engnie":"got"}"#, br#"{"engine":"unknown"}"#] {
            assert!(parse_snapshot(bad).is_err());
        }
        assert!(parse_snapshot(&vec![b' '; MAX_CONFIG_BYTES as usize + 1]).is_err());
    }

    #[test]
    fn snapshots_reject_unknown_nested_stage_and_cleanup_fields() {
        for (input, field) in [
            (br#"{"enabled":true,"stages":[{"engnie":"qwen2vl"}]}"#.as_slice(), "stages[0].engnie"),
            (br#"{"stages":[{"cleanup":{"desckew":false}}]}"#, "stages[0].cleanup.desckew"),
        ] {
            assert!(parse_snapshot(input).unwrap_err().contains(field));
        }
        for input in [br#"{"stages":{}}"#.as_slice(), br#"{"stages":[null]}"#, br#"{"stages":[{"cleanup":[]}]}"#] {
            assert!(parse_snapshot(input).is_err());
        }
        let parsed = parse_snapshot(br#"{"enabled":true,"stages":[{"engine":"qwen2vl","min_chars":0,"min_confidence":0,"cleanup":{"enabled":false,"sauvola_k":0,"auto_crop":true}}]}"#).unwrap();
        assert_eq!(parsed.stages[0].min_chars, 0);
        assert_eq!(parsed.stages[0].min_confidence, 0.0);
        assert_eq!(parsed.stages[0].cleanup.sauvola_k, 0.0);
        assert!(parsed.stages[0].cleanup.auto_crop);
    }

    #[test]
    fn supplied_snapshot_owns_defaults_and_does_not_create_a_settings_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ocr.json");
        std::fs::write(&path, br#"{"engine":"system_tesseract","language":"eng+deu"}"#).unwrap();
        let absent = dir.path().join("unused-app-profile");
        let result = load(&absent, Some(&path)).unwrap();
        assert_eq!(result.ocr_pipeline.language.as_deref(), Some("eng+deu"));
        assert_eq!(result.ocr_tier, crate::extractors::OcrTier::Auto);
        assert!(result.try_ocr && result.image_extraction_enabled);
        assert_eq!(result.ocr_pdf_min_chars, usize::MAX);
        assert!(!absent.exists());
    }
}
