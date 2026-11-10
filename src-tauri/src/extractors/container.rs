//! Shared receipts and budgets for document containers and MIME attachments.
//! Every filesystem write uses a generated private path, never a member name.
use super::ExtractedDocument;
use anyhow::{ensure, Context, Result};
use crisp_index_protocol::container::*;
use sha2::{Digest, Sha256};
use std::{fs::File, io::{Read, Write}, path::Path};

#[derive(Debug, Clone)]
pub struct ExtractedMember {
    pub entries: Vec<ContainerEntry>,
    pub document: ExtractedDocument,
}

#[derive(Debug)]
pub struct MaterializedMember {
    pub file: tempfile::NamedTempFile,
    pub extension: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub archive_bytes: u64,
    pub mailbox_bytes: u64,
    pub directory_bytes: u64,
    pub member_bytes: u64,
    pub expanded_bytes: u64,
    pub text_bytes: usize,
    pub entries: usize,
    pub depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self { archive_bytes: MAX_CONTAINER_ARCHIVE_BYTES, mailbox_bytes: MAX_CONTAINER_MAILBOX_BYTES,
            directory_bytes: 8 * 1024 * 1024, member_bytes: MAX_CONTAINER_MEMBER_BYTES,
            expanded_bytes: MAX_CONTAINER_EXPANDED_BYTES, text_bytes: MAX_CONTAINER_TEXT_BYTES,
            entries: MAX_CONTAINER_MEMBERS, depth: MAX_CONTAINER_DEPTH }
    }
}

pub(crate) struct Budget {
    pub limits: Limits,
    pub coverage: ContainerCoverage,
    pub expanded: u64,
    pub text: usize,
}
impl Budget {
    pub fn new(limits: Limits) -> Self { Self { limits, coverage: ContainerCoverage::default(), expanded: 0, text: 0 } }

    pub fn discover(&mut self, depth: usize) -> Result<()> {
        ensure!(depth <= self.limits.depth, "Container nesting depth exceeds the shared limit");
        ensure!((self.coverage.discovered as usize) < self.limits.entries, "Container entry count exceeds the shared limit");
        self.coverage.discovered += 1; Ok(())
    }

    pub fn charge_expanded(&mut self, bytes: usize) -> Result<()> {
        ensure!(bytes as u64 <= self.limits.expanded_bytes.saturating_sub(self.expanded), "Container expanded-byte limit exceeded");
        self.expanded += bytes as u64; Ok(())
    }

    pub fn charge_text(&mut self, text: &str) -> Result<()> {
        ensure!(text.len() <= self.limits.text_bytes.saturating_sub(self.text), "Container extracted text exceeds the shared byte limit");
        self.text += text.len(); Ok(())
    }

    pub fn omitted(&mut self, path: &str, reason: ContainerOmissionReason) {
        self.coverage.skipped += 1;
        if self.coverage.omissions.len() < MAX_CONTAINER_OMISSION_DETAILS {
            let display = path.chars().flat_map(char::escape_debug).collect::<String>();
            let limit = MAX_CONTAINER_NAME_BYTES * MAX_CONTAINER_DEPTH;
            let end = if display.len() <= limit { display.len() } else { display.char_indices().map(|(offset, _)| offset).take_while(|offset| *offset <= limit).last().unwrap_or(0) };
            self.coverage.omissions.push(ContainerOmission { path: display[..end].to_owned(), reason });
        } else { self.coverage.omitted_details += 1; }
    }
}

pub(crate) struct SourceSnapshot {
    pub file: tempfile::NamedTempFile,
    pub receipt: ContainerSourceReceipt,
}

pub(crate) fn snapshot_source(path: &Path, kind: ContainerKind, limits: &Limits) -> Result<SourceSnapshot> {
    let limit = if kind == ContainerKind::Mailbox { limits.mailbox_bytes } else { limits.archive_bytes };
    let mut input = File::open(path).context("Opening container source")?;
    let metadata = input.metadata()?;
    ensure!(metadata.is_file(), "Container source must be a regular file");
    ensure!(metadata.len() <= limit, "Container compressed input exceeds its byte limit");
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or("container");
    let extension = safe_extension(extension).unwrap_or("container");
    let mut file = tempfile::Builder::new().prefix("crisp-container-source-").suffix(&format!(".{extension}")).tempfile()?;
    let (sha256, size) = copy_and_hash(&mut input, file.as_file_mut(), limit, None)?;
    ensure!(size == metadata.len(), "Container source changed size during its private snapshot");
    file.as_file_mut().flush()?;
    Ok(SourceSnapshot { file, receipt: ContainerSourceReceipt { kind, sha256, size } })
}

