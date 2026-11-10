//! Compile the application's ZIP, mail, presentation, HTML and text extractors without model or UI dependencies.
#![allow(dead_code)]

#[derive(Debug, Default)]
pub struct ExtractedDocument {
    pub full_text: String,
    pub headings: Vec<String>,
    pub ext: String,
    pub language: Option<String>,
    pub translated_text: Option<String>,
    pub translated_to_lang: Option<String>,
    pub audio: Option<()>,
    pub image_exif: Option<()>,
    pub source_url: Option<String>,
    pub tags: Vec<String>,
    pub audio_pcm: Option<()>,
}

#[path = "../../src-tauri/src/extractors/eml.rs"]
pub mod eml;
#[path = "../../src-tauri/src/extractors/html.rs"]
pub mod html;
#[path = "../../src-tauri/src/extractors/text.rs"]
pub mod text;
#[path = "../../src-tauri/src/extractors/archive.rs"]
pub mod archive;
#[path = "../../src-tauri/src/extractors/pptx.rs"]
pub mod pptx;
#[path = "../../src-tauri/src/extractors/native_text.rs"]
pub mod native_text;
