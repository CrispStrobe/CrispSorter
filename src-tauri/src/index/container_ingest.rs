//! Container input validation and owner-bound member expansion before indexing.
use anyhow::{ensure, Context, Result};
use crisp_index_protocol::container::{ContainerCoverage, ContainerEntry, ContainerMemberLocator,
    ContainerSourceReceipt, CONTAINER_LOCATOR_VERSION, MAX_CONTAINER_MEMBERS, MAX_CONTAINER_TEXT_BYTES};
use crate::index::ingest::RawDocument;

#[derive(Debug, Clone)]
pub enum ContainerDocument {
    Root { source: ContainerSourceReceipt, coverage: ContainerCoverage, members: Vec<RawContainerMember> },
    Member { locator: ContainerMemberLocator, coverage: Option<ContainerCoverage> },
}

#[derive(Debug, Clone)]
pub struct RawContainerMember {
    pub entries: Vec<ContainerEntry>,
    pub document: RawDocument,
    pub coverage: Option<ContainerCoverage>,
}

/// Content is supplied by the extractor. Identity and source provenance are
/// always bound to the selected root by the native ingestion path.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberContentInput {
    #[serde(default)] pub full_text: String,
    #[serde(default)] pub full_text_md: String,
    #[serde(default)] pub headings: Vec<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub year: Option<i32>,
    #[serde(default)] pub filename: String,
    #[serde(default)] pub ext: String,
    #[serde(default)] pub language: String,
    #[serde(default)] pub tags: Vec<String>,
    #[serde(default, flatten)] pub metadata: crate::index::ingest_metadata::IngestMetadata,
    pub container_coverage: Option<ContainerCoverage>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberInput {
    pub entries: Vec<ContainerEntry>,
    pub document: MemberContentInput,
}

impl MemberInput {
    pub fn into_raw(self) -> Result<RawContainerMember> {
        crisp_index_protocol::container::validate_entry_chain(&self.entries).map_err(anyhow::Error::msg)?;
        let content = self.document;
        content.metadata.validate()?;
        ensure!(content.full_text.len() <= MAX_CONTAINER_TEXT_BYTES && content.full_text_md.len() <= MAX_CONTAINER_TEXT_BYTES,
            "Container member text exceeds the shared extraction limit");
        ensure!(content.filename.len() <= 2048 && content.ext.len() <= 64 && content.language.len() <= 128,
            "Container member display metadata exceeds its limit");
        ensure!(content.headings.len() <= 4096 && content.headings.iter().all(|value| value.len() <= 8192),
            "Container member headings exceed their limit");
        ensure!(content.tags.len() <= 1024 && content.tags.iter().all(|value| value.len() <= 1024),
            "Container member tags exceed their limit");
        if let Some(coverage) = &content.container_coverage { coverage.validate().map_err(anyhow::Error::msg)?; }
        let document = RawDocument {
            full_text: content.full_text, full_text_md: content.full_text_md, headings: content.headings,
            title: content.title, author: content.author, year: content.year,
            filename: content.filename, ext: content.ext, language: content.language, tags: content.tags,
            translated_text: content.metadata.text_translated, translated_to_lang: content.metadata.text_translated_lang,
            audio_duration_seconds: content.metadata.audio_duration_seconds,
            image_camera_make: content.metadata.image_camera_make, image_camera_model: content.metadata.image_camera_model,
            url: content.metadata.url,
            ..Default::default()
        };
        Ok(RawContainerMember { entries: self.entries, document, coverage: content.container_coverage })
    }
}

