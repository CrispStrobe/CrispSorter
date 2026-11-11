//! Ephemeral binary-image OCR for frontend image and PDF-page extraction.
//! Uses the same saved policy and native dispatcher as background ingestion;
//! it does not create persistent OCR-workspace assets.

use crate::{extractors, AppState};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::path::Path;
use tauri::State;

const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_IMAGE_PIXELS: u64 = 100_000_000;

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct RecognitionOptions {
    engine: Option<String>,
    language: Option<String>,
    config: Option<extractors::OcrPipelineConfig>,
    cpu_pixel_budget: Option<bool>,
    ocr_tier: Option<String>,
    ocr_rec_lang: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImageMetadata {
    name: String,
    #[serde(default)]
    options: RecognitionOptions,
}

#[derive(Serialize)]
pub struct ImageRecognition {
    text: String,
    engine: String,
    warnings: Vec<String>,
}

fn parse_metadata(header: &[u8], bytes: &[u8]) -> Result<ImageMetadata, String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err("Choose a nonempty image no larger than 32 MiB.".into());
    }
    if header.len() > MAX_METADATA_BYTES.div_ceil(3) * 4 {
        return Err("OCR metadata exceeds 64 KiB.".into());
    }
    let json = base64::engine::general_purpose::STANDARD.decode(header)
        .map_err(|_| "Invalid OCR metadata encoding.")?;
    if json.len() > MAX_METADATA_BYTES { return Err("OCR metadata exceeds 64 KiB.".into()); }
    let metadata: ImageMetadata = serde_json::from_slice(&json).map_err(|e| format!("Invalid OCR metadata: {e}"))?;
    if metadata.name.is_empty() || metadata.name.len() > 4096 || metadata.name.contains('\0') {
        return Err("Choose a valid image name.".into());
    }
    Ok(metadata)
}

fn extraction_options(mut saved: extractors::ExtractOptions, request: RecognitionOptions, source_name: &str) -> Result<extractors::ExtractOptions, String> {
    if let Some(config) = request.config { saved.ocr_pipeline = config; }
    if let Some(engine) = request.engine { saved.ocr_pipeline.engine = Some(engine); }
    if let Some(language) = request.language { saved.ocr_pipeline.language = Some(language); }
    if let Some(budget) = request.cpu_pixel_budget { saved.ocr_pipeline.cpu_pixel_budget = budget; }
    if let Some(tier) = request.ocr_tier {
        saved.ocr_tier = match tier.as_str() {
            "auto" => extractors::OcrTier::Auto,
            "tier1" => extractors::OcrTier::Tier1,
            "tier2" => extractors::OcrTier::Tier2,
            "tier3" => extractors::OcrTier::Tier3,
            "tier4" => extractors::OcrTier::Tier4,
            _ => return Err(format!("Unknown OCR tier: {tier}")),
        };
    }
    if let Some(language) = request.ocr_rec_lang {
        saved.ocr_rec_lang = match language.as_str() {
            "auto" => extractors::OcrRecLang::Auto,
            "latin" => extractors::OcrRecLang::Latin,
            "cjk" => extractors::OcrRecLang::Cjk,
            _ => return Err(format!("Unknown OCR recognition language: {language}")),
        };
    }
    // Staging uses an opaque filename; preserve automatic script selection
    // from the original label before that label leaves the recognition path.
    if saved.ocr_rec_lang == extractors::OcrRecLang::Auto {
        saved.ocr_rec_lang = extractors::ocr_paddle::paddle_language_for_name(source_name);
    }
    saved.try_ocr = true;
    saved.ingest_image_level = "l3".into();
    saved.image_extraction_enabled = true;
    Ok(saved)
}

/// Content determines the staged extension; a supplied name never becomes a
/// filesystem path. The temporary owner outlives recognition and drops on
/// success, validation failure, or a model/recognizer error.
fn with_image<T>(bytes: &[u8], recognize: impl FnOnce(&Path) -> Result<T, String>) -> Result<T, String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES { return Err("Image exceeds the 32 MiB input limit.".into()); }
    let format = image::guess_format(bytes).map_err(|e| format!("Identify OCR image: {e}"))?;
    let extension = match format {
        image::ImageFormat::Png => "png", image::ImageFormat::Jpeg => "jpg",
        image::ImageFormat::WebP => "webp", image::ImageFormat::Bmp => "bmp",
        image::ImageFormat::Tiff => "tiff", image::ImageFormat::Gif => "png",
        _ => return Err("OCR supports PNG, JPEG, WebP, BMP, TIFF and single-frame GIF images.".into()),
    };
    extractors::image_input::validate(Cursor::new(bytes)).map_err(|e| format!("Validate OCR image: {e}"))?;
    let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()
        .map_err(|e| format!("Read OCR image dimensions: {e}"))?;
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err("OCR image exceeds the 100 megapixel limit.".into());
    }
    let directory = tempfile::Builder::new().prefix("crispsorter-ocr-").tempdir()
        .map_err(|e| format!("Create temporary OCR input: {e}"))?;
    let path = directory.path().join(format!("source.{extension}"));
    if format == image::ImageFormat::Gif {
        image::load_from_memory_with_format(bytes, format).map_err(|e| format!("Decode GIF: {e}"))?
            .save_with_format(&path, image::ImageFormat::Png).map_err(|e| format!("Stage GIF frame: {e}"))?;
    } else {
        std::fs::write(&path, bytes).map_err(|e| format!("Stage OCR image: {e}"))?;
    }
    recognize(&path)
}

