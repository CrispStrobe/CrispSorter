//! Bounded HTML extraction with an HTML5 parser, entity decoding, and explicit
//! exclusion of scripts, styles, templates, and markup-hidden subtrees.
//! External resources are never fetched and scripts are never executed.

use anyhow::{ensure, Context, Result};
use scraper::{ElementRef, Html, Selector};
use std::{io::Read, path::Path};

use super::ExtractedDocument;

const MAX_HTML_BYTES: usize = 16 * 1024 * 1024;
const MAX_HTML_NODES: usize = 500_000;

pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    let bytes = read_bounded(path, MAX_HTML_BYTES)?;
    let raw = decode_html(&bytes);
    let document = parse_bounded(&raw)?;
    let body = document
        .select(&selector("body"))
        .next()
        .unwrap_or_else(|| document.root_element());
    let full_text = visible_text(body);
    ensure!(
        full_text.len() <= MAX_HTML_BYTES,
        "HTML extracted text exceeds the 16 MiB limit"
    );
    let headings = document
        .select(&selector("title, h1, h2, h3, h4, h5, h6"))
        .filter(|element| !element.ancestors().filter_map(ElementRef::wrap).any(hidden))
        .map(visible_text)
        .filter(|text| !text.is_empty())
        .collect();
    let source_url = document
        .select(&selector("link[rel], meta[property]"))
        .find_map(|element| {
            let value = element.value();
            let candidate = if value.name() == "link"
                && value.attr("rel").is_some_and(|rel| {
                    rel.split_whitespace()
                        .any(|s| s.eq_ignore_ascii_case("canonical"))
                }) {
                value.attr("href")
            } else if value
                .attr("property")
                .is_some_and(|p| p.eq_ignore_ascii_case("og:url"))
            {
                value.attr("content")
            } else {
                None
            }?;
            let candidate = candidate.trim();
            (candidate.starts_with("https://") || candidate.starts_with("http://"))
                .then(|| candidate.to_string())
        });
    Ok(ExtractedDocument {
        full_text,
        headings,
        ext: "html".into(),
        source_url,
        ..Default::default()
    })
}

/// Shared by MIME HTML parts. This keeps visible text and phrase boundaries
/// consistent whether a document arrived as an HTML file or an email.
pub(super) fn visible_html(html: &str) -> Result<String> {
    ensure!(
        html.len() <= MAX_HTML_BYTES,
        "HTML part exceeds the 16 MiB limit"
    );
    let document = parse_bounded(html)?;
    let body = document
        .select(&selector("body"))
        .next()
        .unwrap_or_else(|| document.root_element());
    let text = visible_text(body);
    ensure!(
        text.len() <= MAX_HTML_BYTES,
        "HTML extracted text exceeds the 16 MiB limit"
    );
    Ok(text)
}

pub(super) fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        file.metadata()?.len() <= limit as u64,
        "Source exceeds extraction limit ({limit} bytes)"
    );
    let mut bytes = Vec::new();
    file.take((limit as u64) + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "Source grew beyond extraction limit ({limit} bytes)"
    );
    Ok(bytes)
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static HTML selector")
}

fn parse_bounded(raw: &str) -> Result<Html> {
    let document = Html::parse_document(raw);
    ensure!(
        document.tree.nodes().take(MAX_HTML_NODES + 1).count() <= MAX_HTML_NODES,
        "HTML tree exceeds the node limit"
    );
    Ok(document)
}

fn decode_html(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    if let Some((encoding, skip)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding.decode_without_bom_handling(&bytes[skip..]).0;
    }
    // HTML encoding declarations are ASCII-compatible and are inspected only
    // in a bounded prefix. A declaration takes precedence over UTF-8 guessing.
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(8192)]);
    let probe = Html::parse_document(&prefix);
    let declared = probe.select(&selector("meta")).find_map(|meta| {
        let element = meta.value();
        if let Some(label) = element.attr("charset") {
            return encoding_rs::Encoding::for_label(label.as_bytes());
        }
        if element
            .attr("http-equiv")
            .is_some_and(|v| v.eq_ignore_ascii_case("content-type"))
        {
            let content = element.attr("content")?;
            for entry in content.split(';') {
                if let Some((name, label)) = entry.split_once('=') {
                    if name.trim().eq_ignore_ascii_case("charset") {
                        return encoding_rs::Encoding::for_label(
                            label.trim().trim_matches(['\'', '"']).as_bytes(),
                        );
                    }
                }
            }
        }
        None
    });
    if let Some(encoding) = declared {
        return encoding.decode_without_bom_handling(bytes).0;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.into(),
        Err(_) => {
            encoding_rs::WINDOWS_1252
                .decode_without_bom_handling(bytes)
                .0
        }
    }
}

