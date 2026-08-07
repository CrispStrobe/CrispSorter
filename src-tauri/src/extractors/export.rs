//! Document export to additional formats (P27.10).
//!
//! Exports extracted/OCR'd text to DOCX and standalone HTML.

use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct ExportResult {
    pub path: String,
    pub format: String,
    pub bytes: usize,
}

/// Export text content to a DOCX file.
pub fn export_to_docx(
    title: &str,
    body: &str,
    out_path: &Path,
) -> Result<ExportResult, String> {
    use docx_rs::*;

    // `Docx::new()` ships a styles part containing only `Normal`, so
    // referencing `Heading1` without defining it is a dangling reference:
    // Word silently falls back to body text and the title never reaches the
    // navigation pane or a generated table of contents. The run below is
    // bold either way, which is why this went unnoticed — it *looked* right.
    let mut docx = Docx::new().add_style(
        Style::new("Heading1", StyleType::Paragraph)
            .name("heading 1")
            .based_on("Normal")
            .next("Normal")
            .bold()
            .size(32)
            .outline_lvl(0)
            .q_format(true),
    );

    // Add title
    if !title.is_empty() {
        docx = docx.add_paragraph(
            Paragraph::new()
                .add_run(Run::new().add_text(title).bold())
                .style("Heading1"),
        );
    }

    // Add body paragraphs (split on double newlines)
    for para in body.split("\n\n") {
        let trimmed = para.trim();
        if trimmed.is_empty() { continue; }
        docx = docx.add_paragraph(
            Paragraph::new().add_run(Run::new().add_text(trimmed)),
        );
    }

    let _bytes = docx.build().pack(std::fs::File::create(out_path)
        .map_err(|e| format!("create {}: {e}", out_path.display()))?)
        .map_err(|e| format!("write DOCX: {e}"))?;

    let size = std::fs::metadata(out_path)
        .map(|m| m.len() as usize)
        .unwrap_or(0);

    Ok(ExportResult {
        path: out_path.to_string_lossy().into_owned(),
        format: "docx".into(),
        bytes: size,
    })
}

/// Export text content to a standalone HTML file with embedded CSS.
pub fn export_to_html(
    title: &str,
    body: &str,
    out_path: &Path,
) -> Result<ExportResult, String> {
    let escaped_title = html_escape(title);
    let body_html = body
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("<p>{}</p>", html_escape(p.trim())))
        .collect::<Vec<_>>()
        .join("\n");

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{escaped_title}</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
         max-width: 800px; margin: 40px auto; padding: 0 20px;
         line-height: 1.7; color: #1a1a1a; }}
  h1 {{ font-size: 1.8em; margin-bottom: 0.5em; }}
  p {{ margin: 0.8em 0; }}
</style>
</head>
<body>
<h1>{escaped_title}</h1>
{body_html}
</body>
</html>"#
    );

    std::fs::write(out_path, html.as_bytes())
        .map_err(|e| format!("write {}: {e}", out_path.display()))?;

    Ok(ExportResult {
        path: out_path.to_string_lossy().into_owned(),
        format: "html".into(),
        bytes: html.len(),
    })
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn export_html_basic() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("test.html");
        let result = export_to_html("Test Title", "First paragraph.\n\nSecond paragraph.", &out).unwrap();
        assert_eq!(result.format, "html");
        assert!(result.bytes > 0);
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("Test Title"));
        assert!(content.contains("First paragraph."));
        assert!(content.contains("<p>Second paragraph.</p>"));
    }

    #[test]
    fn export_html_escapes_special_chars() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("escaped.html");
        export_to_html("A < B & C", "x > y", &out).unwrap();
        let content = std::fs::read_to_string(&out).unwrap();
        assert!(content.contains("A &lt; B &amp; C"));
        assert!(content.contains("x &gt; y"));
    }

    #[test]
    fn export_html_empty_body() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("empty.html");
        let result = export_to_html("Title", "", &out).unwrap();
        assert!(result.bytes > 0);
    }

    #[test]
    fn export_docx_basic() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("test.docx");
        let result = export_to_docx("My Document", "Hello world.\n\nSecond para.", &out).unwrap();
        assert_eq!(result.format, "docx");
        assert!(result.bytes > 0);
        assert!(out.exists());
    }

    #[test]
    fn export_docx_empty_title() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("notitle.docx");
        let result = export_to_docx("", "Just body text.", &out).unwrap();
        assert!(result.bytes > 0);
    }

    #[test]
    fn export_html_escapes_title() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("escaped2.html");
        let result = export_to_html("He said \"hello\" & <bye>", "body text", &out).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains("&amp;"), "& must be escaped");
        assert!(html.contains("&lt;"), "< must be escaped");
        assert!(!html.contains("<bye>"), "raw tag must not appear");
        assert!(result.bytes > 0);
    }

    #[test]
    fn export_html_unicode_body() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("unicode.html");
        let result = export_to_html("日本語", "日本語テスト\n\n第二段落", &out).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains("日本語"));
        assert!(html.contains("<p>"));
        assert!(result.bytes > 0);
    }

    #[test]
    fn export_docx_unicode_no_panic() {
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("unicode.docx");
        let result = export_to_docx("日本語タイトル", "こんにちは世界\n\n第二段落", &out).unwrap();
        assert!(result.bytes > 0);
    }

    /// The title paragraph asks for `Heading1`, so the package has to define
    /// it. It did not: `Docx::new()` ships a styles part containing only
    /// `Normal`, and a `pStyle` pointing at a style that does not exist is
    /// silently ignored by Word — the title rendered as body text and never
    /// reached the navigation pane or a generated table of contents. The
    /// bold run made it *look* right, which is why it survived so long.
    #[test]
    fn export_docx_defines_every_style_it_references() {
        use std::io::Read;

        let dir = TempDir::new().unwrap();
        let out = dir.path().join("styled.docx");
        export_to_docx("Ein Titel", "Der Fliesstext.", &out).unwrap();

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let read = |zip: &mut zip::ZipArchive<std::fs::File>, name: &str| {
            let mut s = String::new();
            zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
            s
        };
        let document = read(&mut zip, "word/document.xml");
        let styles = read(&mut zip, "word/styles.xml");

        assert!(
            document.contains(r#"w:val="Heading1""#),
            "the title should still be a heading: {document}"
        );
        assert!(
            styles.contains(r#"w:styleId="Heading1""#),
            "document.xml references Heading1 but styles.xml does not define it — \
             Word will render the title as body text:\n{styles}"
        );
    }
}
