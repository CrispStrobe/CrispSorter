use super::*;
use std::io::{Cursor, Write};

fn source(ext: &str, bytes: &[u8]) -> tempfile::NamedTempFile {
    let mut file = tempfile::Builder::new().suffix(&format!(".{ext}")).tempfile().unwrap();
    file.write_all(bytes).unwrap(); file.flush().unwrap(); file
}
fn leaf(path: &Path) -> Result<ExtractedDocument> {
    match extension(&path.to_string_lossy()).as_str() {
        "html" => super::super::html::extract(path), "pptx" => super::super::pptx::extract(path),
        _ => super::super::text::extract(path),
    }
}
fn run(path: &Path) -> Result<ExtractedDocument> { extract(path, |ext| matches!(ext,"txt"|"html"|"md"|"pptx"), leaf) }
fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries { writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap(); writer.write_all(bytes).unwrap(); }
    writer.finish().unwrap().into_inner()
}
fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = tar::Builder::new(Vec::new());
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu(); header.set_size(bytes.len() as u64); header.set_mode(0o600); header.set_cksum();
        writer.append_data(&mut header, *name, *bytes).unwrap();
    }
    writer.into_inner().unwrap()
}
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut writer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()); writer.write_all(bytes).unwrap(); writer.finish().unwrap()
}
fn locator(root: &ExtractedDocument, entries: Vec<ContainerEntry>, path: &Path) -> ContainerMemberLocator {
    ContainerMemberLocator { version: CONTAINER_LOCATOR_VERSION, root_owner_id:"owner".into(), root_doc_id:"root-occurrence".into(),
        root_location_uri:path.to_string_lossy().into_owned(), root_sha256: root.container_source.as_ref().unwrap().sha256.clone(), entries }
}
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let value = ((chunk[0] as usize)<<16) | ((chunk.get(1).copied().unwrap_or(0) as usize)<<8) | chunk.get(2).copied().unwrap_or(0) as usize;
        for index in 0..4 { result.push(if index > chunk.len() { '=' } else { ALPHABET[(value >> (18 - 6*index)) & 63] as char }); }
    }
    result
}

#[test]
fn archive_members_are_independent_receipted_and_reopen_exact_bytes() {
    let file = source("zip", &zip_bytes(&[("notes.txt", b"Distinct member content."),("unknown.bin",b"\0binary")]));
    let result = run(file.path()).unwrap();
    assert_eq!(result.members.len(),1); assert!(result.full_text.contains("Distinct member content."));
    assert!(!result.own_text.as_ref().unwrap().contains("Distinct member content."));
    let member = &result.members[0]; assert_eq!(member.document.full_text,"Distinct member content.");
    let expected = locator(&result,member.entries.clone(),file.path());
    let materialized = materialize_member(file.path(),&expected).unwrap();
    assert_eq!(std::fs::read(materialized.file.path()).unwrap(),b"Distinct member content.");
    assert_ne!(materialized.file.path(),Path::new("notes.txt"));
    assert_eq!(result.coverage.as_ref().unwrap().skipped,1);
    assert_eq!(result.coverage.as_ref().unwrap().omissions[0].reason,ContainerOmissionReason::Unsupported);
    let mut tampered = expected.clone(); tampered.entries[0].sha256 = "0".repeat(64); assert!(materialize_member(file.path(),&tampered).is_err());
    std::fs::write(file.path(),zip_bytes(&[("notes.txt",b"Changed source content.")])).unwrap();
    assert!(materialize_member(file.path(),&expected).unwrap_err().to_string().contains("changed since indexing"));
}

#[test]
fn tar_duplicate_names_have_distinct_ids_and_stable_reorder_identity() {
    let file = source("tar",&tar_bytes(&[("same.txt",b"First"),("different.txt",b"Other"),("same.txt",b"Second")]));
    let doc = run(file.path()).unwrap(); assert_eq!(doc.members.len(),3);
    assert_eq!(doc.members[2].entries[0].occurrence,1);
    let ids = doc.members.iter().map(|member| member_id("owner","root",&member.entries).unwrap()).collect::<std::collections::BTreeSet<_>>(); assert_eq!(ids.len(),3);
    let expected = locator(&doc,doc.members[2].entries.clone(),file.path());
    assert_eq!(std::fs::read(materialize_member(file.path(),&expected).unwrap().file.path()).unwrap(),b"Second");
    let other = source("tar",&tar_bytes(&[("different.txt",b"Changed other"),("same.txt",b"First updated"),("same.txt",b"Second updated")]));
    let refreshed = run(other.path()).unwrap();
    assert_eq!(member_id("owner","root",&doc.members[0].entries).unwrap(),member_id("owner","root",&refreshed.members[1].entries).unwrap());
}

