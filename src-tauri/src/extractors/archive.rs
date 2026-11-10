//! Bounded ZIP document extraction. Member names are provenance, never paths
//! passed to the filesystem. Nested archives share one expansion/text budget.
use super::ExtractedDocument;
use anyhow::{ensure, Context, Result};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use zip::ZipArchive;

#[derive(Clone, Copy)]
struct Limits {
    archive_bytes: u64,
    directory_bytes: u64,
    member_bytes: u64,
    expanded_bytes: u64,
    text_bytes: usize,
    entries: usize,
    depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            archive_bytes: 128 * 1024 * 1024,
            directory_bytes: 8 * 1024 * 1024,
            member_bytes: 32 * 1024 * 1024,
            expanded_bytes: 128 * 1024 * 1024,
            text_bytes: 16 * 1024 * 1024,
            entries: 2048,
            depth: 4,
        }
    }
}

#[derive(Default)]
struct Budget {
    entries: usize,
    expanded_bytes: u64,
    extracted: usize,
    skipped: usize,
    text: String,
    headings: Vec<String>,
    tags: BTreeSet<String>,
}

/// Reuse the application's document/OCR dispatcher for individual supported
/// members. ZIP recursion is handled here so a member cannot reset the limits.
pub fn extract<A, F>(path: &Path, accepts: A, extract_member: F) -> Result<ExtractedDocument>
where
    A: Fn(&str) -> bool,
    F: FnMut(&Path) -> Result<ExtractedDocument>,
{
    extract_with_limits(path, accepts, extract_member, Limits::default())
}

fn extract_with_limits<A, F>(path: &Path, accepts: A, mut extract_member: F, limits: Limits) -> Result<ExtractedDocument>
where
    A: Fn(&str) -> bool,
    F: FnMut(&Path) -> Result<ExtractedDocument>,
{
    let mut budget = Budget::default();
    let label = display_name(&path.file_name().unwrap_or_default().to_string_lossy());
    // Parse a private bounded snapshot, so a concurrently growing/replaced
    // source cannot change the central directory after its allocation guard.
    ensure!(std::fs::metadata(path)?.is_file(), "ZIP source must be a regular file");
    let input = File::open(path).context("opening ZIP source")?;
    let declared = input.metadata()?.len();
    ensure!(declared <= limits.archive_bytes, "ZIP compressed input exceeds its byte limit");
    let mut snapshot = tempfile::Builder::new().prefix("crisp-archive-source-").suffix(".zip").tempfile()?;
    let copied = std::io::copy(&mut input.take(limits.archive_bytes + 1), snapshot.as_file_mut())?;
    ensure!(copied <= limits.archive_bytes && copied == declared, "ZIP compressed input changed size or exceeds its byte limit");
    snapshot.as_file_mut().flush()?;
    walk(snapshot.path(), &label, 0, &limits, &mut budget, &accepts, &mut extract_member)?;
    let detail_end = budget.text.char_indices().map(|(offset, _)| offset).find(|offset| *offset >= 4096).unwrap_or(budget.text.len());
    ensure!(budget.extracted > 0,
        "ZIP contains no searchable document text ({} skipped members). {}", budget.skipped, &budget.text[..detail_end]);
    let summary = format!(
        "ZIP extraction: {} searchable members; {} not extracted; {} archive entries inspected.\n\n",
        budget.extracted, budget.skipped, budget.entries,
    );
    ensure!(summary.len() <= limits.text_bytes.saturating_sub(budget.text.len()), "ZIP extracted text exceeds its byte limit");
    budget.text.insert_str(0, &summary);
    budget.tags.insert("extraction:zip-archive".into());
    if budget.skipped > 0 { budget.tags.insert("extraction:archive-partial".into()); }
    Ok(ExtractedDocument {
        full_text: budget.text, headings: budget.headings, ext: "zip".into(),
        tags: budget.tags.into_iter().collect(), ..Default::default()
    })
}

