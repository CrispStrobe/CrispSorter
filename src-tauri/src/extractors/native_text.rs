//! Direct native text ingestion for mail, mailboxes, and presentations.
//! The private source copy and package preflight keep format readers bounded;
//! this path does not execute attachments, open external links, or run OCR.
use anyhow::{ensure, Context, Result};
use std::{fs::File, io::{Read, Write}, path::Path};

use super::{archive, eml, pptx, ExtractedDocument};

const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

/// These are the only formats accepted by the foreground native text bridge.
pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    ensure!(path.is_absolute(), "Native text extraction requires a local absolute file-system path");
    let ext = path.extension().and_then(|value| value.to_str()).unwrap_or_default().to_ascii_lowercase();
    let max_source_bytes = match ext.as_str() {
        "eml" => 64 * 1024 * 1024,
        "mbox" => 256 * 1024 * 1024,
        "pptx" => 128 * 1024 * 1024,
        _ => anyhow::bail!("Native text extraction supports EML, MBOX, and PPTX files only"),
    };
    let snapshot = snapshot(path, &ext, max_source_bytes)?;
    let mut document = match ext.as_str() {
        "eml" => eml::extract(snapshot.path())?,
        "mbox" => eml::extract_mbox(snapshot.path())?,
        "pptx" => {
            archive::preflight_document_package(snapshot.path()).context("Checking the presentation package")?;
            let deck = pptx::read_deck_bounded(snapshot.path(), &pptx::ReadOptions::default(), MAX_TEXT_BYTES)?;
            ensure!(deck.slides.iter().any(|slide| {
                slide.title.as_deref().is_some_and(|text| !text.trim().is_empty())
                    || !slide.body_paragraphs().is_empty()
                    || slide.notes.as_deref().is_some_and(|text| !text.trim().is_empty())
                    || slide.comments.iter().any(|comment| !comment.text.trim().is_empty())
            }), "The presentation contains no searchable text. Slide images and embedded media are not OCRed by this text reader.");
            ExtractedDocument {
                full_text: pptx::render_markdown(&deck, &pptx::RenderOptions::default()),
                headings: deck.slides.iter().map(pptx::Slide::heading).collect(),
                ext: ext.clone(),
                tags: vec!["extraction:presentation-text-only".into()],
                ..Default::default()
            }
        }
        _ => unreachable!(),
    };
    ensure!(!document.full_text.trim().is_empty(), "The file contains no searchable text");
    let coverage = if ext == "pptx" {
        "[Extraction coverage: slide text, speaker notes, and comments. Images, charts, and embedded media were not extracted.]\n\n"
    } else if document.tags.iter().any(|tag| tag == "extraction:attachment-name-only") {
        "[Extraction coverage: message text, headers, text attachments, and forwarded messages. Binary attachments are listed by name; their contents were not extracted.]\n\n"
    } else { "" };
    ensure!(document.full_text.len() <= MAX_TEXT_BYTES.saturating_sub(coverage.len()),
        "Native extracted text exceeds the 16 MiB limit");
    document.full_text.insert_str(0, coverage);
    document.tags.push(format!("extraction:native-{ext}"));
    document.tags.sort();
    document.tags.dedup();
    Ok(document)
}

