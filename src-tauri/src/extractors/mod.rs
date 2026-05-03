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

/// Run the appropriate extractor for `path`. Returns an empty
/// `ExtractedDocument` for unsupported extensions rather than erroring
/// — callers can pre-filter via `supported(ext)` if they want to skip.
pub fn extract_text_from_path(path: &Path) -> Result<ExtractedDocument> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => pdf::extract(path).map(|mut doc| {
            doc.ext = ext;
            doc
        }),
        "html" | "htm" => html::extract(path).map(|mut doc| {
            doc.ext = ext;
            doc
        }),
        e if supported(e) => text::extract(path).map(|mut doc| {
            doc.ext = ext;
            doc
        }),
        _ => Err(anyhow::anyhow!(
            "no extractor for `.{ext}` ({})",
            path.display()
        )),
    }
    .with_context(|| format!("extracting {}", path.display()))
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
