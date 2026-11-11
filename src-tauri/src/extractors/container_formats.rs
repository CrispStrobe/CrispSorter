//! Container stream adapters. They stage generated files under the same budget
//! and return only after CRC, length, and terminal stream checks have succeeded.
use super::{container::*, ExtractedDocument};
use anyhow::{ensure, Result};
use crisp_index_protocol::container::*;
use std::{collections::BTreeMap, fs::File, io::{Read, Seek, SeekFrom}, path::Path};

pub(crate) fn decode(path: &Path, name: &str, kind: ContainerKind, depth: usize, budget: &mut Budget) -> Result<DecodedContainer> {
    let entries = match kind {
        ContainerKind::Zip => zip_entries(path, depth, budget)?,
        ContainerKind::Tar => tar_entries(path, depth, budget)?,
        ContainerKind::Gzip => gzip_entries(path, name, depth, budget)?,
        ContainerKind::Sevenz => sevenz_entries(path, depth, budget)?,
        _ => anyhow::bail!("Unsupported archive container"),
    };
    Ok(DecodedContainer { own: ExtractedDocument::default(), entries })
}

fn canonical(name: &str, budget: &mut Budget) -> Option<String> {
    match normalize_member_name(name) {
        Ok(name) => Some(name), Err(_) => { budget.omitted(if name.is_empty() { "[unnamed entry]" } else { name }, ContainerOmissionReason::UnsafeName); None }
    }
}

fn zip_entries(path: &Path, depth: usize, budget: &mut Budget) -> Result<Vec<StagedEntry>> {
    let mut zip = super::archive::open_zip_shared(path, budget)?;
    let mut entries = Vec::new(); let mut names = BTreeMap::new();
    for index in 0..zip.len() {
        budget.discover(depth + 1)?;
        let raw = zip.by_index_raw(index)?;
        let name = raw.name().to_owned();
        let mode = raw.unix_mode().unwrap_or_default() & 0o170000;
        let linked = raw.is_symlink() || !matches!(mode, 0 | 0o100000 | 0o040000);
        let directory = raw.is_dir(); let encrypted = raw.encrypted(); let size = raw.size(); drop(raw);
        if directory { continue; }
        let Some(name) = canonical(&name, budget) else { continue; };
        let occurrence = next_occurrence(&mut names, &name);
        if linked { budget.omitted(&name, ContainerOmissionReason::Link); continue; }
        if encrypted { budget.omitted(&name, ContainerOmissionReason::Encrypted); continue; }
        entries.push(stage_entry(&mut zip.by_index(index)?, name, ContainerEntryKind::Zip, index, occurrence, Some(size), budget)?);
    }
    Ok(entries)
}

fn tar_entries(path: &Path, depth: usize, budget: &mut Budget) -> Result<Vec<StagedEntry>> {
    // Raw pass bounds extension records before the tar library allocates GNU
    // long names/PAX buffers. Sparse/link/device records never reach a writer.
    let mut preflight = tar::Archive::new(File::open(path)?);
    for entry in preflight.entries()?.raw(true) {
        let mut entry = entry?; budget.discover(depth + 1)?;
        let kind = entry.header().entry_type();
        if kind.is_gnu_longname() || kind.is_gnu_longlink() || kind.is_pax_local_extensions() || kind.is_pax_global_extensions() {
            ensure!(entry.size() <= 64 * 1024, "TAR extension record exceeds its metadata byte limit");
            let mut metadata = Vec::new(); entry.read_to_end(&mut metadata)?;
            ensure!(!metadata.windows(10).any(|bytes| bytes == b"GNU.sparse"), "Sparse TAR extensions are not supported");
        } else { ensure!(!kind.is_gnu_sparse(), "Sparse TAR members are not supported"); }
    }
    let mut archive = tar::Archive::new(File::open(path)?);
    let mut entries = Vec::new(); let mut names = BTreeMap::new();
    for (index, entry) in archive.entries()?.enumerate() {
        let mut entry = entry?;
        let name = match std::str::from_utf8(&entry.path_bytes()) { Ok(name) => name.to_owned(), Err(_) => { budget.omitted("[non-Unicode TAR name]", ContainerOmissionReason::UnsafeName); continue; } };
        let kind = entry.header().entry_type(); if kind.is_dir() { continue; }
        let Some(name) = canonical(&name, budget) else { continue; };
        let occurrence = next_occurrence(&mut names, &name);
        if !kind.is_file() { budget.omitted(&name, if kind.is_symlink() || kind.is_hard_link() { ContainerOmissionReason::Link } else { ContainerOmissionReason::Unsupported }); continue; }
        let size = entry.size();
        entries.push(stage_entry(&mut entry, name, ContainerEntryKind::Tar, index, occurrence, Some(size), budget)?);
    }
    Ok(entries)
}

