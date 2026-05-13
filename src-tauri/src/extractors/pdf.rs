//! PDF extractor — wraps the existing `pdf-extract` dep so the
//! per-filetype registry has a uniform call shape.
//!
//! No new dependency: `pdf-extract` is already pulled in for the
//! existing `extract_pdf_native` Tauri command. This module just
//! gives it the `Extractor`-like interface the registry uses.

use anyhow::Result;
use std::path::Path;

use super::ExtractedDocument;

pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    let text = pdf_extract::extract_text(path)?;
    Ok(ExtractedDocument {
        full_text: text,
        // `pdf_extract` doesn't surface heading structure — leaving
        // empty for now. A future improvement: lift heading-shaped
        // lines (single-sentence-per-line, larger-than-body-font, …)
        // via lopdf's content-stream walk. The existing
        // `extract_pdf_metadata` already opens lopdf for /Info dict;
        // reusing that load here would be a free win.
        headings: Vec::new(),
        // Filled in by the dispatcher.
        ext: String::new(),
        // Filled in by the dispatcher's post-LID hook when an
        // `ExtractOptions.text_lid_model` was supplied.
        language: None,
        // Filled in by the dispatcher's post-translate hook when
        // an `ExtractOptions.translate_to` was supplied.
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
    })
}