/// Count actual bytes before writing, including when a container lies about an
/// entry size. A failed copy is discarded with its private temporary file.
pub(crate) fn copy_and_hash(input: &mut impl Read, output: &mut impl Write, limit: u64,
    mut budget: Option<&mut Budget>) -> Result<(String, u64)> {
    let mut hash = Sha256::new(); let mut total = 0u64; let mut bytes = [0u8; 64 * 1024];
    loop {
        let requested = bytes.len().min(limit.saturating_sub(total).saturating_add(1) as usize);
        let count = input.read(&mut bytes[..requested])?;
        if count == 0 { break; }
        ensure!(count as u64 <= limit.saturating_sub(total), "Container member expanded-byte limit exceeded");
        if let Some(budget) = budget.as_deref_mut() { budget.charge_expanded(count)?; }
        output.write_all(&bytes[..count])?; hash.update(&bytes[..count]); total += count as u64;
    }
    Ok((format!("{:x}", hash.finalize()), total))
}

pub(crate) fn safe_extension(value: &str) -> Option<&str> {
    (!value.is_empty() && value.len() <= 16 && value.bytes().all(|ch| ch.is_ascii_alphanumeric())).then_some(value)
}


/// Container dispatch is kept outside the leaf registry so nested inputs never
/// reset the shared expansion budget or bypass their receipt checks.
pub fn kind_for_extension(extension: &str) -> Option<ContainerKind> {
    match extension.to_ascii_lowercase().as_str() {
        "zip" => Some(ContainerKind::Zip), "tar" => Some(ContainerKind::Tar),
        "gz" | "gzip" | "tgz" => Some(ContainerKind::Gzip), "7z" => Some(ContainerKind::Sevenz),
        "eml" => Some(ContainerKind::Email), "mbox" => Some(ContainerKind::Mailbox), _ => None,
    }
}