fn gzip_entries(path: &Path, name: &str, depth: usize, budget: &mut Budget) -> Result<Vec<StagedEntry>> {
    budget.discover(depth + 1)?;
    // MultiGzDecoder consumes every concatenated gzip member, including CRCs.
    // Their logical output is one stream; .tgz/.tar.gz then traverse a TAR step.
    let mut decoder = flate2::read::MultiGzDecoder::new(File::open(path)?);
    let lower = name.to_ascii_lowercase();
    let name = if lower.ends_with(".tgz") { format!("{}.tar", &name[..name.len()-4]) }
        else if lower.ends_with(".gzip") { name[..name.len()-5].to_owned() }
        else if lower.ends_with(".gz") { name[..name.len()-3].to_owned() } else { "payload".to_owned() };
    let Some(name) = canonical(&name, budget) else { return Ok(Vec::new()); };
    Ok(vec![stage_entry(&mut decoder, name, ContainerEntryKind::Gzip, 0, 0, None, budget)?])
}

fn sevenz_entries(path: &Path, depth: usize, budget: &mut Budget) -> Result<Vec<StagedEntry>> {
    use sevenz_rust2::{Archive, BlockDecoder, EncoderMethod as M, Password};
    let mut file = File::open(path)?;
    // The vendored reader caps header allocations separately from this aggregate
    // dictionary budget. It is deliberately decoded with one worker thread.
    let archive = Archive::read_with_memory_limit(&mut file, &Password::empty(), 64 * 1024)?;
    ensure!(archive.files.len() <= budget.limits.entries.saturating_sub(budget.coverage.discovered as usize), "7z entry count exceeds the shared limit");
    let allowed: &[&[u8]] = &[M::ID_COPY, M::ID_LZMA, M::ID_LZMA2, M::ID_DELTA, M::ID_BCJ2, M::ID_BCJ_X86,
        M::ID_BCJ_PPC, M::ID_BCJ_IA64, M::ID_BCJ_ARM, M::ID_BCJ_ARM64, M::ID_BCJ_ARM_THUMB, M::ID_BCJ_SPARC, M::ID_BCJ_RISCV];
    let mut names = BTreeMap::new();
    let mut descriptors = Vec::with_capacity(archive.files.len());
    for (index, entry) in archive.files.iter().enumerate() {
        budget.discover(depth + 1)?;
        let name = canonical(entry.name(), budget);
        let occurrence = name.as_ref().map(|name| next_occurrence(&mut names, name)).unwrap_or(0);
        let reason = if entry.is_directory { None } else if entry.is_anti_item { Some(ContainerOmissionReason::Unsupported) }
            else if !matches!((entry.windows_attributes >> 16) & 0o170000, 0 | 0o100000 | 0o040000) || entry.windows_attributes & 0x400 != 0 { Some(ContainerOmissionReason::Link) }
            else if archive.is_file_encrypted(index) { Some(ContainerOmissionReason::Encrypted) }
            else { None };
        if let (Some(name), Some(reason)) = (&name, reason) { budget.omitted(name, reason); }
        descriptors.push((name, occurrence, reason));
    }
    let mut staged = Vec::new();
    for (block_index, block) in archive.blocks.iter().enumerate() {
        let start = archive.stream_map.block_first_file_index[block_index];
        let password = Password::empty();
        let decoder = BlockDecoder::new_with_memory_limit(1, 64 * 1024, block_index, &archive, &password, &mut file);
        let end = start + decoder.entry_count();
        if block.coders.iter().any(|coder| !allowed.contains(&coder.encoder_method_id())) {
            for index in start..end { let entry = &archive.files[index]; let (name, _, reason) = &descriptors[index];
                if !entry.is_directory && reason.is_none() { if let Some(name) = name { budget.omitted(name, ContainerOmissionReason::Unsupported); } }
            }
            continue;
        }
        let mut next_index = start;
        decoder.for_each_entries(&mut |entry, reader| {
            let index = next_index; next_index += 1;
            let (name, occurrence, reason) = &descriptors[index];
            let result: Result<()> = (|| {
                if entry.is_directory { return Ok(()); }
                if name.is_none() || reason.is_some() {
                    // A skipped solid prefix still costs decompression work.
                    let mut sink = std::io::sink(); let limit = budget.limits.member_bytes;
                    copy_and_hash(&mut reader.take(limit.saturating_add(1)), &mut sink, limit, Some(budget))?;
                } else {
                    staged.push(stage_entry(reader, name.clone().unwrap(), ContainerEntryKind::Sevenz, index, *occurrence, Some(entry.size), budget)?);
                }
                Ok(())
            })();
            result.map(|_| true).map_err(|error| sevenz_rust2::Error::Other(error.to_string().into()))
        })?;
    }
    // Streamless non-directory files do not belong to a compression block.
    for (index, entry) in archive.files.iter().enumerate() {
        if entry.has_stream || entry.is_directory || archive.stream_map.file_block_index[index].is_some() { continue; }
        let (name, occurrence, reason) = &descriptors[index];
        if let (Some(name), None) = (name, reason) { staged.push(stage_entry(&mut std::io::empty(), name.clone(), ContainerEntryKind::Sevenz, index, *occurrence, Some(0), budget)?); }
    }
    staged.sort_by_key(|entry| entry.entry.index);
    file.seek(SeekFrom::Start(0))?;
    Ok(staged)
}