#[test]
fn concatenated_gzip_streams_and_tgz_traverse_one_decoded_payload() {
    let bytes = tar_bytes(&[("nested.txt",b"Across concatenated gzip streams.")]);
    let half = bytes.len()/2; let mut compressed = gzip(&bytes[..half]); compressed.extend(gzip(&bytes[half..]));
    let file = source("tgz",&compressed); let doc = run(file.path()).unwrap();
    let member = doc.members.iter().find(|member| member.document.ext=="txt").unwrap();
    assert_eq!(member.entries.len(),2); assert_eq!(member.entries[0].kind,ContainerEntryKind::Gzip); assert_eq!(member.entries[1].kind,ContainerEntryKind::Tar);
    let expected = locator(&doc,member.entries.clone(),file.path());
    assert_eq!(std::fs::read(materialize_member(file.path(),&expected).unwrap().file.path()).unwrap(),b"Across concatenated gzip streams.");
    let crc_offset = compressed.len()-8; compressed[crc_offset] ^= 1; let broken = source("tgz",&compressed); assert!(run(broken.path()).is_err());
}

#[test]
fn binary_mime_attachments_use_native_presentation_and_nested_archive_readers() {
    let directory = tempfile::tempdir().unwrap(); let deck = super::super::pptx::tests_support::write_sample_deck(directory.path());
    let archive = zip_bytes(&[("slides.pptx",&std::fs::read(deck).unwrap())]);
    let raw = format!("Subject: Parent subject\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain\r\n\r\nParent body only.\r\n--x\r\nContent-Type: application/zip\r\nContent-Disposition: attachment; filename=collection.zip\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--x--\r\n",base64(&archive));
    let file = source("eml",raw.as_bytes()); let doc = run(file.path()).unwrap();
    assert!(doc.own_text.as_ref().unwrap().contains("Parent body only.")); assert!(!doc.own_text.as_ref().unwrap().contains("Erste Folie"));
    let slide = doc.members.iter().find(|member|member.document.ext=="pptx").unwrap();
    assert_eq!(slide.entries.len(),2); assert_eq!(slide.entries[0].kind,ContainerEntryKind::MimePart);
    assert!(slide.document.full_text.contains("Nicht zu schnell sprechen"));
    assert_eq!(std::fs::read(materialize_member(file.path(),&locator(&doc,slide.entries.clone(),file.path())).unwrap().file.path()).unwrap(),std::fs::read(super::super::pptx::tests_support::write_sample_deck(directory.path())).unwrap());
}

#[test]
fn mime_charset_and_nested_messages_retain_decoded_byte_receipts() {
    let raw = b"Subject: Parent\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain; charset=windows-1252\r\nContent-Disposition: attachment; filename=notes.txt\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nCaf=E9\r\n--x\r\nContent-Type: message/rfc822\r\nContent-Disposition: attachment; filename=forwarded.data\r\n\r\nSubject: Child\r\nContent-Type: multipart/mixed; boundary=y\r\n\r\n--y\r\nContent-Type: text/plain\r\n\r\nForwarded body.\r\n--y\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=inside.txt\r\n\r\nNested attachment.\r\n--y--\r\n--x--\r\n";
    let file = source("eml",raw); let doc = run(file.path()).unwrap();
    let text = doc.members.iter().find(|member|member.entries.last().unwrap().name=="notes.txt").unwrap();
    assert!(text.document.full_text.contains("Café"));
    let reopened = materialize_member(file.path(),&locator(&doc,text.entries.clone(),file.path())).unwrap();
    assert!(std::fs::read(reopened.file.path()).unwrap().windows(4).any(|bytes|bytes==b"Caf\xe9"));
    let nested = doc.members.iter().find(|member|member.entries.last().unwrap().name=="inside.txt").unwrap();
    assert_eq!(nested.entries.len(),2); assert_eq!(nested.entries[0].name,"forwarded.data");
    assert!(std::fs::read_to_string(materialize_member(file.path(),&locator(&doc,nested.entries.clone(),file.path())).unwrap().file.path()).unwrap().contains("Nested attachment."));
}