#[tauri::command]
pub async fn ocr_extract_image(state: State<'_, AppState>, request: tauri::ipc::Request<'_>) -> Result<ImageRecognition, String> {
    let request_id = request.headers().get("x-operation-id").and_then(|value|value.to_str().ok());
    let log = std::sync::Arc::new(crate::operation_log::OperationLog::start_with_id("ocr.raw_image",
        serde_json::json!({"surface":"gui","kind":"image"}), request_id));
    let result: Result<ImageRecognition,String> = async {
    log.step("validating_request", serde_json::Value::Null);
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("Image OCR requires a raw binary payload.".into());
    };
    let header = request.headers().get("x-ocr-metadata").ok_or("Missing OCR metadata.")?;
    let metadata = parse_metadata(header.as_bytes(), bytes)?;
    let mut owned = Vec::new();
    owned.try_reserve_exact(bytes.len()).map_err(|_| "Not enough memory for this OCR image.")?;
    owned.extend_from_slice(bytes);
    log.step("snapshot_settings", serde_json::json!({"input_bytes":bytes.len()}));
    let saved = state.bg_ingest.lock().await.ocr_options();
    let options = extraction_options(saved, metadata.options, &metadata.name)?;
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    let worker_log=log.clone();
    tokio::task::spawn_blocking(move || {
        let (config, warnings) = extractors::ocr_policy::resolve_extraction_config(&options)?;
        let engine = config.engine.clone().unwrap_or_else(|| "auto".into());
        worker_log.step("policy_resolved",serde_json::json!({"engine":engine,"language":config.language,
            "stages":config.stages.len(),"cpu_budget":config.cpu_pixel_budget}));
        worker_log.step("staging_image",serde_json::json!({"bytes":owned.len()}));
        with_image(&owned, |path| {
            let document = extractors::extract_text_from_path_with_opts(path, options)
                .map_err(|e| format!("Image OCR failed: {e:#}"))?;
            Ok(ImageRecognition { text: document.full_text, engine, warnings })
        })
    }).await.map_err(|e| format!("Image OCR worker: {e}"))?
    }.await;
    match &result {
        Ok(image) => log.succeed(serde_json::json!({"engine":image.engine,"characters":image.text.chars().count(),"warnings":image.warnings.len()})),
        Err(_) => log.fail("image_request_failed"),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(value: serde_json::Value) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&value).unwrap()).into_bytes()
    }

    #[test]
    fn metadata_preserves_unicode_and_rejects_unknown_options() {
        let parsed = parse_metadata(&header(serde_json::json!({"name":"Überprüfung.png","options":{"engine":"qwen2vl","cpuPixelBudget":true}})), &[1]).unwrap();
        assert_eq!(parsed.name, "Überprüfung.png");
        assert_eq!(parsed.options.engine.as_deref(), Some("qwen2vl"));
        assert_eq!(parsed.options.cpu_pixel_budget, Some(true));
        assert!(parse_metadata(&header(serde_json::json!({"name":"scan","options":{"engnie":"got"}})), &[1]).is_err());
        assert!(parse_metadata(b"invalid base64!", &[1]).is_err());
        assert!(parse_metadata(&header(serde_json::json!({"name":"scan"})), &[]).is_err());
        assert!(parse_metadata(&vec![b'A'; MAX_METADATA_BYTES.div_ceil(3) * 4 + 1], &[1]).is_err());
    }

    #[test]
    fn explicit_options_override_snapshot_and_omission_preserves_it() {
        let mut saved = extractors::ExtractOptions::default();
        saved.ocr_pipeline.engine = Some("tesseract".into());
        saved.ocr_pipeline.language = Some("deu".into());
        saved.ocr_pipeline.cpu_pixel_budget = true;
        let request: RecognitionOptions = serde_json::from_value(serde_json::json!({"engine":"qwen2vl","cpuPixelBudget":false,"ocrTier":"tier4"})).unwrap();
        let result = extraction_options(saved, request, "scan.png").unwrap();
        assert_eq!(result.ocr_pipeline.engine.as_deref(), Some("qwen2vl"));
        assert_eq!(result.ocr_pipeline.language.as_deref(), Some("deu"));
        assert!(!result.ocr_pipeline.cpu_pixel_budget);
        assert_eq!(result.ocr_tier, extractors::OcrTier::Tier4);
        assert!(result.try_ocr);
        assert_eq!(result.ingest_image_level, "l3");
        assert!(result.image_extraction_enabled);
        assert!(extraction_options(Default::default(), serde_json::from_value(serde_json::json!({"ocrTier":"unknown"})).unwrap(), "scan.png").is_err());
        let cjk = extraction_options(Default::default(), Default::default(), "日本語領収書.png").unwrap();
        assert_eq!(cjk.ocr_rec_lang, extractors::OcrRecLang::Cjk);
        let explicit = extraction_options(Default::default(), serde_json::from_value(serde_json::json!({"ocrRecLang":"latin"})).unwrap(), "日本語領収書.png").unwrap();
        assert_eq!(explicit.ocr_rec_lang, extractors::OcrRecLang::Latin);
    }

    #[test]
    fn temporary_image_is_content_named_and_removed_after_failure() {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let mut staged = None;
        let error = with_image(bytes.get_ref(), |path| -> Result<(), String> {
            assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("png"));
            assert_eq!(image::image_dimensions(path).unwrap(), (2, 3));
            staged = Some(path.to_owned());
            Err("selected model unavailable".into())
        }).unwrap_err();
        assert_eq!(error, "selected model unavailable");
        assert!(!staged.unwrap().exists());
        assert!(with_image(b"not an image", |_| Ok(())).is_err());
    }
}