pub(crate) fn extension(name: &str) -> String {
    Path::new(name).extension().and_then(|part| part.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub(crate) struct StagedEntry { pub entry: ContainerEntry, pub file: tempfile::NamedTempFile, pub document: Option<ExtractedDocument>, pub kind_hint: Option<ContainerKind> }
pub(crate) struct DecodedContainer { pub own: ExtractedDocument, pub entries: Vec<StagedEntry> }

pub(crate) fn stage_entry(input: &mut dyn Read, name: String, kind: ContainerEntryKind, index: usize,
    occurrence: u32, declared: Option<u64>, budget: &mut Budget) -> Result<StagedEntry> {
    if let Some(size) = declared { ensure!(size <= budget.limits.member_bytes, "Container member exceeds its decoded-byte limit"); }
    let ext = extension(&name);
    let mut file = tempfile::Builder::new().prefix("crisp-container-member-")
        .suffix(&format!(".{}", safe_extension(&ext).unwrap_or("bin"))).tempfile()?;
    let (sha256, size) = copy_and_hash(&mut input.take(budget.limits.member_bytes.saturating_add(1)), file.as_file_mut(), budget.limits.member_bytes, Some(budget))?;
    ensure!(declared.is_none_or(|value| value == size), "Container member byte count disagrees with its declared size");
    file.flush()?;
    Ok(StagedEntry { entry: ContainerEntry { kind, index: index.try_into()?, occurrence, name, sha256, size }, file, document: None, kind_hint: None })
}

pub(crate) fn next_occurrence(names: &mut std::collections::BTreeMap<String, u32>, name: &str) -> u32 {
    let count = names.entry(name.to_owned()).or_default(); let occurrence = *count; *count += 1; occurrence
}

fn decode(path: &Path, name: &str, kind: ContainerKind, depth: usize, budget: &mut Budget) -> Result<DecodedContainer> {
    ensure!(depth < budget.limits.depth, "Container nesting depth exceeds the shared limit");
    match kind {
        ContainerKind::Email => super::eml::decode_container(path, depth, budget),
        ContainerKind::Mailbox => super::eml::decode_mailbox(path, depth, budget),
        _ => super::container_formats::decode(path, name, kind, depth, budget),
    }
}

pub fn extract<A, F>(path: &Path, accepts: A, mut extract_leaf: F) -> Result<ExtractedDocument>
where A: Fn(&str) -> bool, F: FnMut(&Path) -> Result<ExtractedDocument> {
    extract_with_limits(path, accepts, &mut extract_leaf, Limits::default())
}

fn extract_with_limits<A, F>(path: &Path, accepts: A, extract_leaf: &mut F, limits: Limits) -> Result<ExtractedDocument>
where A: Fn(&str) -> bool, F: FnMut(&Path) -> Result<ExtractedDocument> {
    let name = path.file_name().and_then(|name| name.to_str()).context("Container filename is not valid Unicode")?;
    let kind = kind_for_extension(&extension(name)).context("Unsupported container extension")?;
    let source = snapshot_source(path, kind, &limits)?;
    let mut budget = Budget::new(limits);
    let mut members = Vec::new();
    let mut root = walk(source.file.path(), name, kind, &[], &mut budget, &accepts, extract_leaf, &mut members)?;
    // Keep the parent content separate from the aggregate preview. Member text
    // is indexed under each member's own stable identity by the ingest layer.
    let mut preview = root.full_text.clone();
    for member in &members {
        let label = member.entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>().join(" ! ");
        let heading = format!("\n\nContainer member: {label}\n"); budget.charge_text(&heading)?;
        preview.push_str(&heading); preview.push_str(&member.document.full_text);
    }
    if budget.coverage.skipped > 0 { root.tags.push("extraction:container-partial".into()); }
    root.own_text = Some(root.full_text);
    root.full_text = preview;
    root.members = members;
    root.container_source = Some(source.receipt);
    budget.coverage.validate().map_err(anyhow::Error::msg)?;
    root.coverage = Some(budget.coverage);
    Ok(root)
}

fn walk<A, F>(path: &Path, name: &str, kind: ContainerKind, chain: &[ContainerEntry], budget: &mut Budget,
    accepts: &A, extract_leaf: &mut F, members: &mut Vec<ExtractedMember>) -> Result<ExtractedDocument>
where A: Fn(&str) -> bool, F: FnMut(&Path) -> Result<ExtractedDocument> {
    let skipped_before = budget.coverage.skipped;
    let decoded = decode(path, name, kind, chain.len(), budget)?;
    let had_entries = !decoded.entries.is_empty();
    let mut own = decoded.own;
    // All decoding streams are dropped before children are parsed, bounding
    // simultaneous dictionaries even for nested solid archives.
    let mut extracted = 0usize;
    for staged in decoded.entries {
        let mut entries = chain.to_vec(); entries.push(staged.entry.clone());
        validate_entry_chain(&entries).map_err(anyhow::Error::msg)?;
        let ext = extension(&staged.entry.name);
        let mut document = if let Some(doc) = staged.document {
            budget.charge_text(&doc.full_text)?; doc
        } else if let Some(child_kind) = staged.kind_hint.or_else(|| kind_for_extension(&ext)) {
            walk(staged.file.path(), &staged.entry.name, child_kind, &entries, budget, accepts, extract_leaf, members)?
        } else if accepts(&ext) {
            if matches!(ext.as_str(), "docx" | "epub" | "pptx") {
                super::archive::verify_document_package_shared(staged.file.path(), budget)?;
            }
            let mut doc = extract_leaf(staged.file.path()).with_context(|| format!("Extracting container member {}", staged.entry.name))?;
            ensure!(doc.members.is_empty() && doc.container_source.is_none(), "Leaf extractor returned an unexpected nested expansion");
            doc.ext = ext;
            budget.charge_text(&doc.full_text)?;
            doc
        } else { budget.omitted(&provenance(&entries), ContainerOmissionReason::Unsupported); continue; };
        if document.full_text.trim().is_empty() {
            budget.omitted(&provenance(&entries), ContainerOmissionReason::NoText); continue;
        }
        document.own_text = None; document.members.clear(); document.container_source = None;
        budget.coverage.extracted += 1; extracted += 1;
        members.push(ExtractedMember { entries, document });
    }
    if own.full_text.trim().is_empty() {
        ensure!(kind != ContainerKind::Email || had_entries || budget.coverage.skipped > skipped_before, "Email contains no searchable body or attachment coverage");
        own.full_text = format!("Container: {name}\n{extracted} directly readable members. Coverage is recorded with this source.\n");
    }
    budget.charge_text(&own.full_text)?;
    own.ext = match kind { ContainerKind::Email => "eml".into(), ContainerKind::Mailbox => "mbox".into(), _ => extension(name) };
    own.tags.push(format!("extraction:container-{}", match kind { ContainerKind::Zip => "zip", ContainerKind::Tar => "tar", ContainerKind::Gzip => "gzip", ContainerKind::Sevenz => "7z", ContainerKind::Email => "email", ContainerKind::Mailbox => "mailbox" }));
    Ok(own)
}

fn provenance(entries: &[ContainerEntry]) -> String { entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>().join(" ! ") }

/// Reopen one indexed member from the captured source generation. The caller
/// supplies a stored, owner-validated locator, never an arbitrary member path.
/// Every step is decoded again and checked against its exact byte receipt.
pub fn materialize_member(root_path: &Path, locator: &ContainerMemberLocator) -> Result<MaterializedMember> {
    locator.validate().map_err(anyhow::Error::msg)?;
    let kind = kind_for_extension(&extension(&root_path.to_string_lossy())).context("Unsupported container source extension")?;
    let mut budget = Budget::new(Limits::default());
    let source = snapshot_source(root_path, kind, &budget.limits)?;
    ensure!(source.receipt.sha256.eq_ignore_ascii_case(&locator.root_sha256), "Container source changed since indexing; refresh this source before opening its members");
    let mut current = source.file;
    let mut current_name = root_path.file_name().and_then(|name| name.to_str()).unwrap_or("container").to_owned();
    let mut current_kind = kind;
    let mut current_extension = extension(&current_name);
    for (depth, expected) in locator.entries.iter().enumerate() {
        let decoded = decode(current.path(), &current_name, current_kind, depth, &mut budget)?;
        let found = decoded.entries.into_iter().find(|entry| entry.entry.index == expected.index)
            .context("Indexed container entry is no longer available")?;
        ensure!(found.entry.kind == expected.kind && found.entry.name == expected.name && found.entry.occurrence == expected.occurrence
            && found.entry.size == expected.size && found.entry.sha256.eq_ignore_ascii_case(&expected.sha256), "Indexed container member receipt does not match the decoded source");
        current_extension = if found.kind_hint == Some(ContainerKind::Email) { "eml".into() }
            else if let Some(document) = &found.document { document.ext.clone() } else { extension(&found.entry.name) };
        current = found.file; current_name = found.entry.name;
        if depth + 1 < locator.entries.len() { current_kind = found.kind_hint.or_else(|| kind_for_extension(&extension(&current_name))).context("Indexed chain traverses a non-container member")?; }
    }
    let last = locator.entries.last().context("Container locator has no member")?;
    Ok(MaterializedMember { file: current, extension: safe_extension(&current_extension).unwrap_or("bin").to_owned(), sha256: last.sha256.clone(), size: last.size })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn snapshots_capture_exact_bytes_and_never_reuse_a_member_path() {
        let mut input = tempfile::Builder::new().suffix(".zip").tempfile().unwrap();
        input.write_all(b"captured container bytes").unwrap(); input.flush().unwrap();
        let snapshot = snapshot_source(input.path(), ContainerKind::Zip, &Limits::default()).unwrap();
        assert_ne!(snapshot.file.path(), input.path());
        assert_eq!(snapshot.receipt.sha256, format!("{:x}", Sha256::digest(b"captured container bytes")));
        assert_eq!(snapshot.receipt.size, 24);
        std::fs::write(input.path(), b"changed source").unwrap();
        assert_eq!(std::fs::read(snapshot.file.path()).unwrap(), b"captured container bytes");
    }

    #[test]
    fn nested_actual_byte_work_shares_one_expansion_limit() {
        let mut budget = Budget::new(Limits { expanded_bytes: 7, ..Limits::default() });
        copy_and_hash(&mut Cursor::new(b"first"), &mut Vec::new(), 8, Some(&mut budget)).unwrap();
        assert!(copy_and_hash(&mut Cursor::new(b"next"), &mut Vec::new(), 8, Some(&mut budget)).is_err());
        assert_eq!(budget.expanded, 5);
        assert!(copy_and_hash(&mut Cursor::new(b"oversize"), &mut Vec::new(), 7, None).is_err());
    }

    #[test]
    fn shared_entry_text_and_coverage_limits_are_explicit() {
        let mut budget = Budget::new(Limits { entries: 2, depth: 1, text_bytes: 3, ..Limits::default() });
        budget.discover(1).unwrap(); budget.discover(1).unwrap(); assert!(budget.discover(1).is_err());
        assert!(budget.discover(2).is_err()); budget.charge_text("abc").unwrap(); assert!(budget.charge_text("x").is_err());
        for index in 0..70 { budget.omitted(&format!("unsafe\nname {index}"), ContainerOmissionReason::UnsafeName); }
        assert_eq!(budget.coverage.omissions.len(), 64); assert_eq!(budget.coverage.omitted_details, 6);
        assert!(budget.coverage.omissions.iter().all(|item| !item.path.contains('\n')));
    }
}

#[cfg(test)]
#[path = "container_tests.rs"]
mod contract_tests;
