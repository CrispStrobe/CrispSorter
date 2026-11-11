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

/// Export the text layer through the same validated, atomic path for GUI and
/// CLI callers. OCR is an explicit separate operation; empty pages stay empty.
pub fn export_text(path: &Path, format: &str, out_path: &Path) -> Result<usize, String> {
    crate::pdf_ops::with_operation("pdf.export_text", |log| {
        log.step("validating", serde_json::json!({"format":format}));
        // Reject unsupported formats before reading or touching either file.
        render_text(&[], format, "")?;
        log.step("extracting", serde_json::Value::Null);
        let pages = page_texts(path)?;
        let title = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        log.step("rendering", serde_json::json!({"pages":pages.len()}));
        let body = render_text(&pages, format, &title)?;
        log.step("publishing", serde_json::json!({"bytes":body.len()}));
        crate::pdf_ops::publish_bytes(out_path, body.as_bytes())?;
        Ok(pages.len())
    })
}

pub fn export_docx(path: &Path, out_path: &Path) -> Result<usize, String> {
    crate::pdf_ops::with_operation("pdf.export_docx", |log| {
        log.step("validating", serde_json::json!({"format":"docx"}));
        #[cfg(feature = "ocr-office")]
        {
            log.step("extracting", serde_json::Value::Null);
            let pages = page_texts(path)?;
            log.step("rendering", serde_json::json!({"pages":pages.len()}));
            let bytes = render_docx(&pages)?;
            log.step("publishing", serde_json::json!({"bytes":bytes.len()}));
            crate::pdf_ops::publish_bytes(out_path, &bytes)?;
            Ok(pages.len())
        }
        #[cfg(not(feature = "ocr-office"))]
        {
            let _ = (path, out_path);
            Err("Word export is not part of this build (needs the ocr-office feature). Nothing was written.".into())
        }
    })
}

pub mod tauri_commands {
    use super::*;

    /// Export the text layer as `text` or `markdown`. Returns the page count.
    #[tauri::command]
    pub async fn pdf_export_text(path: String, format: String, out_path: String) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || {
            export_text(Path::new(&path), &format, Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))
        .and_then(|r| r)
    }

    /// Export the text layer as a Word document, one PDF page per Word
    /// page. Returns the page count.
    #[tauri::command]
    pub async fn pdf_export_docx(path: String, out_path: String) -> Result<usize, String> {
        tokio::task::spawn_blocking(move || {
            export_docx(Path::new(&path), Path::new(&out_path))
        })
        .await
        .map_err(|e| format!("join: {e}"))
        .and_then(|r| r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_pdf(path: &Path) {
        use lopdf::{dictionary, Document, Object, Stream};
        let mut doc = Document::with_version("1.5");
        let pages = doc.new_object_id();
        let font = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        });
        let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
        let mut kids = Vec::<Object>::new();
        for content in ["First page", "Second page"] {
            let stream = doc.add_object(Stream::new(dictionary! {},
                format!("BT /F1 12 Tf 10 700 Td ({content}) Tj ET").into_bytes()));
            let page = doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages, "Resources" => resources,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
                "Contents" => stream,
            });
            kids.push(page.into());
        }
        doc.objects.insert(pages, dictionary! { "Type" => "Pages", "Count" => 2, "Kids" => kids }.into());
        let root = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        doc.trailer.set("Root", root);
        doc.save(path).unwrap();
    }

    #[test]
    fn native_text_export_publishes_complete_pages_and_safe_lifecycle_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("private-source.pdf");
        let target = dir.path().join("private-output.md");
        text_pdf(&source);
        std::fs::write(&target, b"previous output").unwrap();
        let (result, events) = crate::operation_log::capture(|| export_text(&source, "markdown", &target));
        assert_eq!(result.unwrap(), 2);
        let written = std::fs::read_to_string(&target).unwrap();
        assert!(written.contains("First page") && written.contains("Second page"));
        let receipts: Vec<_> = events.iter().filter(|e| e.operation == "pdf.export_text").collect();
        assert_eq!(receipts.first().unwrap().phase, "started");
        assert_eq!(receipts.last().unwrap().phase, "completed");
        assert!(receipts.iter().all(|e| e.operation_id == receipts[0].operation_id));
        assert!(receipts.iter().any(|e| e.stage.as_deref() == Some("rendering")));
        let logged = serde_json::to_string(&events).unwrap();
        assert!(!logged.contains("private-source") && !logged.contains("private-output"));
        assert!(!logged.contains("First page"));
    }

    #[test]
    fn rejected_export_and_publication_failure_preserve_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.pdf");
        text_pdf(&source);
        let original = std::fs::read(&source).unwrap();
        assert!(export_text(&source, "unsupported-format", &source).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), original);

        let target = dir.path().join("private-output");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("preserve"), b"previous complete output").unwrap();
        let (result, events) = crate::operation_log::capture(|| export_text(&source, "text", &target));
        let error = result.unwrap_err();
        assert_eq!(std::fs::read(target.join("preserve")).unwrap(), b"previous complete output");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        assert_eq!(events.last().unwrap().phase, "failed");
        let logged = serde_json::to_string(&events).unwrap();
        assert!(!logged.contains("private-output") && !logged.contains(&error));
    }

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
        use std::io::Read;
        let bytes = render_docx(&["Page one".into(), "".into(), "Page three".into()]).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut document = String::new();
        zip.by_name("word/document.xml").unwrap().read_to_string(&mut document).unwrap();
        assert!(document.contains("Page one") && document.contains("Page three"));
        assert_eq!(document.matches("w:type=\"page\"").count(), 2, "empty PDF pages retain their Word page boundary");
    }

    #[cfg(not(feature = "ocr-office"))]
    #[test]
    fn unavailable_word_export_is_a_logged_error_without_touching_output() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("existing.docx");
        std::fs::write(&target, b"existing package").unwrap();
        let (result, events) = crate::operation_log::capture(|| export_docx(Path::new("unopened-private.pdf"), &target));
        assert!(result.unwrap_err().contains("ocr-office"));
        assert_eq!(std::fs::read(&target).unwrap(), b"existing package");
        assert_eq!(events.first().unwrap().phase, "started");
        assert_eq!(events.last().unwrap().phase, "failed");
    }
}
