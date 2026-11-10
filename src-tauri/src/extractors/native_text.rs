//! Direct native text ingestion for mail, mailboxes, and presentations.
//! The private source copy and package preflight keep format readers bounded;
//! this path does not execute attachments, open external links, or run OCR.
use anyhow::{ensure, Context, Result};
use std::{fs::File, io::{Read, Write}, path::Path};

use super::{archive, eml, pptx, ExtractedDocument};

/// Serializable extraction content. Root text and flattened members remain
/// separate so the foreground path does not index the aggregate preview twice.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeExtractionResult {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub own_text: Option<String>,
    pub headings: Vec<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub ext: String,
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_source: Option<crisp_index_protocol::ContainerSourceReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_coverage: Option<crisp_index_protocol::ContainerCoverage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<NativeExtractedMember>,
}

#[derive(Debug, serde::Serialize)]
pub struct NativeExtractedMember {
    pub entries: Vec<crisp_index_protocol::ContainerEntry>,
    pub document: NativeExtractionResult,
}

impl NativeExtractionResult {
    pub fn from_document(document: ExtractedDocument) -> Result<Self> {
        if !document.members.is_empty() {
            ensure!(document.own_text.is_some() && document.container_source.is_some(),
                "Container extraction is missing its separate root text or source receipt");
        }
        if let Some(source) = &document.container_source { source.validate().map_err(anyhow::Error::msg)?; }
        if let Some(coverage) = &document.coverage { coverage.validate().map_err(anyhow::Error::msg)?; }
        let mut metadata = serde_json::Map::new();
        macro_rules! optional_metadata {
            ($name:literal, $value:expr) => {
                if let Some(value) = $value { metadata.insert($name.into(), serde_json::to_value(value)?); }
            };
        }
        optional_metadata!("language", document.language);
        optional_metadata!("text_translated", document.translated_text);
        optional_metadata!("text_translated_lang", document.translated_to_lang);
        optional_metadata!("source_url", document.source_url);
        optional_metadata!("audio", document.audio);
        optional_metadata!("image_exif", document.image_exif);
        let members = document.members.into_iter().map(|member| {
            ensure!(member.document.members.is_empty(), "Container member expansion must be flattened");
            crisp_index_protocol::container::validate_entry_chain(&member.entries).map_err(anyhow::Error::msg)?;
            Ok(NativeExtractedMember { entries: member.entries, document: Self::from_document(member.document)? })
        }).collect::<Result<Vec<_>>>()?;
        Ok(Self { text: document.full_text, own_text: document.own_text, headings: document.headings,
            tags: document.tags, ext: document.ext, metadata, container_source: document.container_source,
            container_coverage: document.coverage, members })
    }
}

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

    #[test]
    fn foreground_bridge_preserves_member_receipts_and_separate_root_content() {
        use crisp_index_protocol::{ContainerEntry, ContainerEntryKind, ContainerKind, ContainerSourceReceipt};
        let document = ExtractedDocument { full_text: "Archive summary\nMember body".into(), own_text: Some("Archive summary".into()),
            ext: "zip".into(), container_source: Some(ContainerSourceReceipt { kind: ContainerKind::Zip, sha256: "a".repeat(64), size: 100 }),
            members: vec![super::super::ExtractedMember {
                entries: vec![ContainerEntry { kind: ContainerEntryKind::Zip, index: 1, occurrence: 0,
                    name: "notes.txt".into(), sha256: "b".repeat(64), size: 20 }],
                document: ExtractedDocument { full_text: "Member body".into(), ext: "txt".into(), language: Some("en".into()),
                    source_url: Some("https://example.test/source".into()), translated_text: Some("Text des Eintrags".into()),
                    translated_to_lang: Some("de".into()), tags: vec!["reviewed".into()], ..Default::default() },
            }], ..Default::default() };
        let wire = serde_json::to_value(NativeExtractionResult::from_document(document).unwrap()).unwrap();
        assert_eq!(wire["ownText"], "Archive summary");
        assert_eq!(wire["text"], "Archive summary\nMember body");
        assert_eq!(wire["containerSource"]["sha256"], "a".repeat(64));
        assert_eq!(wire["members"][0]["entries"][0]["name"], "notes.txt");
        let child = &wire["members"][0]["document"];
        assert_eq!(child["text"], "Member body");
        assert_eq!(child["metadata"]["language"], "en");
        assert_eq!(child["metadata"]["text_translated_lang"], "de");
        assert_eq!(child["metadata"]["source_url"], "https://example.test/source");
        assert!(child.get("members").is_none());
    }

    #[test]
    fn foreground_bridge_rejects_members_without_a_root_byte_receipt() {
        let document = ExtractedDocument { members: vec![super::super::ExtractedMember {
            entries: vec![], document: ExtractedDocument::default(),
        }], ..Default::default() };
        assert!(NativeExtractionResult::from_document(document).unwrap_err().to_string().contains("root text or source receipt"));
    }

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
