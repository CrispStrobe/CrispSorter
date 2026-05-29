//! Plain-text + source-code extractor — UTF-8 read, no transformation.
//!
//! Markdown gets two pre-passes:
//! 1. ATX-style headings (`# Heading`) lifted into `headings` so the
//!    FTS `headings_text` column sees them.
//! 2. YAML frontmatter (`---\n…\n---\n`) parsed for `url:` so the
//!    wallabag-style provenance lands on `ExtractedDocument.source_url`.
//!    The frontmatter is left in `full_text` verbatim — keeps FTS
//!    matching tag/domain tokens and keeps wire-compat with cb-api
//!    rows ingested before this parser existed.
//!
//! Anything non-markdown passes through verbatim — log files, CSVs,
//! JSON dumps, source code, etc.

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
    let is_md = matches!(ext.as_str(), "md" | "markdown");
    let headings = if is_md {
        full_text
            .lines()
            .filter(|line| line.trim_start().starts_with('#'))
            .map(|line| line.trim_start_matches('#').trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    };

    // v106 — YAML-frontmatter url extraction (wallabag-style).
    let source_url = if is_md {
        extract_frontmatter_url(&full_text)
    } else {
        None
    };

    Ok(ExtractedDocument {
        full_text,
        headings,
        ext: String::new(),            // dispatcher fills
        language: None,                // post-LID hook fills
        translated_text: None,         // post-translate hook fills
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url,
    })
}

/// Lift the `url:` value from a YAML frontmatter block at the top of
/// a markdown file.  Expects the wallabag-export shape:
///
/// ```yaml
/// ---
/// title: "..."
/// url: "https://example.org/..."
/// ---
/// ```
///
/// Tiny on purpose — a full YAML parser is overkill for the simple
/// flat `key: value` shape every read-later exporter emits.  Handles
/// double-quoted, single-quoted, and bare values; Windows line
/// endings; and returns `None` for any non-conforming input (no
/// frontmatter, missing `url:`, empty value, etc.).
fn extract_frontmatter_url(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    // Must start with `---` followed by a newline.
    let after_start = if bytes.starts_with(b"---\n") {
        4
    } else if bytes.starts_with(b"---\r\n") {
        5
    } else {
        return None;
    };
    let rest = &text[after_start..];
    // Find the closing `---` line.
    let end = rest
        .find("\n---\n")
        .or_else(|| rest.find("\n---\r\n"))?;
    let frontmatter = &rest[..end];
    for line in frontmatter.lines() {
        let line = line.trim_end_matches('\r');
        let trimmed = line.trim_start();
        if !trimmed.starts_with("url:") {
            continue;
        }
        let value = trimmed["url:".len()..].trim();
        if value.is_empty() {
            return None;
        }
        // Strip matched surrounding quotes.
        let unquoted = if (value.starts_with('"')
            && value.ends_with('"')
            && value.len() >= 2)
            || (value.starts_with('\'')
                && value.ends_with('\'')
                && value.len() >= 2)
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        return Some(unquoted.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn frontmatter_lifts_double_quoted_url() {
        let body = "---\ntitle: \"X\"\nurl: \"https://example.org/a\"\n---\n\nbody\n";
        assert_eq!(
            extract_frontmatter_url(body),
            Some("https://example.org/a".to_string())
        );
    }

    #[test]
    fn frontmatter_lifts_unquoted_url() {
        let body = "---\nurl: https://example.org/b\n---\n";
        assert_eq!(
            extract_frontmatter_url(body),
            Some("https://example.org/b".to_string())
        );
    }

    #[test]
    fn frontmatter_handles_crlf() {
        let body = "---\r\nurl: \"https://example.org/c\"\r\n---\r\n";
        assert_eq!(
            extract_frontmatter_url(body),
            Some("https://example.org/c".to_string())
        );
    }

    #[test]
    fn frontmatter_missing_returns_none() {
        // No frontmatter at all
        assert_eq!(extract_frontmatter_url("# Just markdown\n"), None);
        // Frontmatter exists but no url: key
        assert_eq!(
            extract_frontmatter_url("---\ntitle: \"X\"\n---\n"),
            None
        );
        // url: present but empty value
        assert_eq!(extract_frontmatter_url("---\nurl: \n---\n"), None);
    }

    #[test]
    fn frontmatter_only_at_byte_zero() {
        // A `---` that appears later in the body isn't a frontmatter start.
        let body = "# Heading\n\n---\nurl: should-not-match\n---\n";
        assert_eq!(extract_frontmatter_url(body), None);
    }

    #[test]
    fn extractor_populates_source_url_from_wallabag_md() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("article.md");
        std::fs::write(
            &p,
            b"---\n\
              title: \"Zur juedischen Wahrnehmung\"\n\
              url: \"https://www.compass-infodienst.de/foo\"\n\
              domain: \"www.compass-infodienst.de\"\n\
              saved: 2026-05-25\n\
              tags: [\"pocket-import\"]\n\
              ---\n\
              \n\
              Body content here.\n",
        )
        .unwrap();
        let d = extract(&p).unwrap();
        assert_eq!(
            d.source_url.as_deref(),
            Some("https://www.compass-infodienst.de/foo")
        );
        // Body still contains the frontmatter (preserves FTS hits on
        // domain tokens and matches the wallabag-ingest behavior on
        // the cb-api side).
        assert!(d.full_text.contains("url:"));
        assert!(d.full_text.contains("Body content here."));
    }

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