fn hidden(element: ElementRef<'_>) -> bool {
    let value = element.value();
    matches!(value.name(), "script" | "style" | "template" | "noscript")
        || value.attr("hidden").is_some()
        || value
            .attr("aria-hidden")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
        || value.attr("style").is_some_and(|style| {
            style.split(';').any(|declaration| {
                let Some((property, value)) = declaration.split_once(':') else {
                    return false;
                };
                let property = property.trim();
                let value = value.split('!').next().unwrap_or("").trim();
                (property.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none"))
                    || (property.eq_ignore_ascii_case("visibility")
                        && value.eq_ignore_ascii_case("hidden"))
                    || (property.eq_ignore_ascii_case("content-visibility")
                        && value.eq_ignore_ascii_case("hidden"))
            })
        })
}

fn block(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "br"
            | "caption"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "td"
            | "th"
            | "tr"
            | "ul"
    )
}

fn visible_text(root: ElementRef<'_>) -> String {
    // Iterative traversal avoids growing the Rust stack on deeply nested HTML.
    let mut stack = vec![(*root, false)];
    let mut out = String::new();
    while let Some((node, closing)) = stack.pop() {
        if let Some(element) = ElementRef::wrap(node) {
            if hidden(element) {
                continue;
            }
            let name = element.value().name();
            if block(name) {
                out.push(' ');
            }
            if closing {
                continue;
            }
            if name == "img" {
                if let Some(alt) = element.value().attr("alt") {
                    out.push(' ');
                    out.push_str(alt);
                    out.push(' ');
                }
            }
            stack.push((node, true));
            stack.extend(node.children().rev().map(|child| (child, false)));
        } else if let Some(text) = node.value().as_text() {
            out.push_str(text);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_inline_words_and_separates_blocks() {
        assert_eq!(
            visible_html("<p>inter<b>oper</b>able</p><p>second<br>line</p>").unwrap(),
            "interoperable second line"
        );
    }

    #[test]
    fn decodes_entities_once_without_exposing_scripts_or_comments() {
        assert_eq!(visible_html("<p>A &amp; B &#x1F680; &lt;literal&gt;</p><!-- secret --><script>secret()</script><style>.secret{}</style>").unwrap(), "A & B 🚀 <literal>");
    }

    #[test]
    fn removes_hidden_subtrees_but_retains_image_alternatives() {
        assert_eq!(visible_html("<div hidden>one</div><div aria-hidden='true'>two</div><div style='display: none !important'>three</div><template>four</template><p>Visible<img alt='chart legend'></p>").unwrap(), "Visible chart legend");
    }

    #[test]
    fn parses_quoted_attributes_and_malformed_markup() {
        assert_eq!(
            visible_html("<p title='2 > 1'>Start <strong>bold</p><p>End").unwrap(),
            "Start bold End"
        );
    }

    #[test]
    fn extracts_headings_and_canonical_source_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saved.html");
        std::fs::write(&path, "<title>A &amp; B</title><link rel='canonical' href='https://example.test/page'><h1>First</h1><div hidden><h2>Hidden</h2></div><p>Body</p>").unwrap();
        let doc = extract(&path).unwrap();
        assert_eq!(doc.headings, ["A & B", "First"]);
        assert_eq!(doc.full_text, "First Body");
        assert_eq!(doc.source_url.as_deref(), Some("https://example.test/page"));
    }

    #[test]
    fn honors_declared_legacy_encoding() {
        let text = b"<meta charset='windows-1252'><p>caf\xe9 \x80</p>";
        assert!(decode_html(text).contains("café €"));
    }

    #[test]
    fn honors_utf16_bom() {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend("<p>東京</p>".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(visible_html(&decode_html(&bytes)).unwrap(), "東京");
    }

    #[test]
    fn bounded_reader_rejects_oversized_sources_without_loading_them() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(1025).unwrap();
        assert!(read_bounded(file.path(), 1024).is_err());
    }

    #[test]
    fn iterative_text_walk_handles_deep_markup() {
        let html = format!("{}deep{}", "<div>".repeat(2048), "</div>".repeat(2048));
        assert_eq!(visible_html(&html).unwrap(), "deep");
    }
}
