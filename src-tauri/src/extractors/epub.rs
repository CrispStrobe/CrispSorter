//! Sandboxed EPUB text reader. Follows the package spine in reading order.
//! Malformed packages fail explicitly so the caller can try another parser.
use super::ExtractedDocument;
use anyhow::{anyhow, bail, Context, Result};
use quick_xml::{events::Event, Reader};
use std::{collections::HashMap, fs::File, io::Read, path::Path};
use zip::ZipArchive;

const MAX_MEMBER: u64 = 32 * 1024 * 1024;
const MAX_TOTAL: usize = 128 * 1024 * 1024;

fn member(zip: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    let mut entry = zip
        .by_name(name)
        .with_context(|| format!("missing EPUB member {name}"))?;
    if entry.size() > MAX_MEMBER {
        bail!("EPUB member exceeds size limit: {name}");
    }
    let mut bytes = Vec::new();
    (&mut entry).take(MAX_MEMBER + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MEMBER {
        bail!("EPUB member exceeds size limit: {name}");
    }
    Ok(bytes)
}

fn attr(
    e: &quick_xml::events::BytesStart<'_>,
    key: &[u8],
    reader: &Reader<&[u8]>,
) -> Result<Option<String>> {
    for a in e.attributes().with_checks(false) {
        let a = a?;
        if a.key.local_name().as_ref() == key {
            return Ok(Some(
                a.decode_and_unescape_value(reader.decoder())?.into_owned(),
            ));
        }
    }
    Ok(None)
}

fn package_path(data: &[u8]) -> Result<String> {
    let mut r = Reader::from_reader(data);
    loop {
        match r.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"rootfile" => {
                if let Some(path) = attr(&e, b"full-path", &r)? {
                    return Ok(path);
                }
            }
            Event::Eof => bail!("EPUB container has no rootfile"),
            _ => {}
        }
    }
}

fn spine(data: &[u8], allow_missing: bool) -> Result<Vec<String>> {
    let mut r = Reader::from_reader(data);
    let mut manifest = HashMap::new();
    let mut order = Vec::new();
    loop {
        match r.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"item" => {
                if let (Some(id), Some(href), Some(media)) = (
                    attr(&e, b"id", &r)?,
                    attr(&e, b"href", &r)?,
                    attr(&e, b"media-type", &r)?,
                ) {
                    if media == "application/xhtml+xml" || media == "text/html" {
                        manifest.insert(id, href);
                    }
                }
            }
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"itemref" => {
                if let Some(id) = attr(&e, b"idref", &r)? {
                    order.push(id);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if order.is_empty() {
        bail!("EPUB has no reading-order spine");
    }
    let mut paths = Vec::new();
    for id in order {
        match manifest.get(&id) {
            Some(path) => paths.push(path.clone()),
            None if allow_missing => eprintln!("[epub] WARNING: skipping missing or unsupported spine item {id}; extraction is incomplete"),
            None => bail!("EPUB spine refers to missing or unsupported item: {id}"),
        }
    }
    if paths.is_empty() {
        bail!("EPUB spine has no readable chapters");
    }
    Ok(paths)
}

fn resolve_href(opf: &str, href: &str) -> Result<String> {
    let href = href.split(['#', '?']).next().unwrap_or("");
    let mut bytes = Vec::new();
    let raw = href.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let (Some(a), Some(b)) = (
                (raw[i + 1] as char).to_digit(16),
                (raw[i + 2] as char).to_digit(16),
            ) {
                bytes.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    let decoded = std::str::from_utf8(&bytes).context("invalid EPUB href encoding")?;
    if decoded.starts_with('/') || decoded.contains("://") {
        bail!("external or absolute EPUB spine reference");
    }
    let mut parts = Vec::new();
    let parent = opf.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    for p in parent.split('/').chain(decoded.split('/')) {
        match p {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("EPUB href escapes package root");
                }
            }
            _ => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

pub fn extract(path: &Path) -> Result<ExtractedDocument> {
    extract_inner(path, false)
}

/// Recover readable chapters after the strict reader and other parsers fail.
/// Missing spine items are explicitly reported, since text may be incomplete.
pub fn extract_partial(path: &Path) -> Result<ExtractedDocument> {
    extract_inner(path, true)
}

fn extract_inner(path: &Path, allow_missing: bool) -> Result<ExtractedDocument> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut zip = ZipArchive::new(f).context("invalid EPUB ZIP archive")?;
    let opf = package_path(&member(&mut zip, "META-INF/container.xml")?)?;
    let items = spine(&member(&mut zip, &opf)?, allow_missing)?;
    let mut total = 0usize;
    let mut text = String::new();
    for href in items {
        let name = resolve_href(&opf, &href)?;
        let bytes = member(&mut zip, &name)?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| anyhow!("EPUB size overflow"))?;
        if total > MAX_TOTAL {
            bail!("EPUB text exceeds size limit");
        }
        let chapter = html2text::from_read(bytes.as_slice(), 120)
            .with_context(|| format!("parsing EPUB chapter {name}"))?;
        if !chapter.trim().is_empty() {
            text.push_str(chapter.trim());
            text.push_str("\n\n");
        }
    }
    if text.trim().is_empty() {
        bail!("EPUB contains no extractable text");
    }
    Ok(ExtractedDocument {
        full_text: text,
        ext: "epub".into(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn reads_spine_order_and_decodes_html() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.epub");
        let mut z = zip::ZipWriter::new(File::create(&path).unwrap());
        let opt = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("META-INF/container.xml", "<container><rootfiles><rootfile full-path='OPS/content.opf'/></rootfiles></container>"),
            ("OPS/content.opf", "<package><manifest><item id='a' href='Text/first%20chapter.xhtml' media-type='application/xhtml+xml'/><item id='b' href='Text/second.xhtml' media-type='application/xhtml+xml'/></manifest><spine><itemref idref='b'/><itemref idref='a'/></spine></package>"),
            ("OPS/Text/first chapter.xhtml", "<html><body><p>First &amp; final.</p></body></html>"),
            ("OPS/Text/second.xhtml", "<html><body><script>bad script</script><p>Second chapter.</p></body></html>"),
        ] { z.start_file(name, opt).unwrap(); z.write_all(body.as_bytes()).unwrap(); }
        z.finish().unwrap();
        let d = extract(&path).unwrap();
        assert!(
            d.full_text.find("Second chapter").unwrap()
                < d.full_text.find("First & final").unwrap()
        );
        assert!(!d.full_text.contains("bad script"));
    }
    #[test]
    fn refuses_missing_spine_item_and_path_escape() {
        assert!(spine(
            b"<package><spine><itemref idref='missing'/></spine></package>",
            false
        )
        .is_err());
        assert!(resolve_href("book.opf", "../../outside.xhtml").is_err());
        let package = b"<package><manifest><item id='ok' href='chapter.xhtml' media-type='application/xhtml+xml'/></manifest><spine><itemref idref='missing'/><itemref idref='ok'/></spine></package>";
        assert!(spine(package, false).is_err());
        assert_eq!(spine(package, true).unwrap(), vec!["chapter.xhtml"]);
    }
}
