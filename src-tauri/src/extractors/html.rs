//! HTML extractor — basic tag-stripping. Lifts `<title>` + `<h1>..<h6>`
//! into `headings`; everything else gets the tags removed.
//!
//! Deliberately *no* `scraper` / `html5ever` dep — that's a substantial
//! tree we'd rather not pull in for what's mostly a fallback path. If
//! HTML extraction quality becomes a real bottleneck, swap this whole
//! module for a `scraper`-backed implementation; the public signature
//! is the trait-shaped function we already use, so callers don't move.
//!
//! Limitations the regex approach inherits:
//! * Scripts and styles aren't stripped — they leak into full_text.
//!   Fine for indexing (the embedder + BM25 ignore them as noise) but
//!   a real parser would do better.
//! * Comments / CDATA aren't handled specially.
//! * Entities aren't decoded — `&amp;` stays literal. Acceptable for
//!   token-based search; would matter for a strict text dump.

use anyhow::{Context, Result};
use std::path::Path;

use super::ExtractedDocument;

pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let raw = String::from_utf8_lossy(&bytes);

    let headings = lift_headings(&raw);
    let full_text = strip_tags(&raw);

    Ok(ExtractedDocument {
        full_text,
        headings,
        ext: String::new(),
        language: None,
        translated_text: None,
        translated_to_lang: None,
    })
}

/// Pull text content from `<title>`, `<h1>` through `<h6>`. Naïve but
/// good enough for FTS boost — `<h1>foo</h1>` yields `["foo"]`.
fn lift_headings(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tag in ["title", "h1", "h2", "h3", "h4", "h5", "h6"] {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let mut start = 0;
        while let Some(open_pos) = html[start..].to_ascii_lowercase().find(&open) {
            let abs_open = start + open_pos;
            // Skip past the opening tag (find the `>` that ends it,
            // accounting for attributes).
            if let Some(gt) = html[abs_open..].find('>') {
                let content_start = abs_open + gt + 1;
                if let Some(rel_close) = html[content_start..].to_ascii_lowercase().find(&close) {
                    let content = &html[content_start..content_start + rel_close];
                    let stripped = strip_tags(content).trim().to_string();
                    if !stripped.is_empty() {
                        out.push(stripped);
                    }
                    start = content_start + rel_close + close.len();
                    continue;
                }
            }
            // Malformed — bail on this tag.
            break;
        }
    }
    out
}

/// Remove every `<...>` tag, leaving the inner text. Whitespace gets
/// collapsed so `<p>foo</p><p>bar</p>` becomes `foo bar` rather than
/// `foobar` or `foo\n\n\n\nbar`.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    // Collapse runs of whitespace.
    let mut collapsed = String::with_capacity(out.len());
    let mut last_was_space = false;
    for ch in out.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                collapsed.push(' ');
                last_was_space = true;
            }
        } else {
            collapsed.push(ch);
            last_was_space = false;
        }
    }
    collapsed.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn strips_tags_to_text() {
        let s = strip_tags("<p>hello</p> <p>world</p>");
        assert_eq!(s, "hello world");
    }

    #[test]
    fn lifts_title_and_headings() {
        let html = "<html><head><title>Doc Title</title></head><body><h1>Top</h1><p>x</p><h2>Sub</h2></body></html>";
        let h = lift_headings(html);
        assert!(h.contains(&"Doc Title".to_string()));
        assert!(h.contains(&"Top".to_string()));
        assert!(h.contains(&"Sub".to_string()));
    }

    #[test]
    fn extract_writes_full_text_and_headings() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("page.html");
        std::fs::write(
            &p,
            b"<html><head><title>T</title></head><body><h1>H</h1><p>Body text here.</p></body></html>",
        )
        .unwrap();
        let d = extract(&p).unwrap();
        assert_eq!(d.headings, vec!["T", "H"]);
        assert!(d.full_text.contains("Body text here."));
    }

    #[test]
    fn collapses_whitespace_runs() {
        let s = strip_tags("<p>foo</p>\n\n  \n  <p>bar</p>");
        assert_eq!(s, "foo bar");
    }
}