/// Expand only flattened members. A root and every child remain one write job.
/// Returned ordering keeps roots first; their receipt is independent of children.
pub(crate) fn expand_roots(roots: &mut Vec<RawDocument>) -> Result<()> {
    let mut children = Vec::new();
    for root in roots.iter_mut() {
        let root_id = crate::index::ingest::doc_id_for(root);
        let Some(container) = &mut root.container else { continue; };
        let ContainerDocument::Root { source, coverage, members } = container else {
            anyhow::bail!("Container members must be indexed with their root source");
        };
        source.validate().map_err(anyhow::Error::msg)?;
        coverage.validate().map_err(anyhow::Error::msg)?;
        ensure!(source.sha256.eq_ignore_ascii_case(&root.source_hash),
            "Container source receipt differs from the captured root fingerprint");
        ensure!(members.len() <= MAX_CONTAINER_MEMBERS, "Container member count exceeds the shared limit");
        ensure!(coverage.extracted as usize >= members.len(), "Container coverage omits extracted member documents");
        let mut text_bytes = root.full_text.len().checked_add(root.full_text_md.len()).context("Container text size overflow")?;
        let mut identities = std::collections::BTreeSet::new();
        for member in std::mem::take(members) {
            ensure!(member.document.container.is_none(), "Container members must be flattened before indexing");
            let locator = ContainerMemberLocator {
                version: CONTAINER_LOCATOR_VERSION, root_owner_id: root.owner_id.clone(), root_doc_id: root_id.clone(),
                root_location_uri: root.location_uri.clone(), root_sha256: root.source_hash.to_ascii_lowercase(), entries: member.entries,
            };
            locator.validate().map_err(anyhow::Error::msg)?;
            let id = locator.member_id().map_err(anyhow::Error::msg)?;
            ensure!(identities.insert(id), "Duplicate container member identity in one extraction");
            let mut child = member.document;
            text_bytes = text_bytes.checked_add(child.full_text.len()).and_then(|bytes| bytes.checked_add(child.full_text_md.len()))
                .context("Container text size overflow")?;
            ensure!(text_bytes <= MAX_CONTAINER_TEXT_BYTES, "Combined container text exceeds the shared extraction limit");
            child.owner_id = root.owner_id.clone();
            child.location_uri = locator.member_uri().map_err(anyhow::Error::msg)?;
            let leaf = locator.entries.last().context("Container entry chain is empty")?;
            child.source_hash = leaf.sha256.to_ascii_lowercase();
            child.file_size = Some(leaf.size as i64);
            if child.filename.is_empty() { child.filename = leaf.name.rsplit('/').next().unwrap_or(&leaf.name).to_owned(); }
            if child.ext.is_empty() { child.ext = child.filename.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default(); }
            child.parent_dir = root.parent_dir.clone();
            child.volume_id = root.volume_id.clone();
            child.mtime_unix = root.mtime_unix;
            child.container = Some(ContainerDocument::Member { locator, coverage: member.coverage });
            children.push(child);
        }
        ensure!(text_bytes <= MAX_CONTAINER_TEXT_BYTES, "Combined container text exceeds the shared extraction limit");
    }
    roots.extend(children);
    Ok(())
}

pub(crate) fn attach_metadata(raw: &RawDocument, previous: Option<String>) -> Option<String> {
    let Some(container) = &raw.container else { return previous; };
    let mut metadata: serde_json::Value = previous.as_deref().and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    match container {
        ContainerDocument::Root { source, coverage, .. } => {
            metadata["container_source"] = serde_json::json!(source);
            metadata["container_coverage"] = serde_json::json!(coverage);
            metadata["container_group_version"] = 1.into();
        }
        ContainerDocument::Member { locator, coverage } => {
            metadata["container_member"] = serde_json::json!(locator);
            if let Some(coverage) = coverage { metadata["container_coverage"] = serde_json::json!(coverage); }
        }
    }
    Some(metadata.to_string())
}

pub(crate) fn stored_locator(metadata: Option<&str>) -> Result<Option<ContainerMemberLocator>> {
    let value: serde_json::Value = metadata.map(serde_json::from_str).transpose()?.unwrap_or(serde_json::Value::Null);
    value.get("container_member").filter(|value| !value.is_null()).cloned().map(serde_json::from_value)
        .transpose().context("Stored container locator is invalid")
}

pub fn requires_member_refresh(extension:&str,state:Option<&crate::index::local_index::IndexedState>)->bool {
    matches!(extension.to_ascii_lowercase().as_str(),"zip"|"tar"|"gz"|"gzip"|"tgz"|"7z"|"eml"|"mbox")
        && state.is_some_and(|state|!state.extraction_skip&&state.container_group_version!=crisp_index_protocol::CONTAINER_MEMBERS_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historical_roots_refresh_once_and_explicit_skips_remain_respected() {
        let mut state=crate::index::local_index::IndexedState {mtime_unix:Some(123),images_indexed:true,
            source_hash:Some("a".repeat(64)),extraction_failed:false,extraction_skip:false,container_group_version:0};
        assert!(requires_member_refresh("ZIP",Some(&state)));
        assert!(requires_member_refresh("mbox",Some(&state)));
        assert!(!requires_member_refresh("pdf",Some(&state)));
        assert!(!requires_member_refresh("zip",None));
        state.extraction_skip=true;assert!(!requires_member_refresh("zip",Some(&state)));
        state.extraction_skip=false;state.container_group_version=1;
        assert!(!requires_member_refresh("zip",Some(&state)));
    }
}
