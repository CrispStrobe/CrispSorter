//! PDF extractor — wraps the existing `pdf-extract` dep so the
//! per-filetype registry has a uniform call shape.
//!
//! No new dependency: `pdf-extract` is already pulled in for the
//! existing `extract_pdf_native` Tauri command. This module just
//! gives it the `Extractor`-like interface the registry uses.
//!
//! v106 — also opportunistically lifts a source URL from the PDF's
//! Info dict / XMP packet via lopdf (already a dep).  Real-world
//! coverage is low — only browser-saved PDFs and a subset of
//! academic papers carry a meaningful URL — but when it's there it's
//! exactly the kind of provenance we want.

use anyhow::Result;
use std::path::Path;

use super::ExtractedDocument;

pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    let text = pdf_extract::extract_text(path)?;
    let source_url = extract_source_url(path);
    Ok(ExtractedDocument {
        full_text: text,
        // `pdf_extract` doesn't surface heading structure — leaving
        // empty for now. A future improvement: lift heading-shaped
        // lines (single-sentence-per-line, larger-than-body-font, …)
        // via lopdf's content-stream walk. The existing
        // `extract_pdf_metadata` already opens lopdf for /Info dict;
        // reusing that load here would be a free win.
        headings: Vec::new(),
        // Filled in by the dispatcher.
        ext: String::new(),
        // Filled in by the dispatcher's post-LID hook when an
        // `ExtractOptions.text_lid_model` was supplied.
        language: None,
        // Filled in by the dispatcher's post-translate hook when
        // an `ExtractOptions.translate_to` was supplied.
        translated_text: None,
        translated_to_lang: None,
        audio: None,
        image_exif: None,
        source_url,
        // v107 — PDF tag lift not implemented yet (would require XMP
        // dc:subject parse).  Empty Vec keeps the wire happy.
        tags: vec![],
        audio_pcm: None,
    })
}

/// Best-effort lift of a source URL from a PDF's metadata.  Tries
/// (in order):
///
/// 1. `Info` dict, key `/URL` — rare but unambiguous when present.
/// 2. XMP packet, `<dc:source>` — the academic-paper convention
///    (Springer / Elsevier / arXiv tooling write this).
/// 3. XMP packet, `<xmp:URL>` — older Adobe-era convention.
///
/// Soft-fails: any lopdf error / missing key / non-URL string just
/// returns None.  We don't try to validate the URL shape — the
/// caller treats it as opaque provenance.
fn extract_source_url(path: &Path) -> Option<String> {
    let doc = lopdf::Document::load(path).ok()?;
    // 1. Info dict /URL
    if let Some(url) = read_info_url(&doc) {
        return Some(url);
    }
    // 2/3. XMP packet
    if let Some(url) = read_xmp_url(&doc) {
        return Some(url);
    }
    None
}

fn read_info_url(doc: &lopdf::Document) -> Option<String> {
    use lopdf::Object;
    let info_id = match doc.trailer.get(b"Info") {
        Ok(Object::Reference(r)) => *r,
        _ => return None,
    };
    let dict = match doc.get_object(info_id) {
        Ok(Object::Dictionary(d)) => d,
        _ => return None,
    };
    // PDF metadata strings are often Latin-1 or UTF-16 BE; lopdf's
    // helper returns &[u8] which we decode lossily.
    let val = dict.get(b"URL").ok()?;
    let bytes = match val {
        Object::String(b, _) => b.as_slice(),
        _ => return None,
    };
    let s = decode_pdf_string(bytes);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

fn read_xmp_url(doc: &lopdf::Document) -> Option<String> {
    use lopdf::Object;
    // Find the /Metadata stream on the catalog.
    let catalog_id = doc.trailer.get(b"Root").ok()?;
    let catalog_id = match catalog_id {
        Object::Reference(r) => *r,
        _ => return None,
    };
    let catalog = match doc.get_object(catalog_id).ok()? {
        Object::Dictionary(d) => d,
        _ => return None,
    };
    let meta_id = catalog.get(b"Metadata").ok()?;
    let meta_id = match meta_id {
        Object::Reference(r) => *r,
        _ => return None,
    };
    let stream = match doc.get_object(meta_id).ok()? {
        Object::Stream(s) => s,
        _ => return None,
    };
    let xml_bytes = stream
        .get_plain_content()
        .or_else(|_| stream.decompressed_content())
        .ok()?;
    let xml = String::from_utf8_lossy(&xml_bytes);
    // Tag-soup extract — no full XML parse for so little gain.
    for tag in &["dc:source", "xmp:URL"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        if let Some(start) = xml.find(&open) {
            let after = &xml[start + open.len()..];
            if let Some(end) = after.find(&close) {
                let val = after[..end].trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

fn decode_pdf_string(bytes: &[u8]) -> String {
    // BOM-marked UTF-16 BE (the standard PDF / Adobe metadata
    // encoding) — quick decode without a unicode-bom crate.
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    // Otherwise treat as Latin-1 (PDF default).
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_url_extraction_via_tag_soup() {
        // Synthesise an XMP fragment to exercise the tag-soup path
        // in isolation from lopdf's stream reading.
        let xml = r#"
            <?xpacket begin='?'?>
            <rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/">
                <dc:source>https://arxiv.org/abs/2401.12345</dc:source>
            </rdf:Description>
            <?xpacket end='r'?>
        "#;
        // Inline reproduction of the find() loop for unit-test scope.
        let mut found = None;
        for tag in &["dc:source", "xmp:URL"] {
            let open = format!("<{tag}>");
            let close = format!("</{tag}>");
            if let Some(s) = xml.find(&open) {
                let rest = &xml[s + open.len()..];
                if let Some(e) = rest.find(&close) {
                    found = Some(rest[..e].trim().to_string());
                    break;
                }
            }
        }
        assert_eq!(found.as_deref(), Some("https://arxiv.org/abs/2401.12345"));
    }

    #[test]
    fn decode_pdf_string_handles_utf16_be_bom() {
        // PDF metadata strings: BOM-marked UTF-16 BE is the spec.
        let mut bytes = vec![0xFE, 0xFF];
        for c in "https://example.org/x".encode_utf16() {
            bytes.extend_from_slice(&c.to_be_bytes());
        }
        let decoded = decode_pdf_string(&bytes);
        assert_eq!(decoded, "https://example.org/x");
    }

    #[test]
    fn decode_pdf_string_falls_back_to_latin1() {
        // No BOM → Latin-1 default.
        let bytes = b"plain ascii";
        assert_eq!(decode_pdf_string(bytes), "plain ascii");
    }
}