fn append(budget: &mut Budget, limits: &Limits, text: &str) -> Result<()> {
    ensure!(text.len() <= limits.text_bytes.saturating_sub(budget.text.len()), "ZIP extracted text exceeds its byte limit");
    budget.text.push_str(text);
    Ok(())
}

fn skipped(budget: &mut Budget, limits: &Limits, label: &str, reason: &str, tag: &str) -> Result<()> {
    budget.skipped += 1;
    budget.tags.insert(format!("extraction:archive-skipped-{tag}"));
    append(budget, limits, &format!("[Not extracted: {label} — {reason}]\n\n"))
}

fn display_name(name: &str) -> String {
    let mut display = name.chars().take(2048).flat_map(char::escape_debug).collect::<String>();
    if name.chars().count() > 2048 { display.push('…'); }
    display
}

fn member_name(name: &str) -> Result<String> {
    ensure!(name.len() <= 2048 && !name.is_empty(), "member name exceeds its byte limit or is empty");
    ensure!(!name.chars().any(|c| c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')), "member name contains control characters");
    let normalized = name.replace('\\', "/");
    ensure!(!normalized.starts_with('/') && !normalized.contains(':'), "absolute or drive-qualified member path");
    ensure!(!normalized.split('/').any(|part| part == ".."), "parent traversal in member path");
    let parts = normalized.split('/').filter(|part| !part.is_empty() && *part != ".").collect::<Vec<_>>();
    ensure!(!parts.is_empty(), "empty member path");
    Ok(parts.join("/"))
}

fn le16(bytes: &[u8]) -> u16 { u16::from_le_bytes(bytes[..2].try_into().unwrap()) }
fn le32(bytes: &[u8]) -> u32 { u32::from_le_bytes(bytes[..4].try_into().unwrap()) }
fn le64(bytes: &[u8]) -> u64 { u64::from_le_bytes(bytes[..8].try_into().unwrap()) }

/// Check the bounded tail and central-directory declaration before the ZIP
/// library allocates its entry table. ZIP64 is accepted within the same limits.
fn directory_count(file: &mut File, limits: &Limits, remaining: usize) -> Result<usize> {
    let size = file.metadata()?.len();
    ensure!(size <= limits.archive_bytes, "ZIP compressed input exceeds its byte limit");
    ensure!(size >= 22, "ZIP end-of-directory record is missing");
    let tail_len = size.min(22 + u16::MAX as u64) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail)?;
    let end = (0..=tail_len - 22).rev().find(|&i| {
        tail[i..i + 4] == *b"PK\x05\x06" && i + 22 + le16(&tail[i + 20..]) as usize == tail_len
    }).context("ZIP end-of-directory record is invalid")?;
    let record = &tail[end..];
    ensure!(le16(&record[4..]) == 0 && le16(&record[6..]) == 0, "Multi-volume ZIP archives are unsupported");
    let mut count = le16(&record[10..]) as u64;
    let mut directory_size = le32(&record[12..]) as u64;
    let mut directory_offset = le32(&record[16..]) as u64;
    let mut directory_end = size - tail_len as u64 + end as u64;
    if count == u16::MAX as u64 || directory_size == u32::MAX as u64 || directory_offset == u32::MAX as u64 {
        ensure!(directory_end >= 20 + 56, "ZIP64 locator or directory record is missing");
        file.seek(SeekFrom::Start(directory_end - 20))?;
        let mut locator = [0; 20];
        file.read_exact(&mut locator)?;
        ensure!(locator[..4] == *b"PK\x06\x07" && le32(&locator[4..]) == 0 && le32(&locator[16..]) == 1, "Invalid or multi-volume ZIP64 locator");
        let offset = le64(&locator[8..]);
        ensure!(offset <= directory_end.saturating_sub(20 + 56), "ZIP64 directory offset is outside the archive");
        file.seek(SeekFrom::Start(offset))?;
        let mut record64 = [0; 56];
        file.read_exact(&mut record64)?;
        ensure!(record64[..4] == *b"PK\x06\x06" && le64(&record64[4..]) >= 44, "Invalid ZIP64 directory record");
        ensure!(le64(&record64[4..]) <= directory_end - 20 - offset - 12, "ZIP64 directory record exceeds the archive");
        ensure!(le32(&record64[16..]) == 0 && le32(&record64[20..]) == 0, "Multi-volume ZIP64 archives are unsupported");
        count = le64(&record64[32..]);
        ensure!(le64(&record64[24..]) == count, "Split ZIP64 central directory is unsupported");
        directory_size = le64(&record64[40..]);
        directory_offset = le64(&record64[48..]);
        directory_end = offset;
    } else {
        ensure!(le16(&record[8..]) as u64 == count, "Split ZIP central directory is unsupported");
    }
    ensure!(count <= remaining as u64, "ZIP archive entry count exceeds the shared limit");
    ensure!(directory_size <= limits.directory_bytes, "ZIP central directory exceeds its byte limit");
    ensure!(directory_offset <= directory_end && directory_size <= directory_end - directory_offset, "ZIP central directory is outside the archive");
    file.seek(SeekFrom::Start(0))?;
    Ok(count as usize)
}

fn open_zip(path: &Path, limits: &Limits, budget: &mut Budget) -> Result<ZipArchive<File>> {
    let mut file = File::open(path).context("opening ZIP input")?;
    let count = directory_count(&mut file, limits, limits.entries.saturating_sub(budget.entries))?;
    let zip = ZipArchive::new(file).context("reading ZIP directory")?;
    ensure!(zip.len() == count, "ZIP entry count disagrees with its directory; duplicate member names are ambiguous");
    budget.entries += count;
    Ok(zip)
}

fn copy_member<R: Read, W: Write>(reader: R, output: &mut W, declared: u64, limits: &Limits, budget: &mut Budget) -> Result<()> {
    let available = limits.member_bytes.min(limits.expanded_bytes.saturating_sub(budget.expanded_bytes));
    ensure!(declared <= available, "ZIP member or shared expanded-byte limit exceeded");
    let bytes = std::io::copy(&mut reader.take(available + 1), output).context("reading ZIP member contents or checksum")?;
    ensure!(bytes <= available, "ZIP actual expanded bytes exceed the shared limit");
    ensure!(bytes == declared, "ZIP member length does not match its directory");
    budget.expanded_bytes += bytes;
    Ok(())
}

fn is_package(ext: &str) -> bool {
    matches!(ext, "docx" | "docm" | "pptx" | "pptm" | "ppsx" | "ppsm"
        | "xlsx" | "xlsm" | "xlsb" | "epub" | "odt" | "ods" | "odp")
}

/// ZIP-based documents are handed to their native parser only after their
/// actual package expansion has also been charged to the shared archive budget.
fn verify_package(path: &Path, limits: &Limits, budget: &mut Budget) -> Result<()> {
    let mut zip = open_zip(path, limits, budget)?;
    for index in 0..zip.len() {
        let raw = zip.by_index_raw(index)?;
        member_name(raw.name()).context("unsafe document-package member name")?;
        ensure!(!raw.encrypted() && !raw.is_symlink(), "Encrypted or linked document-package members are unsupported");
        let size = raw.size();
        let directory = raw.is_dir();
        drop(raw);
        if !directory {
            copy_member(zip.by_index(index)?, &mut std::io::sink(), size, limits, budget)?;
        }
    }
    Ok(())
}

fn walk<A, F>(path: &Path, parent: &str, depth: usize, limits: &Limits, budget: &mut Budget, accepts: &A, extract_member: &mut F) -> Result<()>
where
    A: Fn(&str) -> bool,
    F: FnMut(&Path) -> Result<ExtractedDocument>,
{
    ensure!(depth < limits.depth, "ZIP nesting depth exceeds the shared limit at {parent}");
    let mut zip = open_zip(path, limits, budget).with_context(|| format!("archive {parent}"))?;
    for index in 0..zip.len() {
        let raw = zip.by_index_raw(index).with_context(|| format!("reading {parent} entry {}", index + 1))?;
        let name = raw.name().to_owned();
        let label = format!("{parent} ! {} [entry {}]", display_name(&name), index + 1);
        let mode = raw.unix_mode().unwrap_or_default() & 0o170000;
        let encrypted = raw.encrypted();
        let linked = raw.is_symlink() || !matches!(mode, 0 | 0o100000 | 0o040000);
        let directory = raw.is_dir();
        let size = raw.size();
        drop(raw);
        if directory { continue; }
        let normalized = match member_name(&name) {
            Ok(name) => name,
            Err(reason) => { skipped(budget, limits, &label, &reason.to_string(), "unsafe-path")?; continue; }
        };
        if linked { skipped(budget, limits, &label, "symbolic link or special entry", "link")?; continue; }
        if encrypted { skipped(budget, limits, &label, "encrypted member; password required", "encrypted")?; continue; }
        let ext = Path::new(&normalized).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if ext != "zip" && !accepts(&ext) {
            skipped(budget, limits, &label, &format!("unsupported member format .{ext}"), "unsupported")?;
            continue;
        }
        // Only a validated extension reaches the generated scratch filename.
        ensure!(ext.len() <= 16 && ext.chars().all(|c| c.is_ascii_alphanumeric()), "Invalid extraction extension for {label}");
        let mut temporary = tempfile::Builder::new().prefix("crisp-archive-").suffix(&format!(".{ext}")).tempfile()
            .context("creating private archive extraction file")?;
        copy_member(zip.by_index(index).with_context(|| format!("opening {label}"))?, temporary.as_file_mut(), size, limits, budget)
            .with_context(|| format!("extracting archive member {label}"))?;
        temporary.as_file_mut().flush()?;
        if ext == "zip" {
            walk(temporary.path(), &label, depth + 1, limits, budget, accepts, extract_member)?;
            continue;
        }
        if is_package(&ext) { verify_package(temporary.path(), limits, budget).with_context(|| format!("checking package {label}"))?; }
        let doc = extract_member(temporary.path()).with_context(|| format!("extracting archive member {label}"))?;
        if doc.full_text.trim().is_empty() {
            skipped(budget, limits, &label, "no searchable text; extraction settings or OCR may be required", "no-text")?;
            continue;
        }
        append(budget, limits, &format!("Archive member: {label}\n"))?;
        append(budget, limits, &doc.full_text)?;
        append(budget, limits, "\n\n")?;
        budget.headings.push(format!("Archive member: {label}"));
        budget.extracted += 1;
        // Coverage tags survive without promoting a member's user collection
        // or source URL to the identity/metadata of the enclosing ZIP.
        budget.tags.extend(doc.tags.into_iter().filter(|tag| tag.starts_with("extraction:")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            writer.start_file(*name, SimpleFileOptions::default().compression_method(CompressionMethod::Stored)).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn source(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut source = tempfile::Builder::new().suffix(".zip").tempfile().unwrap();
        source.write_all(bytes).unwrap();
        source.flush().unwrap();
        source
    }

    fn fixture_extract(path: &Path) -> Result<ExtractedDocument> {
        match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "html" => super::super::html::extract(path),
            "eml" => super::super::eml::extract(path),
            _ => super::super::text::extract(path),
        }
    }

    fn accepts(ext: &str) -> bool { matches!(ext, "txt" | "md" | "html" | "eml") }

    #[test]
    fn nested_text_html_and_mail_keep_member_provenance_and_native_decoding() {
        let inner = zip_bytes(&[
            ("pages/receipt.html", b"<h1>Receipt</h1><p>Paid &amp; delivered</p><script>hidden payload</script>"),
            ("mail.eml", b"Subject: Dispatch\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nReady to ship"),
        ]);
        let outer = source(&zip_bytes(&[("notes.md", b"# Project\nArchive source text"), ("nested.zip", &inner)]));
        let doc = extract(outer.path(), accepts, fixture_extract).unwrap();
        assert!(doc.full_text.contains("3 searchable members; 0 not extracted"));
        assert!(doc.full_text.contains("nested.zip [entry 2] ! pages/receipt.html [entry 1]"));
        assert!(doc.full_text.contains("Paid & delivered"));
        assert!(doc.full_text.contains("Ready to ship"));
        assert!(!doc.full_text.contains("hidden payload"));
        assert_eq!(doc.headings.len(), 3);
        assert_eq!(doc.ext, "zip");
        assert!(doc.tags.contains(&"extraction:zip-archive".to_string()));
    }

    #[test]
    fn uses_generated_temporary_paths_and_removes_them_after_dispatch() {
        let archive = source(&zip_bytes(&[("deep/private/report.txt", b"retained document")]));
        let mut paths = Vec::new();
        let doc = extract(archive.path(), accepts, |path| {
            assert!(path.file_name().unwrap().to_str().unwrap().starts_with("crisp-archive-"));
            assert!(!path.to_string_lossy().contains("private/report"));
            assert_eq!(path.extension().unwrap(), "txt");
            paths.push(path.to_owned());
            fixture_extract(path)
        }).unwrap();
        assert!(doc.full_text.contains("deep/private/report.txt"));
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn rejects_path_traversal_and_links_without_extracting_their_contents() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["../outside.txt", "/absolute.txt", "C:\\private.txt", "notes.exe", "safe.txt"] {
            writer.start_file(name, SimpleFileOptions::default()).unwrap();
            writer.write_all(if name == "safe.txt" { b"searchable" } else { b"do not index unsafe contents" }).unwrap();
        }
        writer.add_symlink("linked.txt", "/outside/secret.txt", SimpleFileOptions::default()).unwrap();
        let archive = source(&writer.finish().unwrap().into_inner());
        let doc = extract(archive.path(), accepts, fixture_extract).unwrap();
        assert!(doc.full_text.contains("1 searchable members; 5 not extracted"));
        assert!(!doc.full_text.contains("do not index unsafe contents"));
        assert!(!doc.full_text.contains("/outside/secret.txt"));
        for tag in ["extraction:archive-partial", "extraction:archive-skipped-unsafe-path", "extraction:archive-skipped-unsupported", "extraction:archive-skipped-link"] {
            assert!(doc.tags.iter().any(|value| value == tag), "missing {tag}");
        }
    }

    #[test]
    fn missing_content_is_visible_and_an_archive_of_names_only_is_not_success() {
        let archive = source(&zip_bytes(&[("program.exe", b"binary")]));
        let error = format!("{:#}", extract(archive.path(), accepts, fixture_extract).unwrap_err());
        assert!(error.contains("no searchable document text"));
        assert!(error.contains("unsupported member format .exe"));
    }

    #[test]
    fn limits_compressed_input_before_reading_the_directory() {
        let bytes = zip_bytes(&[("readme.txt", b"body")]);
        let archive = source(&bytes);
        let limits = Limits { archive_bytes: bytes.len() as u64 - 1, ..Limits::default() };
        let error = extract_with_limits(archive.path(), accepts, |_| panic!("must not dispatch"), limits).unwrap_err();
        assert!(error.to_string().contains("compressed input"));
    }

    #[test]
    fn limits_directory_bytes_and_declared_count_before_allocating_entries() {
        let mut bytes = zip_bytes(&[("body.txt", b"body")]);
        let archive = source(&bytes);
        let error = format!("{:#}", extract_with_limits(archive.path(), accepts,
            |_| panic!("must not dispatch"), Limits { directory_bytes: 1, ..Limits::default() }).unwrap_err());
        assert!(error.contains("central directory exceeds"));
        let end = bytes.len() - 22;
        bytes[end + 8..end + 10].copy_from_slice(&2049u16.to_le_bytes());
        bytes[end + 10..end + 12].copy_from_slice(&2049u16.to_le_bytes());
        let forged = source(&bytes);
        let error = format!("{:#}", extract(forged.path(), accepts, |_| panic!("must not dispatch")).unwrap_err());
        assert!(error.contains("entry count exceeds"));
    }

    #[test]
    fn a_forged_small_uncompressed_size_cannot_evade_actual_byte_checks() {
        let mut bytes = zip_bytes(&[("body.txt", b"actual content is longer than declared")]);
        let central = bytes.windows(4).position(|part| part == b"PK\x01\x02").unwrap();
        bytes[central + 24..central + 28].copy_from_slice(&1u32.to_le_bytes());
        let archive = source(&bytes);
        assert!(extract_with_limits(archive.path(), accepts, |_| panic!("must not dispatch"),
            Limits { member_bytes: 8, ..Limits::default() }).is_err());
    }

    #[test]
    fn nested_archives_share_the_entry_limit() {
        let inner = zip_bytes(&[("one.txt", b"one"), ("two.txt", b"two")]);
        let archive = source(&zip_bytes(&[("first.txt", b"first"), ("more.zip", &inner)]));
        let error = format!("{:#}", extract_with_limits(archive.path(), accepts, fixture_extract, Limits { entries: 3, ..Limits::default() }).unwrap_err());
        assert!(error.contains("shared limit"));
    }

    #[test]
    fn nested_archives_share_the_depth_limit() {
        let deepest = zip_bytes(&[("body.txt", b"body")]);
        let inner = zip_bytes(&[("deep.zip", &deepest)]);
        let archive = source(&zip_bytes(&[("more.zip", &inner)]));
        let error = format!("{:#}", extract_with_limits(archive.path(), accepts, fixture_extract, Limits { depth: 2, ..Limits::default() }).unwrap_err());
        assert!(error.contains("nesting depth"));
        assert!(error.contains("deep.zip"));
    }

    #[test]
    fn enforces_per_member_and_total_expanded_byte_limits() {
        let archive = source(&zip_bytes(&[("one.txt", b"123456789"), ("two.txt", b"123456789")]));
        for limits in [Limits { member_bytes: 8, ..Limits::default() }, Limits { expanded_bytes: 16, ..Limits::default() }] {
            let error = format!("{:#}", extract_with_limits(archive.path(), accepts, fixture_extract, limits).unwrap_err());
            assert!(error.contains("expanded-byte limit"));
        }
    }

    #[test]
    fn package_expansion_is_charged_before_the_native_document_reader_runs() {
        let package = zip_bytes(&[("word/document.xml", b"12345678901234567890")]);
        let archive = source(&zip_bytes(&[("report.docx", &package)]));
        let limits = Limits { expanded_bytes: package.len() as u64 + 19, ..Limits::default() };
        let error = format!("{:#}", extract_with_limits(archive.path(), |ext| ext == "docx", |_| panic!("package must fail preflight"), limits).unwrap_err());
        assert!(error.contains("checking package"));
        assert!(error.contains("expanded-byte limit"));
    }

    #[test]
    fn a_text_limit_error_does_not_return_partial_success() {
        let archive = source(&zip_bytes(&[("body.txt", b"input")]));
        let error = extract_with_limits(archive.path(), accepts, |_| Ok(ExtractedDocument {
            full_text: "é".repeat(100), ..Default::default()
        }), Limits { text_bytes: 128, ..Limits::default() }).unwrap_err();
        assert!(error.to_string().contains("text exceeds"));
    }

    #[test]
    fn a_failed_supported_member_fails_the_whole_refresh_with_its_identity() {
        let archive = source(&zip_bytes(&[("good.txt", b"good"), ("broken.txt", b"broken")]));
        let error = format!("{:#}", extract(archive.path(), accepts, |path| {
            let doc = fixture_extract(path)?;
            ensure!(doc.full_text != "broken", "fixture native parser failed");
            Ok(doc)
        }).unwrap_err());
        assert!(error.contains("broken.txt [entry 2]"));
        assert!(error.contains("fixture native parser failed"));
    }

    #[test]
    fn validates_checksums_before_dispatching_a_member() {
        let mut bytes = zip_bytes(&[("body.txt", b"unique content")]);
        let offset = bytes.windows(14).position(|part| part == b"unique content").unwrap();
        bytes[offset] ^= 0x20;
        let archive = source(&bytes);
        let error = format!("{:#}", extract(archive.path(), accepts, |_| panic!("checksum must fail first")).unwrap_err());
        assert!(error.contains("checksum"));
        assert!(error.contains("body.txt"));
    }

    #[test]
    fn refuses_ambiguous_duplicate_member_names() {
        let mut bytes = zip_bytes(&[("a.txt", b"first"), ("b.txt", b"second")]);
        for offset in (0..bytes.len() - 4).collect::<Vec<_>>() {
            if &bytes[offset..offset + 5] == b"b.txt" { bytes[offset] = b'a'; }
        }
        let archive = source(&bytes);
        let error = format!("{:#}", extract(archive.path(), accepts, |_| panic!("ambiguous names must fail")).unwrap_err());
        assert!(error.contains("duplicate") || error.contains("Duplicate"));
    }

    #[test]
    fn encrypted_members_are_reported_as_unread_without_claiming_their_text() {
        let mut bytes = zip_bytes(&[("locked.txt", b"protected secret"), ("plain.txt", b"plain text")]);
        for offset in 0..bytes.len() - 46 {
            let local = &bytes[offset..offset + 4] == b"PK\x03\x04";
            let central = &bytes[offset..offset + 4] == b"PK\x01\x02";
            let name_offset = if local { 30 } else if central { 46 } else { continue };
            if bytes.get(offset + name_offset..offset + name_offset + 10) == Some(b"locked.txt") {
                bytes[offset + if local { 6 } else { 8 }] |= 1;
            }
        }
        let archive = source(&bytes);
        let doc = extract(archive.path(), accepts, fixture_extract).unwrap();
        assert!(!doc.full_text.contains("protected secret"));
        assert!(doc.full_text.contains("encrypted member; password required"));
        assert!(doc.tags.contains(&"extraction:archive-skipped-encrypted".to_string()));
    }

    #[test]
    fn supports_bounded_zip64_directories() {
        let bytes = zip_bytes(&[("body.txt", b"ZIP64 body")]);
        let end = bytes.len() - 22;
        let old = &bytes[end..];
        let mut updated = bytes[..end].to_vec();
        updated.extend_from_slice(b"PK\x06\x06");
        updated.extend_from_slice(&44u64.to_le_bytes());
        updated.extend_from_slice(&45u16.to_le_bytes());
        updated.extend_from_slice(&45u16.to_le_bytes());
        updated.extend_from_slice(&0u64.to_le_bytes());
        updated.extend_from_slice(&(le16(&old[8..]) as u64).to_le_bytes());
        updated.extend_from_slice(&(le16(&old[10..]) as u64).to_le_bytes());
        updated.extend_from_slice(&(le32(&old[12..]) as u64).to_le_bytes());
        updated.extend_from_slice(&(le32(&old[16..]) as u64).to_le_bytes());
        updated.extend_from_slice(b"PK\x06\x07");
        updated.extend_from_slice(&0u32.to_le_bytes());
        updated.extend_from_slice(&(end as u64).to_le_bytes());
        updated.extend_from_slice(&1u32.to_le_bytes());
        let mut end_record = old.to_vec();
        end_record[8..12].fill(0xff);
        end_record[12..20].fill(0xff);
        updated.extend_from_slice(&end_record);
        let archive = source(&updated);
        let doc = extract(archive.path(), accepts, fixture_extract).unwrap();
        assert!(doc.full_text.contains("ZIP64 body"));
    }

    #[test]
    fn no_text_and_member_coverage_tags_survive_without_changing_container_identity() {
        let archive = source(&zip_bytes(&[("empty.txt", b""), ("tagged.txt", b"body")]));
        let doc = extract(archive.path(), accepts, |path| {
            let mut doc = fixture_extract(path)?;
            doc.source_url = Some("https://member.example".into());
            doc.tags = vec!["collection:member-only".into(), "extraction:member-warning".into()];
            Ok(doc)
        }).unwrap();
        assert!(doc.full_text.contains("no searchable text"));
        assert!(doc.tags.contains(&"extraction:member-warning".to_string()));
        assert!(!doc.tags.contains(&"collection:member-only".to_string()));
        assert!(doc.source_url.is_none());
    }
}
