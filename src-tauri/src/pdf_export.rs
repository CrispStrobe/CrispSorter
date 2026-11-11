//! Export the edited PDF to other formats (plain text, Markdown, Word).
//!
//! The PDF editor's "Export as…" menu calls these with a temp copy of the
//! edited document. They recover the *text layer* only: a scanned page
//! without one exports as an empty page, and the OCR workbench is the
//! place for those. Layout is reduced to paragraphs per page; that is the
//! honest promise of a text-layer conversion, and the UI says so.

use std::path::Path;

/// Text per page from the PDF's text layer.
pub fn page_texts(path: &Path) -> Result<Vec<String>, String> {
    pdf_extract::extract_text_by_pages(path).map_err(|e| format!("text extraction failed: {e}"))
}

/// Split one page's text into paragraphs: blank lines separate them, and
/// single line breaks inside a paragraph are wrap artefacts of the PDF.
pub fn paragraphs(page: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for line in page.lines() {
        let t = line.trim();
        if t.is_empty() {
            if !cur.is_empty() {
                out.push(cur.join(" "));
                cur.clear();
            }
        } else {
            cur.push(t);
        }
    }
    if !cur.is_empty() {
        out.push(cur.join(" "));
    }
    out
}

/// Render the pages as plain text (`text`) or Markdown (`markdown`).
pub fn render_text(pages: &[String], format: &str, title: &str) -> Result<String, String> {
    let mut out = String::new();
    match format {
        "text" | "txt" => {
            for (i, p) in pages.iter().enumerate() {
                if i > 0 {
                    out.push_str("\n\u{000C}\n");
                }
                out.push_str(&paragraphs(p).join("\n\n"));
                out.push('\n');
            }
        }
        "markdown" | "md" => {
            if !title.is_empty() {
                out.push_str(&format!("# {title}\n\n"));
            }
            for (i, p) in pages.iter().enumerate() {
                out.push_str(&format!("## Page {}\n\n", i + 1));
                for para in paragraphs(p) {
                    out.push_str(&para);
                    out.push_str("\n\n");
                }
            }
        }
        other => return Err(format!("unknown text export format: {other}")),
    }
    Ok(out)
}

#[cfg(feature = "ocr-office")]
pub fn render_docx(pages: &[String]) -> Result<Vec<u8>, String> {
    use docx_rs::*;
    let mut docx = Docx::new().default_size(22);
    for (i, page) in pages.iter().enumerate() {
        let paras = paragraphs(page);
        if paras.is_empty() {
            let mut p = Paragraph::new();
            if i > 0 {
                p = p.add_run(Run::new().add_break(BreakType::Page));
            }
            docx = docx.add_paragraph(p);
            continue;
        }
        for (k, text) in paras.iter().enumerate() {
            let mut p = Paragraph::new();
            if i > 0 && k == 0 {
                p = p.add_run(Run::new().add_break(BreakType::Page));
            }
            docx = docx.add_paragraph(p.add_run(Run::new().add_text(text)));
        }
    }
    let mut buf = std::io::Cursor::new(Vec::new());
    docx.build().pack(&mut buf).map_err(|e| format!("DOCX packaging failed: {e}"))?;
    Ok(buf.into_inner())
}

pub mod tauri_commands {
    use super::*;

    /// Export the text layer as `text` or `markdown`. Returns the page count.
    #[tauri::command]
    pub async fn pdf_export_text(path: String, format: String, out_path: String) -> Result<usize, String> {
        let detail = format!("{format}: {path} -> {out_path}");
        let result = tokio::task::spawn_blocking(move || {
            let pages = page_texts(Path::new(&path))?;
            let title = Path::new(&path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let body = render_text(&pages, &format, &title)?;
            std::fs::write(&out_path, body).map_err(|e| format!("write {out_path}: {e}"))?;
            Ok(pages.len())
        })
        .await
        .map_err(|e| format!("join: {e}"))
        .and_then(|r| r);
        crate::pdf_ops::logged("export_text", &detail, result)
    }

    /// Export the text layer as a Word document, one PDF page per Word
    /// page. Returns the page count.
    #[tauri::command]
    pub async fn pdf_export_docx(path: String, out_path: String) -> Result<usize, String> {
        let detail = format!("{path} -> {out_path}");
        #[cfg(feature = "ocr-office")]
        let result = tokio::task::spawn_blocking(move || {
            let pages = page_texts(Path::new(&path))?;
            let bytes = render_docx(&pages)?;
            std::fs::write(&out_path, bytes).map_err(|e| format!("write {out_path}: {e}"))?;
            Ok(pages.len())
        })
        .await
        .map_err(|e| format!("join: {e}"))
        .and_then(|r| r);
        #[cfg(not(feature = "ocr-office"))]
        let result: Result<usize, String> = {
            let _ = (path, out_path);
            Err("Word export is not part of this build (needs the ocr-office feature). Nothing was written.".into())
        };
        crate::pdf_ops::logged("export_docx", &detail, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_join_wrapped_lines_and_split_on_blanks() {
        let p = paragraphs("Hello\nworld\n\n  Second  \npara\n\n\n");
        assert_eq!(p, vec!["Hello world".to_string(), "Second para".to_string()]);
    }

    #[test]
    fn markdown_has_a_heading_per_page() {
        let md = render_text(&["One".into(), "Two".into()], "markdown", "Doc").unwrap();
        assert!(md.starts_with("# Doc\n"));
        assert!(md.contains("## Page 1\n\nOne"));
        assert!(md.contains("## Page 2\n\nTwo"));
        assert!(render_text(&[], "html", "").is_err());
    }

    #[cfg(feature = "ocr-office")]
    #[test]
    fn docx_is_a_zip_package() {
        let bytes = render_docx(&["Page one".into(), "Page two".into()]).unwrap();
        assert_eq!(&bytes[..2], b"PK");
    }
}
