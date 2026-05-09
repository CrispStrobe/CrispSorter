//! Per-filetype text-extraction registry.
//!
//! Phase 7.4.1 of PLAN P7. This module provides a single uniform entry
//! point `extract_text_from_path` that dispatches to a concrete
//! extractor based on the file's extension. The result is an
//! `ExtractedDocument` carrying the document's plain-text body plus
//! any headings the extractor was able to lift out — both fed into
//! the existing index ingest pipeline (full_text → embedding + body
//! field; headings_text → boosted Tantivy field).
//!
//! The registry is intentionally trait-free for now: dispatch is a
//! single match on the extension. The trait abstraction would be
//! useful if extractors needed to share state or be hot-swappable
//! at runtime, but neither is true today — every extractor is a
//! pure function file-path-in / text-out. We can promote to a trait
//! the moment we need that.
//!
//! Currently supported file types:
//! * **PDF** via the existing `pdf-extract` dep — same code path the
//!   `extract_pdf_native` Tauri command already uses.
//! * **Text + source** — UTF-8 read, no transformation. Covers .txt,
//!   .md, .rst, .log, .csv, .tsv, .json, .yaml, .toml, .xml, .html,
//!   plus most source-code extensions.
//! * **HTML** — basic tag-stripping via the regex crate. Lower
//!   fidelity than scraper but zero new heavy deps.
//!
//! Deferred to follow-ups (heavier deps): docx (zip + xml-rs), epub
//! (epub crate), rtf (rtf-grimoire). Once those land, this module
//! grows new dispatch arms; the public API stays the same.

use anyhow::{Context, Result};
use std::path::Path;

pub mod html;
pub mod ocr;
pub mod ocr_ocrs;
pub mod ocr_paddle;
pub mod pdf;
pub mod text;

/// One file's extracted text + structural breadcrumbs.
#[derive(Debug, Clone, Default)]
pub struct ExtractedDocument {
    /// Plain text body. Suitable as input to the embedder + as the
    /// `full_text` column in the documents table.
    pub full_text: String,
    /// Headings the extractor was able to find. Joined with newlines
    /// and fed into the boosted `headings_text` Tantivy field.
    pub headings: Vec<String>,
    /// Lowercased extension that was used for dispatch (e.g. `"pdf"`).
    /// Useful for downstream code that wants to differentiate by
    /// origin without re-parsing the path.
    pub ext: String,
}

/// Image extensions that OCR can handle. Surface them to `supported`
/// only when the caller opts in via `extract_text_from_path_with_opts`
/// — the no-OCR default would silently produce empty text for these
/// otherwise.
pub const OCR_IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "tif", "tiff", "bmp", "webp",
];

/// Extension classes the registry knows how to handle. Used as the
/// dispatch key so the match arms read top-down by category.
pub fn supported(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "pdf"
            // Plain text + light markup
            | "txt" | "md" | "markdown" | "rst" | "log"
            | "csv" | "tsv" | "json" | "jsonl"
            | "yaml" | "yml" | "toml" | "xml"
            // HTML gets its own arm (tag-strip), but list it here too
            // so callers can pre-filter accept lists with `supported`.
            | "html" | "htm"
            // Source code (UTF-8 read)
            | "rs" | "py" | "js" | "ts" | "tsx" | "jsx"
            | "svelte" | "vue"
            | "go" | "java" | "kt" | "swift" | "scala"
            | "c" | "cpp" | "cc" | "cxx" | "h" | "hpp"
            | "rb" | "php" | "lua" | "r"
            | "sh" | "bash" | "zsh" | "fish"
            | "sql" | "graphql"
    )
}

/// Which OCR tier to try, in descending quality order.
///
/// `Auto` = try the best available tier at runtime:
///   Tier 3 (PaddleOCR) if compiled in → Tier 2 (ocrs) if models present →
///   Tier 1 (Tesseract) if installed → nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OcrTier {
    /// Pick the best available tier automatically.
    #[default]
    Auto,
    /// Tier 1 — Tesseract shell-out.
    Tier1,
    /// Tier 2 — ocrs (pure Rust, Latin-script).
    Tier2,
    /// Tier 3 — PaddleOCR via usls (requires `paddle-ocr` feature).
    Tier3,
}

/// Which recognition language model to use for PaddleOCR Tier 3.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OcrRecLang {
    /// Automatically detect from filename heuristics.
    #[default]
    Auto,
    /// Latin-script languages (EN, DE, FR, …) — `ppocr_rec_v4_en`.
    Latin,
    /// CJK (Chinese, Japanese, Korean) — `ppocr_rec_v4_ch`.
    Cjk,
}

