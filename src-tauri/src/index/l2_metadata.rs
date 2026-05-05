//! Level-2 ingest: read embedded metadata from a file *without* extracting
//! its full text. Cheaper than L3 (~10 ms per file vs seconds), useful when
//! the user only wants Author/Title/Year populated for the catalog.
//!
//! Supported formats:
//!
//! | Format | Source |
//! |---|---|
//! | PDF        | Info dictionary via `lopdf` (re-exported by `pdf-extract`) |
//! | DOCX       | `docProps/core.xml` (Dublin Core) inside the zip |
//! | EPUB       | OPF manifest (`<dc:title>`, `<dc:creator>`, …) inside the zip |
//! | TXT/MD     | filename only (no embedded metadata) |
//! | Image      | not implemented yet — needs `kamadak-exif` |
//!
//! All readers are **best-effort**: missing fields just remain `None`. We
//! never panic; corrupt or non-existent files yield an empty `L2Metadata`.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::Serialize;

/// Embedded-metadata fields extracted at L2.
#[derive(Debug, Default, Clone, Serialize)]
pub struct L2Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
    pub page_count: Option<i32>,
    /// Free-form fields that don't map to first-class columns yet.
    /// Stored verbatim in `metadata_json` (e.g. `creator`, `producer`,
    /// `keywords`, `subject`).
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Dispatch to the right reader by file extension.
pub fn read(path: &Path) -> L2Metadata {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let result: anyhow::Result<L2Metadata> = match ext.as_str() {
        "pdf" => read_pdf(path),
        "docx" => read_docx(path),
        "epub" => read_epub(path),
        "jpg" | "jpeg" | "tif" | "tiff" | "png" | "webp" | "heic" | "heif" => read_image_exif(path),
        _ => Ok(L2Metadata::default()),
    };

    match result {
        Ok(m) => m,
        Err(e) => {
            crate::app_log!(
                "info",
                "L2 metadata: skipping {} ({e})",
                path.display()
            );
            L2Metadata::default()
        }
    }
}

// ── PDF ────────────────────────────────────────────────────────────────────

