//! Machine-readable AI provenance on artifacts that leave the app.
//!
//! AI Act Art 50(2) attaches to the **content**, not to the window it was shown
//! in. A badge in the UI tells the person at the keyboard; it tells nobody who
//! receives the file afterwards. So documents we generate carry the mark
//! themselves.
//!
//! Currently: machine-translated `.docx`. Not needed for TTS audio (CrispASR
//! watermarks the signal) and not applicable to ASR transcripts or OCR text,
//! which are renderings of real input rather than generated content.
//!
//! # Why the OOXML dance
//!
//! A `.docx` is a zip. The obvious implementation — "patch `docProps/core.xml`
//! if present" — silently does nothing for packages that lack it, which is the
//! class of check that cannot fail and therefore protects nothing. So when the
//! part is missing this creates it *properly*: the part, its `[Content_Types]`
//! override, and its package relationship. A marker that applies to only some
//! documents is worse than none, because it invites the belief that it applies
//! to all of them.

use std::io::{Cursor, Read, Write};
use std::path::Path;

const CORE_PART: &str = "docProps/core.xml";
const CONTENT_TYPES: &str = "[Content_Types].xml";
const ROOT_RELS: &str = "_rels/.rels";
const CORE_CT: &str = "application/vnd.openxmlformats-package.core-properties+xml";
const CORE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";

/// The marker text. Deliberately readable by a human *and* greppable by a
/// machine — `cp:contentStatus` is a string field, so it does double duty.
pub const DOCX_MARK: &str = "AI-generated: machine translation (CrispSorter)";

fn core_xml_with(status: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><cp:contentStatus>{status}</cp:contentStatus><dc:description>{status}</dc:description></cp:coreProperties>"#
    )
}

/// Replace an existing `cp:contentStatus`, or insert one, without disturbing the
/// rest of the part. Also refreshes `dc:description` when it already carries one
/// of our marks, so re-stamping does not accumulate duplicates.
fn patch_core_xml(existing: &str, status: &str) -> String {
    let mut out = existing.to_owned();

    let replace_between = |s: &str, open: &str, close: &str, value: &str| -> Option<String> {
        let a = s.find(open)?;
        let b = s[a..].find(close)? + a;
        Some(format!("{}{open}{value}{}", &s[..a], &s[b..]))
    };

    if out.contains("<cp:contentStatus>") {
        if let Some(next) = replace_between(&out, "<cp:contentStatus>", "</cp:contentStatus>", status)
        {
            out = next;
        }
    } else if let Some(idx) = out.find("</cp:coreProperties>") {
        out.insert_str(idx, &format!("<cp:contentStatus>{status}</cp:contentStatus>"));
    } else {
        // Not a shape we recognise — replace wholesale rather than leave the
        // document unmarked while reporting success.
        return core_xml_with(status);
    }

    if out.contains("<dc:description>") {
        if let Some(next) = replace_between(&out, "<dc:description>", "</dc:description>", status) {
            out = next;
        }
    } else if let Some(idx) = out.find("</cp:coreProperties>") {
        out.insert_str(idx, &format!("<dc:description>{status}</dc:description>"));
    }
    out
}