/// PLAN P7.8 options for the extractor dispatcher.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractOptions {
    /// Run OCR on image extensions (png/jpg/tiff/…) and on PDFs whose
    /// text layer is empty after the regular `pdf::extract` pass.
    /// Off by default — OCR is CPU-heavy and most catalogs don't need it.
    pub try_ocr: bool,
    /// PDFs with fewer than this many extracted characters fall through
    /// to OCR if `try_ocr` is on.
    pub ocr_pdf_min_chars: usize,
    /// Which OCR tier to use. Default `Auto` picks the best available.
    pub ocr_tier: OcrTier,
    /// Which recognition language model to use for PaddleOCR.
    /// `Auto` uses the filename path to guess CJK vs. Latin.
    pub ocr_rec_lang: OcrRecLang,
}

/// Run the appropriate extractor for `path`. Returns an empty
/// `ExtractedDocument` for unsupported extensions rather than erroring
/// — callers can pre-filter via `supported(ext)` if they want to skip.
pub fn extract_text_from_path(path: &Path) -> Result<ExtractedDocument> {
    extract_text_from_path_with_opts(
        path,
        ExtractOptions {
            try_ocr: false,
            ocr_pdf_min_chars: 50,
            ocr_tier: OcrTier::Auto,
            ocr_rec_lang: OcrRecLang::Auto,
        },
    )
}

/// Variant that takes the OCR opt-in. Calling sites in bg_ingest +
/// CLI thread the user's catalog-level setting through here.
pub fn extract_text_from_path_with_opts(
    path: &Path,
    opts: ExtractOptions,
) -> Result<ExtractedDocument> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let result: Result<ExtractedDocument> = match ext.as_str() {
        "pdf" => {
            let mut doc = pdf::extract(path)?;
            doc.ext = ext.clone();
            // PLAN P7.8 — fall through to OCR if the PDF's text layer
            // is empty / near-empty (typical for scanned documents).
            if opts.try_ocr
                && doc.full_text.trim().chars().count() < opts.ocr_pdf_min_chars
            {
                if let Ok(mut ocr) = ocr::ocr_via_tesseract(path) {
                    ocr.ext = ext.clone();
                    Ok(ocr)
                } else {
                    Ok(doc)
                }
            } else {
                Ok(doc)
            }
        }
        "html" | "htm" => html::extract(path).map(|mut doc| {
            doc.ext = ext.clone();
            doc
        }),
        e if OCR_IMAGE_EXTS.contains(&e) && opts.try_ocr => {
            // PLAN P7.8 — tiered OCR for images.
            // Tier 3 (PaddleOCR, best quality, requires --features paddle-ocr)
            // → Tier 2 (ocrs, pure Rust, Latin-script)
            // → Tier 1 (Tesseract, system install).
            let want_tier3 = matches!(opts.ocr_tier, OcrTier::Auto | OcrTier::Tier3);
            let want_tier2 = matches!(opts.ocr_tier, OcrTier::Auto | OcrTier::Tier2);

            if want_tier3 && ocr_paddle::is_paddle_ocr_available() {
                if let Ok(mut doc) = ocr_paddle::ocr_via_paddle(path, opts.ocr_rec_lang) {
                    doc.ext = ext.clone();
                    return Ok(doc);
                }
            }
            if want_tier2 && ocr_ocrs::is_ocrs_available() {
                if let Ok(mut doc) = ocr_ocrs::ocr_via_ocrs(path) {
                    doc.ext = ext.clone();
                    return Ok(doc);
                }
            }
            ocr::ocr_via_tesseract(path).map(|mut doc| {
                doc.ext = ext.clone();
                doc
            })
        }
        e if supported(e) => text::extract(path).map(|mut doc| {
            doc.ext = ext.clone();
            doc
        }),
        _ => Err(anyhow::anyhow!(
            "no extractor for `.{ext}` ({})",
            path.display()
        )),
    };
    result.with_context(|| format!("extracting {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_recognises_common_types() {
        assert!(supported("pdf"));
        assert!(supported("txt"));
        assert!(supported("md"));
        assert!(supported("rs"));
        assert!(supported("HTML")); // case-insensitive
        assert!(!supported("docx")); // deferred
        assert!(!supported("zip"));
        assert!(!supported(""));
    }

    #[test]
    fn extract_dispatches_on_extension() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("hello.txt");
        std::fs::write(&p, b"hello world").unwrap();
        let doc = extract_text_from_path(&p).unwrap();
        assert_eq!(doc.full_text, "hello world");
        assert_eq!(doc.ext, "txt");
    }

    #[test]
    fn unknown_extension_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("file.xyz");
        std::fs::write(&p, b"opaque").unwrap();
        let res = extract_text_from_path(&p);
        assert!(res.is_err());
    }
}