fn read_pdf(path: &Path) -> anyhow::Result<L2Metadata> {
    // pdf-extract re-exports lopdf's types, so we don't need a separate dep.
    use pdf_extract::{Dictionary, Document, Object, ObjectId};

    let doc = Document::load(path)?;
    let mut out = L2Metadata::default();

    // --- Info dictionary (Title / Author / Subject / Keywords / Creator / Producer / Dates) ---
    if let Some(info_obj) = doc.trailer.get(b"Info").ok() {
        let dict_opt: Option<&Dictionary> = match info_obj {
            Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_dict().ok()),
            Object::Dictionary(d) => Some(d),
            _ => None,
        };
        if let Some(dict) = dict_opt {
            for (key, val) in dict.iter() {
                let key_s = std::str::from_utf8(key).unwrap_or("").to_owned();
                let v = pdf_string(val);
                match key_s.as_str() {
                    "Title" => out.title = v.clone(),
                    "Author" => out.author = v.clone(),
                    "Subject" | "Keywords" | "Creator" | "Producer" => {
                        if let Some(s) = v.clone() {
                            out.extra.insert(
                                key_s.to_ascii_lowercase(),
                                serde_json::Value::String(s),
                            );
                        }
                    }
                    "CreationDate" | "ModDate" => {
                        if let Some(s) = &v {
                            // PDF date format: "D:YYYYMMDDHHmmSS..."
                            let yr = parse_pdf_year(s);
                            if key_s == "CreationDate" && out.year.is_none() {
                                out.year = yr;
                            }
                            out.extra.insert(
                                key_s.to_ascii_lowercase(),
                                serde_json::Value::String(s.clone()),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // --- Page count via Catalog → Pages → /Count ---
    if let Ok(catalog_obj) = doc.trailer.get(b"Root") {
        if let Object::Reference(catalog_id) = catalog_obj {
            if let Ok(catalog) = doc.get_object(*catalog_id).and_then(|o| o.as_dict()) {
                if let Ok(pages_obj) = catalog.get(b"Pages") {
                    if let Object::Reference(pages_id) = pages_obj {
                        if let Ok(pages) =
                            doc.get_object(*pages_id).and_then(|o| o.as_dict())
                        {
                            if let Ok(count) = pages.get(b"Count").and_then(|o| o.as_i64()) {
                                out.page_count = Some(count as i32);
                            }
                        }
                    }
                }
            }
        }
    }

    // Avoid an unused-import warning on `ObjectId` when the build is run
    // with features that strip the catalog branch — the type is part of the
    // public lopdf API and could be needed by maintainers reading this file.
    let _: Option<ObjectId> = None;

    Ok(out)
}

/// Convert a PDF Object into a UTF-8 string when it's a string-like type.
/// PDF strings are either raw bytes (PDFDocEncoding) or UTF-16 BE with BOM.
fn pdf_string(obj: &pdf_extract::Object) -> Option<String> {
    use pdf_extract::Object;
    let bytes: &[u8] = match obj {
        Object::String(s, _) => s,
        Object::Name(n) => n,
        _ => return None,
    };
    Some(decode_pdf_string(bytes))
}

fn decode_pdf_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        // UTF-16 BE
        let mut iter = bytes[2..].chunks_exact(2);
        let mut out = String::with_capacity(bytes.len() / 2);
        while let Some(pair) = iter.next() {
            let cp = u16::from_be_bytes([pair[0], pair[1]]);
            if let Some(c) = char::from_u32(cp as u32) {
                out.push(c);
            }
        }
        out
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        // UTF-16 LE (rare)
        let mut iter = bytes[2..].chunks_exact(2);
        let mut out = String::with_capacity(bytes.len() / 2);
        while let Some(pair) = iter.next() {
            let cp = u16::from_le_bytes([pair[0], pair[1]]);
            if let Some(c) = char::from_u32(cp as u32) {
                out.push(c);
            }
        }
        out
    } else {
        // PDFDocEncoding — superset of Latin-1 for the printable range.
        // Simple cast keeps us readable for the western metadata case;
        // exotic special chars may be replaced.
        bytes
            .iter()
            .map(|&b| char::from(b))
            .collect::<String>()
            .trim_end_matches('\0')
            .to_owned()
    }
}

fn parse_pdf_year(s: &str) -> Option<i32> {
    // "D:YYYYMMDDHHmmSS" or "YYYYMMDDHHmmSS"
    let s = s.strip_prefix("D:").unwrap_or(s);
    if s.len() < 4 {
        return None;
    }
    s[..4].parse::<i32>().ok().filter(|y| (1500..=2200).contains(y))
}

// ── DOCX (docProps/core.xml) ───────────────────────────────────────────────

fn read_docx(path: &Path) -> anyhow::Result<L2Metadata> {
    let xml = read_zip_member(path, "docProps/core.xml")?;
    let mut out = L2Metadata::default();
    out.title = extract_xml_tag(&xml, "dc:title").or_else(|| extract_xml_tag(&xml, "title"));
    out.author = extract_xml_tag(&xml, "dc:creator").or_else(|| extract_xml_tag(&xml, "creator"));
    out.language = extract_xml_tag(&xml, "dc:language").or_else(|| extract_xml_tag(&xml, "language"));
    if let Some(date) = extract_xml_tag(&xml, "dcterms:created").or_else(|| extract_xml_tag(&xml, "created")) {
        out.year = parse_iso_year(&date);
    }
    if let Some(s) = extract_xml_tag(&xml, "dc:subject") {
        out.extra.insert("subject".to_owned(), serde_json::Value::String(s));
    }
    Ok(out)
}

// ── Image EXIF (JPG / TIFF / WebP / HEIC / PNG) ────────────────────────────

fn read_image_exif(path: &Path) -> anyhow::Result<L2Metadata> {
    use exif::{In, Reader, Tag};
    let f = File::open(path)?;
    let mut bufreader = std::io::BufReader::new(&f);
    let exifreader = Reader::new();
    let exif = match exifreader.read_from_container(&mut bufreader) {
        Ok(e) => e,
        // Many WebP/PNG files have no EXIF block at all. Treat as empty.
        Err(_) => return Ok(L2Metadata::default()),
    };

    let mut out = L2Metadata::default();

    let primary_field = |tag: Tag| -> Option<String> {
        exif.get_field(tag, In::PRIMARY).map(|f| f.display_value().with_unit(&exif).to_string())
    };

    // ImageDescription often holds a free-form caption / title.
    if let Some(desc) = primary_field(Tag::ImageDescription) {
        let trimmed = desc.trim_matches('"').trim().to_owned();
        if !trimmed.is_empty() && trimmed != "\"\"" {
            out.title = Some(trimmed);
        }
    }
    // Artist / XPAuthor → author.
    if let Some(artist) = primary_field(Tag::Artist) {
        let trimmed = artist.trim_matches('"').trim().to_owned();
        if !trimmed.is_empty() && trimmed != "\"\"" {
            out.author = Some(trimmed);
        }
    }
    // DateTimeOriginal → year (preferred over DateTime which is the file mtime).
    let date_str = primary_field(Tag::DateTimeOriginal)
        .or_else(|| primary_field(Tag::DateTime))
        .or_else(|| primary_field(Tag::DateTimeDigitized));
    if let Some(d) = date_str {
        // EXIF date format: "YYYY:MM:DD HH:MM:SS"
        let cleaned = d.trim_matches('"');
        if cleaned.len() >= 4 {
            if let Ok(y) = cleaned[..4].parse::<i32>() {
                if (1500..=2200).contains(&y) {
                    out.year = Some(y);
                }
            }
        }
    }
    // Extras worth keeping for future use.
    for (key, tag) in [
        ("camera_make", Tag::Make),
        ("camera_model", Tag::Model),
        ("software", Tag::Software),
        ("copyright", Tag::Copyright),
    ] {
        if let Some(v) = primary_field(tag) {
            let trimmed = v.trim_matches('"').trim().to_owned();
            if !trimmed.is_empty() {
                out.extra.insert(key.to_owned(), serde_json::Value::String(trimmed));
            }
        }
    }

    Ok(out)
}

// ── EPUB (OPF manifest) ────────────────────────────────────────────────────

fn read_epub(path: &Path) -> anyhow::Result<L2Metadata> {
    // EPUB layout: META-INF/container.xml → opf path → metadata block.
    let container = read_zip_member(path, "META-INF/container.xml").unwrap_or_default();
    // Find the rootfile path.
    let opf_path = container
        .split("rootfile")
        .nth(1)
        .and_then(|s| s.split("full-path").nth(1))
        .and_then(|s| {
            let q = s.find('"')?;
            let rest = &s[q + 1..];
            let end = rest.find('"')?;
            Some(rest[..end].to_owned())
        })
        .unwrap_or_else(|| "OEBPS/content.opf".to_owned());

    let opf = read_zip_member(path, &opf_path)?;
    let mut out = L2Metadata::default();
    out.title = extract_xml_tag(&opf, "dc:title").or_else(|| extract_xml_tag(&opf, "title"));
    out.author = extract_xml_tag(&opf, "dc:creator").or_else(|| extract_xml_tag(&opf, "creator"));
    out.language = extract_xml_tag(&opf, "dc:language").or_else(|| extract_xml_tag(&opf, "language"));
    if let Some(date) = extract_xml_tag(&opf, "dc:date").or_else(|| extract_xml_tag(&opf, "date")) {
        out.year = parse_iso_year(&date);
    }
    Ok(out)
}

// ── helpers ────────────────────────────────────────────────────────────────

fn read_zip_member(zip_path: &Path, member: &str) -> anyhow::Result<String> {
    let f = File::open(zip_path)?;
    let mut zip = zip::ZipArchive::new(f)?;
    let mut entry = zip.by_name(member)?;
    let mut buf = String::new();
    entry.read_to_string(&mut buf)?;
    Ok(buf)
}

/// Pull the inner text of a single XML tag. Tolerant of attributes and
/// whitespace; not a real XML parser. Returns `None` if not found or empty.
fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let start = xml.find(&open)?;
    let after_open = &xml[start + open.len()..];
    // Skip past attributes to the closing '>'
    let gt = after_open.find('>')?;
    let body = &after_open[gt + 1..];
    let close = format!("</{tag}>");
    let end = body.find(&close)?;
    let txt = body[..end].trim();
    if txt.is_empty() {
        None
    } else {
        Some(decode_xml_entities(txt))
    }
}

fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn parse_iso_year(date: &str) -> Option<i32> {
    let s = date.trim();
    if s.len() < 4 {
        return None;
    }
    s[..4]
        .parse::<i32>()
        .ok()
        .filter(|y| (1500..=2200).contains(y))
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_year_parsing() {
        assert_eq!(parse_pdf_year("D:20240817101530+02'00'"), Some(2024));
        assert_eq!(parse_pdf_year("20231201120000"), Some(2023));
        assert_eq!(parse_pdf_year(""), None);
        assert_eq!(parse_pdf_year("D:1100"), None); // out of range
    }

    #[test]
    fn iso_year_parsing() {
        assert_eq!(parse_iso_year("2024-08-17"), Some(2024));
        assert_eq!(parse_iso_year("2024"), Some(2024));
        assert_eq!(parse_iso_year("not-a-date"), None);
    }

    #[test]
    fn xml_tag_extracts_inner_text() {
        let xml = r#"<x><dc:title xmlns:dc="...">Hello &amp; goodbye</dc:title></x>"#;
        assert_eq!(extract_xml_tag(xml, "dc:title"), Some("Hello & goodbye".to_owned()));
    }

    #[test]
    fn xml_tag_none_when_missing() {
        assert_eq!(extract_xml_tag("<x></x>", "dc:title"), None);
    }

    #[test]
    fn pdf_string_utf16_be() {
        let bytes = b"\xFE\xFF\x00H\x00i";
        assert_eq!(decode_pdf_string(bytes), "Hi");
    }
}
