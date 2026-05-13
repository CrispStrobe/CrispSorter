//! Plain-text + source-code extractor — UTF-8 read, no transformation.
//!
//! Markdown gets a tiny pre-pass that lifts ATX-style headings
//! (`# Heading`, `## Heading`) into the `headings` field so the FTS
//! `headings_text` column sees them. Anything else passes through
//! verbatim — log files, CSVs, JSON dumps, source code, etc.

use anyhow::{Context, Result};
use std::path::Path;

use super::ExtractedDocument;

pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    // Use `read` + `String::from_utf8_lossy` so a file with stray
    // non-UTF-8 bytes (typical in old logs or mixed-encoding source)
    // produces a best-effort string instead of an error.
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let full_text = String::from_utf8_lossy(&bytes).into_owned();

    // Markdown ATX headings — only fire on .md/.markdown to avoid
    // false-positive `#` lines in source code or YAML.
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let headings = if matches!(ext.as_str(), "md" | "markdown") {
        full_text
            .lines()
            .filter(|line| line.trim_start().starts_with('#'))
            .map(|line| line.trim_start_matches('#').trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    };

    Ok(ExtractedDocument {
        full_text,
        headings,
        ext: String::new(),            // dispatcher fills
        language: None,                // post-LID hook fills
        translated_text: None,         // post-translate hook fills
        translated_to_lang: None,
        audio: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn extracts_plain_text_verbatim() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("a.txt");
        std::fs::write(&p, b"line one\nline two\n").unwrap();
        let d = extract(&p).unwrap();
        assert_eq!(d.full_text, "line one\nline two\n");
        assert!(d.headings.is_empty());
    }

    #[test]
    fn lifts_markdown_atx_headings() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("a.md");
        std::fs::write(
            &p,
            b"# Title\n\nSome body.\n\n## Subtitle\n\nMore body.\n#NotAHeading",
        )
        .unwrap();
        let d = extract(&p).unwrap();
        assert_eq!(d.headings, vec!["Title", "Subtitle", "NotAHeading"]);
        // The body text is still preserved.
        assert!(d.full_text.contains("Some body."));
    }

    #[test]
    fn lossy_decode_survives_non_utf8() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("a.log");
        // Mix valid UTF-8 with a stray latin-1 byte.
        std::fs::write(&p, b"hello \xe4 world").unwrap();
        let d = extract(&p).unwrap();
        // Non-UTF-8 byte gets replaced with U+FFFD, but extraction
        // succeeds.
        assert!(d.full_text.contains("hello"));
        assert!(d.full_text.contains("world"));
    }
}
