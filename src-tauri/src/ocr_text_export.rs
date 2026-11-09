//! Reviewed OCR text exports. Input is already in the user's reading order.
//! No trimming, Unicode reordering, normalization, or generated text is applied.

pub const PDF_BIDI_UNAVAILABLE: &str = "PDF export for right-to-left text or directional formatting is unavailable in this build. Choose a text format to preserve the reviewed reading order.";
pub const MAX_DOCX_TEXT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_DOCX_STRUCTURAL_ITEMS: usize = 100_000;

/// Match CrispEmbed 9c44a69's conservative `needs_bidirectional_layout` guard.
/// This detects an unavailable renderer path; it does not perform text layout.
pub fn needs_bidirectional_pdf(text: &str) -> bool {
    text.chars().any(|ch| matches!(ch as u32,
        0x0590..=0x08ff | 0xfb1d..=0xfdff | 0xfe70..=0xfeff |
        0x10800..=0x10fff | 0x1e800..=0x1efff | 0x200e..=0x200f |
        0x202a..=0x202e | 0x2066..=0x2069))
}

pub fn text(pages: &[Vec<&str>]) -> String {
    pages.iter().map(|page| page.join("\n")).collect::<Vec<_>>().join("\u{000c}")
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&quot;").replace('\'', "&#39;").replace('\r', "&#13;")
}

pub fn html(pages: &[Vec<&str>]) -> String {
    let body = pages.iter().enumerate().map(|(i, page)| format!(
        "<section aria-label=\"Page {}\"><h2>Page {}</h2>{}</section>", i + 1, i + 1,
        page.iter().map(|text| format!("<p dir=\"auto\">{}</p>", escape_html(text))).collect::<Vec<_>>().join("\n"),
    )).collect::<Vec<_>>().join("\n");
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>Reviewed OCR document</title><style>body{{font:16px/1.6 system-ui,sans-serif;max-width:72ch;margin:3rem auto;padding:0 1rem}}p{{white-space:pre-wrap;unicode-bidi:plaintext;overflow-wrap:anywhere}}section{{break-after:page}}section:last-child{{break-after:auto}}h2{{font-size:1rem;color:#555}}</style></head><body>{body}</body></html>")
}

/// Bound source text and OOXML node growth before building the document.
/// These are input bounds, not a process peak-memory guarantee.
pub fn validate_docx(pages: &[Vec<&str>]) -> Result<(), String> {
    let mut bytes = 0usize;
    let mut items = 0usize;
    for page in pages {
        items = items.saturating_add(page.len().max(1));
        for text in page {
            bytes = bytes.saturating_add(text.len());
            items = items.saturating_add(text.bytes().filter(|ch| matches!(ch, b'\n' | b'\r' | b'\t')).count());
        }
    }
    if bytes > MAX_DOCX_TEXT_BYTES || items > MAX_DOCX_STRUCTURAL_ITEMS {
        return Err("Word export supports at most 16 MiB of reviewed text and 100,000 paragraphs, tabs, and line breaks per export. Select fewer pages or choose Plain text or HTML document.".into());
    }
    Ok(())
}

#[cfg(feature = "ocr-text-export")]
pub fn docx(pages: &[Vec<&str>]) -> Result<Vec<u8>, String> {
    use docx_rs::{Docx, Paragraph, Run};
    use unicode_bidi::{get_base_direction, Direction};

    validate_docx(pages)?;
    let mut document = Docx::new();
    for (page_index, page) in pages.iter().enumerate() {
        // One empty paragraph keeps an explicitly blank source page.
        let empty = [""];
        let regions: &[&str] = if page.is_empty() { &empty } else { page };
        let mut first = true;
        for region in regions {
            // split() keeps empty/trailing paragraphs; lines() would lose them.
            for line in region.split('\n') {
                let mut paragraph = Paragraph::new().page_break_before(page_index > 0 && first);
                paragraph.property = paragraph.property.bidi(get_base_direction(line) == Direction::Rtl);
                let mut run = Run::new();
                let mut start = 0;
                for (index, ch) in line.char_indices() {
                    if ch == '\t' || ch == '\r' {
                        run = run.add_text(&line[start..index]);
                        run = if ch == '\t' { run.add_tab() } else { run.add_carriage_return() };
                        start = index + ch.len_utf8();
                    }
                }
                run = run.add_text(&line[start..]);
                document = document.add_paragraph(paragraph.add_run(run));
                first = false;
            }
        }
    }
    let mut output = std::io::Cursor::new(Vec::new());
    document.build().pack(&mut output).map_err(|error| format!("Write reviewed Word document: {error}"))?;
    Ok(output.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_keeps_logical_unicode_whitespace_and_empty_pages() {
        let pages = [vec!["  مَرْحَبًا ABC 123\t\n\n", "שָׁלוֹם e\u{301}  "], vec![], vec!["𞤀𞤢 😀\r\n"]];
        assert_eq!(text(&pages), "  مَرْحَبًا ABC 123\t\n\n\nשָׁלוֹם e\u{301}  \u{c}\u{c}𞤀𞤢 😀\r\n");
    }

    #[test]
    fn html_keeps_text_escapes_and_mobile_paragraph_direction() {
        let output = html(&[vec!["  العربية <script>&\r\n\tשלום  "]]);
        assert!(output.contains("<meta name=\"viewport\""));
        assert!(output.contains("<p dir=\"auto\">  العربية &lt;script&gt;&amp;&#13;\n\tשלום  </p>"));
        assert!(output.contains("unicode-bidi:plaintext"));
        assert!(!output.contains("<script>"));
    }

    #[test]
    fn pdf_guard_matches_native_conservative_ranges() {
        for value in ["العربية ABC", "שָׁלוֹם", "𞤀𞤢", "\u{2067}test\u{2069}", "\u{200e}Latin"] {
            assert!(needs_bidirectional_pdf(value));
        }
        assert!(!needs_bidirectional_pdf("café e\u{301} 中文 Ελληνικά 😀 123"));
    }

    #[test]
    fn docx_bounds_account_for_whitespace_node_growth() {
        assert!(validate_docx(&[vec![" \n\t\r"]]).is_ok());
        let text = "x".repeat(MAX_DOCX_TEXT_BYTES + 1);
        assert!(validate_docx(&[vec![&text]]).unwrap_err().contains("16 MiB"));
        let tabs = "\t".repeat(MAX_DOCX_STRUCTURAL_ITEMS);
        assert!(validate_docx(&[vec![&tabs]]).unwrap_err().contains("100,000"));
    }
}