fn ensure_content_type(xml: &str) -> String {
    if xml.contains(CORE_CT) {
        return xml.to_owned();
    }
    let override_tag =
        format!(r#"<Override PartName="/{CORE_PART}" ContentType="{CORE_CT}"/>"#);
    match xml.rfind("</Types>") {
        Some(i) => format!("{}{override_tag}{}", &xml[..i], &xml[i..]),
        None => xml.to_owned(),
    }
}

fn ensure_relationship(xml: &str) -> String {
    if xml.contains(CORE_REL_TYPE) {
        return xml.to_owned();
    }
    // A distinctive Id rather than a numeric one: `rId7` might already exist,
    // and a duplicate relationship Id makes Word reject the package.
    let rel = format!(
        r#"<Relationship Id="rIdCrispAiProv" Type="{CORE_REL_TYPE}" Target="{CORE_PART}"/>"#
    );
    match xml.rfind("</Relationships>") {
        Some(i) => format!("{}{rel}{}", &xml[..i], &xml[i..]),
        None => xml.to_owned(),
    }
}

/// Stamp a `.docx` in place with a machine-readable AI-provenance marker.
///
/// Rewrites the package because zip archives cannot be edited in place. Reads
/// fully into memory first — `.docx` files are small, and a streaming rewrite
/// that fails halfway would leave a corrupt document where a translated one
/// used to be.
pub fn stamp_docx(path: &Path, status: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut archive = zip::ZipArchive::new(Cursor::new(&bytes))
        .map_err(|e| format!("{} is not a readable zip: {e}", path.display()))?;

    // Pull every entry out first; the writer needs them in one pass.
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut f = archive
            .by_index(i)
            .map_err(|e| format!("reading zip entry {i}: {e}"))?;
        let name = f.name().to_owned();
        let mut buf = Vec::new();
        // Propagate rather than default: a part we failed to read would be
        // written back empty, quietly corrupting the document we are marking.
        f.read_to_end(&mut buf)
            .map_err(|e| format!("reading {name}: {e}"))?;
        entries.push((name, buf));
    }

    let had_core = entries.iter().any(|(n, _)| n == CORE_PART);
    for (name, data) in entries.iter_mut() {
        match name.as_str() {
            CORE_PART => {
                let existing = String::from_utf8_lossy(data).into_owned();
                *data = patch_core_xml(&existing, status).into_bytes();
            }
            CONTENT_TYPES => {
                let existing = String::from_utf8_lossy(data).into_owned();
                *data = ensure_content_type(&existing).into_bytes();
            }
            ROOT_RELS => {
                let existing = String::from_utf8_lossy(data).into_owned();
                *data = ensure_relationship(&existing).into_bytes();
            }
            _ => {}
        }
    }
    if !had_core {
        entries.push((CORE_PART.to_owned(), core_xml_with(status).into_bytes()));
    }

    let mut out = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut out));
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in &entries {
            writer
                .start_file(name.as_str(), opts)
                .map_err(|e| format!("writing {name}: {e}"))?;
            writer
                .write_all(data)
                .map_err(|e| format!("writing {name}: {e}"))?;
        }
        writer.finish().map_err(|e| format!("finishing zip: {e}"))?;
    }
    std::fs::write(path, out).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_docx() -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut out));
            let o: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            for (name, body) in [
                (CONTENT_TYPES, r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#),
                (ROOT_RELS, r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#),
                ("word/document.xml", r#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body/></w:document>"#),
            ] {
                w.start_file(name, o).unwrap();
                w.write_all(body.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        out
    }

    fn parts(bytes: &[u8]) -> Vec<(String, String)> {
        let mut a = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..a.len())
            .map(|i| {
                let mut f = a.by_index(i).unwrap();
                let n = f.name().to_owned();
                let mut s = String::new();
                let _ = f.read_to_string(&mut s);
                (n, s)
            })
            .collect()
    }

    fn write_tmp(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("out.docx");
        std::fs::write(&p, bytes).unwrap();
        (d, p)
    }

    #[test]
    fn a_package_without_core_properties_gets_a_complete_one() {
        // The case a naive "patch if present" implementation would skip while
        // reporting success — which is the whole reason this module exists.
        let (_d, p) = write_tmp(&minimal_docx());
        stamp_docx(&p, DOCX_MARK).unwrap();

        let got = parts(&std::fs::read(&p).unwrap());
        let core = got.iter().find(|(n, _)| n == CORE_PART).expect("core.xml created");
        assert!(core.1.contains(DOCX_MARK), "marker present");
        assert!(core.1.contains("cp:contentStatus"), "machine-readable field");

        let ct = got.iter().find(|(n, _)| n == CONTENT_TYPES).unwrap();
        assert!(ct.1.contains(CORE_CT), "content-type override added, or Word rejects the part");

        let rels = got.iter().find(|(n, _)| n == ROOT_RELS).unwrap();
        assert!(rels.1.contains(CORE_REL_TYPE), "package relationship added");
    }

    #[test]
    fn an_existing_core_part_is_patched_not_replaced() {
        let mut base = minimal_docx();
        // Rebuild with a core.xml that carries a title we must not destroy.
        {
            let mut out = Vec::new();
            {
                let mut w = zip::ZipWriter::new(Cursor::new(&mut out));
                let o: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
                for (n, b) in parts(&base) {
                    w.start_file(n.as_str(), o).unwrap();
                    w.write_all(b.as_bytes()).unwrap();
                }
                w.start_file(CORE_PART, o).unwrap();
                w.write_all(br#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Important Title</dc:title></cp:coreProperties>"#).unwrap();
                w.finish().unwrap();
            }
            base = out;
        }
        let (_d, p) = write_tmp(&base);
        stamp_docx(&p, DOCX_MARK).unwrap();

        let got = parts(&std::fs::read(&p).unwrap());
        let core = &got.iter().find(|(n, _)| n == CORE_PART).unwrap().1;
        assert!(core.contains("Important Title"), "must not clobber existing metadata");
        assert!(core.contains(DOCX_MARK), "marker added");
    }

    #[test]
    fn stamping_twice_does_not_duplicate_the_marker() {
        let (_d, p) = write_tmp(&minimal_docx());
        stamp_docx(&p, DOCX_MARK).unwrap();
        stamp_docx(&p, DOCX_MARK).unwrap();
        let got = parts(&std::fs::read(&p).unwrap());
        let core = &got.iter().find(|(n, _)| n == CORE_PART).unwrap().1;
        assert_eq!(core.matches("<cp:contentStatus>").count(), 1, "one status element");
        assert_eq!(
            got.iter().filter(|(n, _)| n == CORE_PART).count(),
            1,
            "one core part, not two zip entries with the same name"
        );
        let ct = &got.iter().find(|(n, _)| n == CONTENT_TYPES).unwrap().1;
        assert_eq!(ct.matches(CORE_CT).count(), 1, "one content-type override");
    }

    #[test]
    fn the_document_body_survives_the_rewrite() {
        // Rewriting the package must not lose parts — a lost document.xml would
        // mean a translated file replaced by a corrupt one.
        let (_d, p) = write_tmp(&minimal_docx());
        stamp_docx(&p, DOCX_MARK).unwrap();
        let got = parts(&std::fs::read(&p).unwrap());
        assert!(got.iter().any(|(n, _)| n == "word/document.xml"), "body preserved");
    }

    #[test]
    fn a_non_zip_is_reported_rather_than_silently_skipped() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("not.docx");
        std::fs::write(&p, b"plain text").unwrap();
        assert!(stamp_docx(&p, DOCX_MARK).is_err());
    }
}