fn snapshot(path: &Path, ext: &str, maximum: u64) -> Result<tempfile::NamedTempFile> {
    // Check the opened handle as well as its length, so devices/FIFOs cannot
    // become unbounded parser inputs. The original path is never a temp name.
    ensure!(std::fs::metadata(path)?.is_file(), "Native text source must be a regular file");
    let input = File::open(path).context("Opening native text source")?;
    let metadata = input.metadata()?;
    ensure!(metadata.is_file(), "Native text source must be a regular file");
    ensure!(metadata.len() <= maximum, "Native text source exceeds its format byte limit");
    let mut copy = tempfile::Builder::new().prefix("crisp-native-text-").suffix(&format!(".{ext}")).tempfile()
        .context("Creating a private native text source copy")?;
    let copied = std::io::copy(&mut input.take(maximum + 1), copy.as_file_mut())?;
    ensure!(copied == metadata.len() && copied <= maximum, "Native text source changed size or exceeded its byte limit");
    copy.as_file_mut().flush()?;
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(ext: &str, bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut source = tempfile::Builder::new().suffix(&format!(".{ext}")).tempfile().unwrap();
        source.write_all(bytes).unwrap(); source.flush().unwrap(); source
    }

    #[test]
    fn direct_mail_preserves_decoding_and_visible_binary_attachment_coverage() {
        let mail = source("EML", b"Subject: =?utf-8?Q?Shipping_caf=C3=A9?=\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nReceipt caf=C3=A9 ready.\r\n--x\r\nContent-Type: application/pdf; name=invoice.pdf\r\nContent-Disposition: attachment; filename=invoice.pdf\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjc=\r\n--x--\r\n");
        let document = extract(mail.path()).unwrap();
        assert!(document.full_text.contains("Shipping café"));
        assert!(document.full_text.contains("Receipt café ready."));
        assert!(document.full_text.contains("invoice.pdf"));
        assert!(document.full_text.contains("their contents were not extracted"));
        assert!(!document.full_text.contains("%PDF"));
        assert!(document.tags.contains(&"extraction:attachment-name-only".into()));
        assert!(document.tags.contains(&"extraction:native-eml".into()));
    }

    #[test]
    fn direct_mailbox_keeps_message_boundaries() {
        let mailbox = source("mbox", b"From alice@example.test Thu Jan  1 00:00:00 2026\nSubject: First\n\nAlpha message.\nFrom bob@example.test Thu Jan  1 00:00:01 2026\nSubject: Second\n\nBeta message.\n");
        let document = extract(mailbox.path()).unwrap();
        assert!(document.full_text.contains("Message 1"));
        assert!(document.full_text.contains("Message 2"));
        assert!(document.full_text.contains("Alpha message."));
        assert!(document.full_text.contains("Beta message."));
        assert!(document.tags.contains(&"extraction:native-mbox".into()));
    }

    #[test]
    fn direct_presentation_uses_real_reading_order_notes_and_comments() {
        let directory = tempfile::tempdir().unwrap();
        let path = pptx::tests_support::write_sample_deck(directory.path());
        let document = extract(&path).unwrap();
        assert_eq!(document.headings, ["Slide 1: Erste Folie", "Slide 2"]);
        assert!(document.full_text.contains("Nicht zu schnell sprechen"));
        assert!(document.full_text.contains("Jana"));
        assert!(document.full_text.contains("Images, charts, and embedded media were not extracted"));
        assert!(document.tags.contains(&"extraction:presentation-text-only".into()));
    }

    #[test]
    fn direct_bridge_refuses_unadvertised_formats_relative_paths_and_empty_sources() {
        assert!(extract(Path::new("source.eml")).unwrap_err().to_string().contains("absolute"));
        let unknown = source("ppt", b"unsupported binary format");
        assert!(extract(unknown.path()).unwrap_err().to_string().contains("EML, MBOX, and PPTX"));
        let empty = source("eml", b"");
        assert!(extract(empty.path()).unwrap_err().to_string().contains("no searchable text"));
    }

    #[test]
    fn snapshot_has_generated_path_bounds_and_cleanup() {
        let original = source("eml", b"Subject: Snapshot\n\nOriginal body");
        let copy = snapshot(original.path(), "eml", 1024).unwrap();
        let copy_path = copy.path().to_owned();
        assert_ne!(copy_path, original.path());
        std::fs::write(original.path(), b"changed").unwrap();
        assert!(std::fs::read_to_string(&copy_path).unwrap().contains("Original body"));
        drop(copy);
        assert!(!copy_path.exists());
        assert!(snapshot(original.path(), "eml", 2).is_err());
    }

    #[test]
    fn presentation_preflight_rejects_expansion_before_native_parsing() {
        use zip::{write::SimpleFileOptions, ZipWriter};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversized.pptx");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        writer.start_file("ppt/slides/slide1.xml", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"x").unwrap();
        writer.finish().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let directory_offset = bytes.windows(4).position(|window| window == b"PK\x01\x02").unwrap();
        bytes[directory_offset + 24..directory_offset + 28].copy_from_slice(&(33_u32 * 1024 * 1024).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        let error = format!("{:#}", extract(&path).unwrap_err());
        assert!(error.contains("expanded-byte limit"), "{error}");
    }

    #[test]
    fn image_only_decks_are_not_indexed_as_generated_slide_headings() {
        use zip::{write::SimpleFileOptions, ZipWriter};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scan.pptx");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        writer.start_file("ppt/slides/slide1.xml", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"<p:sld><p:cSld><p:spTree><p:pic/></p:spTree></p:cSld></p:sld>").unwrap();
        writer.finish().unwrap();
        assert!(extract(&path).unwrap_err().to_string().contains("no searchable text"));
    }
}