#[test]
fn mailbox_messages_are_independent_and_malformed_binary_transfer_aborts() {
    let raw = b"From a@example.test Thu Jan 1 00:00:00 2026\nMessage-ID: <first@example.test>\nSubject: First\n\nAlpha.\nFrom b@example.test Thu Jan 1 00:00:01 2026\nMessage-ID: <second@example.test>\nSubject: Second\n\nBeta.\n";
    let file = source("mbox",raw); let doc = run(file.path()).unwrap(); assert_eq!(doc.members.len(),2);
    assert!(doc.members[0].document.full_text.contains("Alpha.")); assert!(!doc.members[0].document.full_text.contains("Beta."));
    assert_ne!(doc.members[0].entries[0].name,doc.members[1].entries[0].name);
    let corrupt = source("eml",b"Subject: Bad\nContent-Type: multipart/mixed; boundary=x\n\n--x\nContent-Type: application/pdf\nContent-Disposition: attachment; filename=a.pdf\nContent-Transfer-Encoding: base64\n\n%%%\n--x--\n");
    assert!(run(corrupt.path()).unwrap_err().to_string().contains("Malformed email attachment"));
}

#[test]
fn cross_format_limits_never_return_a_partial_expansion() {
    let tar = tar_bytes(&[("inner.txt",b"member body")]); let archive=zip_bytes(&[("nested.tar",&tar)]); let file=source("zip",&archive);
    assert!(extract_with_limits(file.path(),|_|true,&mut leaf,Limits{depth:1,..Limits::default()}).is_err());
    assert!(extract_with_limits(file.path(),|_|true,&mut leaf,Limits{entries:1,..Limits::default()}).is_err());
    assert!(extract_with_limits(file.path(),|_|true,&mut leaf,Limits{expanded_bytes:tar.len() as u64+5,..Limits::default()}).is_err());
    assert!(extract_with_limits(file.path(),|_|true,&mut leaf,Limits{text_bytes:4,..Limits::default()}).is_err());
}

#[test]
fn copy_and_solid_lzma2_7z_entries_are_reopenable_and_budgeted() {
    for bytes in [include_bytes!("../../../tests/extraction-contract/fixtures/copy-members.7z").as_slice(),include_bytes!("../../../tests/extraction-contract/fixtures/solid-lzma2.7z").as_slice(),include_bytes!("../../../tests/extraction-contract/fixtures/solid-lzma2-header.7z").as_slice()] {
        let file=source("7z",bytes); let doc=run(file.path()).unwrap(); assert_eq!(doc.members.len(),2);
        assert_eq!(doc.members[1].document.full_text,"Second solid document.");
        let opened=materialize_member(file.path(),&locator(&doc,doc.members[1].entries.clone(),file.path())).unwrap();
        assert_eq!(std::fs::read(opened.file.path()).unwrap(),b"Second solid document.");
        assert!(extract_with_limits(file.path(),|_|true,&mut leaf,Limits{expanded_bytes:25,..Limits::default()}).is_err());
        let mut corrupt=bytes.to_vec();corrupt[32]^=1;assert!(run(source("7z",&corrupt).path()).is_err());
    }
}

#[test]
fn tar_unsafe_links_and_oversized_extension_records_are_explicit() {
    let mut builder = tar::Builder::new(Vec::new());
    let mut unsafe_header = tar::Header::new_gnu(); unsafe_header.set_size(6); unsafe_header.set_mode(0o600);
    unsafe_header.as_mut_bytes()[..13].copy_from_slice(b"../escape.txt"); unsafe_header.set_cksum();
    builder.append(&unsafe_header, b"unsafe".as_slice()).unwrap();
    let mut linked = tar::Header::new_gnu(); linked.set_size(0); linked.set_entry_type(tar::EntryType::Symlink); linked.set_mode(0o600);
    builder.append_link(&mut linked,"linked.txt","../outside").unwrap();
    let archive = source("tar",&builder.into_inner().unwrap()); let document = run(archive.path()).unwrap();
    assert!(document.members.is_empty()); let coverage = document.coverage.unwrap(); assert_eq!(coverage.skipped,2);
    assert_eq!(coverage.omissions[0].reason,ContainerOmissionReason::UnsafeName); assert_eq!(coverage.omissions[1].reason,ContainerOmissionReason::Link);
    let mut builder=tar::Builder::new(Vec::new()); let mut header=tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::new(b'x'));header.set_size(65537);header.set_mode(0o600);header.set_cksum();
    builder.append_data(&mut header,"metadata",Cursor::new(vec![b'x';65537])).unwrap();
    let archive=source("tar",&builder.into_inner().unwrap()); assert!(run(archive.path()).unwrap_err().to_string().contains("metadata byte limit"));
    assert!(run(source("eml",b"").path()).is_err());
}
