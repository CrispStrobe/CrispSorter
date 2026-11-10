//! Optional extracted metadata shared by foreground local and remote ingestion.
use anyhow::{ensure, Result};

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestMetadata {
    pub parent_dir: Option<String>,
    pub volume_id: Option<String>,
    pub url: Option<String>,
    pub text_translated: Option<String>,
    pub text_translated_lang: Option<String>,
    pub audio_duration_seconds: Option<f64>,
    pub image_camera_make: Option<String>,
    pub image_camera_model: Option<String>,
}

impl IngestMetadata {
    pub fn validate(&self) -> Result<()> {
        for value in [self.parent_dir.as_ref(), self.url.as_ref()].into_iter().flatten() {
            ensure!(value.len() <= 8192 && !value.contains('\0'), "Source metadata paths and URLs must contain at most 8192 bytes and no NUL characters");
        }
        for value in [self.volume_id.as_ref(), self.text_translated_lang.as_ref(), self.image_camera_make.as_ref(), self.image_camera_model.as_ref()].into_iter().flatten() {
            ensure!(!value.trim().is_empty() && value.len() <= 1024 && !value.contains('\0'), "Source metadata values must contain 1–1024 bytes and no NUL characters");
        }
        if let Some(seconds) = self.audio_duration_seconds {
            ensure!(seconds.is_finite() && seconds >= 0.0, "Audio duration must be a finite non-negative number of seconds");
        }
        ensure!(self.text_translated.is_some() == self.text_translated_lang.is_some(), "A translation requires both text and its target language");
        if let Some(text) = &self.text_translated {
            ensure!(!text.trim().is_empty() && text.len() <= 16 * 1024 * 1024, "Translated text must contain 1 byte to 16 MiB of searchable content");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_payloads_and_zero_duration_remain_valid() {
        let old: IngestMetadata = serde_json::from_str("{}").unwrap();
        old.validate().unwrap();
        let value: IngestMetadata = serde_json::from_value(serde_json::json!({
            "parentDir": "/records/100%_complete", "volumeId": "source-volume",
            "url": "https://example.test/source", "audioDurationSeconds": 0.0,
            "imageCameraMake": "Camera", "imageCameraModel": "Model_1"
        })).unwrap();
        value.validate().unwrap();
        assert_eq!(value.audio_duration_seconds, Some(0.0));
        assert_eq!(value.parent_dir.as_deref(), Some("/records/100%_complete"));
    }

    #[test]
    fn incomplete_translations_and_invalid_metadata_are_rejected() {
        for metadata in [
            IngestMetadata { audio_duration_seconds: Some(f64::NAN), ..Default::default() },
            IngestMetadata { audio_duration_seconds: Some(-1.0), ..Default::default() },
            IngestMetadata { text_translated_lang: Some("en".into()), ..Default::default() },
            IngestMetadata { text_translated: Some("translation".into()), ..Default::default() },
            IngestMetadata { volume_id: Some(" ".into()), ..Default::default() },
            IngestMetadata { parent_dir: Some("/source\0alias".into()), ..Default::default() },
        ] { assert!(metadata.validate().is_err()); }
        IngestMetadata { text_translated: Some("Translated evidence".into()), text_translated_lang: Some("en".into()), ..Default::default() }.validate().unwrap();
    }
}
