//! Native extractor adapter shared by foreground, background and CLI ingestion.
use anyhow::{ensure, Result};
use crate::extractors::ExtractedDocument;
use crate::index::container_ingest::{ContainerDocument, RawContainerMember};
use crate::index::ingest::RawDocument;

pub fn indexed_text(extracted: &ExtractedDocument) -> &str {
    extracted.own_text.as_deref().unwrap_or(&extracted.full_text)
}

/// Prepare typed members before callers move fields out of the extraction.
/// Parent metadata overrides remain the caller's responsibility.
pub fn from_extraction(extracted: &ExtractedDocument) -> Result<Option<ContainerDocument>> {
    let Some(source) = &extracted.container_source else {
        ensure!(extracted.members.is_empty() && extracted.coverage.is_none(),
            "Extracted members require a captured container source receipt");
        return Ok(None);
    };
    let coverage = extracted.coverage.clone().unwrap_or_default();
    let mut members = Vec::with_capacity(extracted.members.len());
    for member in &extracted.members {
        let content = &member.document;
        ensure!(content.members.is_empty(), "Native container members must be flattened");
        let filename = member.entries.last().map(|entry|entry.name.rsplit('/').next().unwrap_or(&entry.name).to_owned()).unwrap_or_default();
        let document = RawDocument {
            full_text: indexed_text(content).to_owned(), headings: content.headings.clone(),
            title: content.headings.first().cloned().or_else(||Some(filename.clone())),
            filename, ext: content.ext.clone(), language: content.language.clone().unwrap_or_default(),
            tags: content.tags.clone(), translated_text: content.translated_text.clone(),
            translated_to_lang: content.translated_to_lang.clone(), url: content.source_url.clone(),
            audio_duration_seconds: content.audio.as_ref().and_then(|value|value.duration_seconds),
            audio_codec: content.audio.as_ref().and_then(|value|value.codec.clone()),
            audio_sample_rate_hz: content.audio.as_ref().and_then(|value|value.sample_rate_hz.map(|value|value as i32)),
            audio_channels: content.audio.as_ref().and_then(|value|value.channels.map(|value|value as i32)),
            audio_bitrate_kbps: content.audio.as_ref().and_then(|value|value.bitrate_kbps.map(|value|value as i32)),
            image_camera_make: content.image_exif.as_ref().and_then(|value|value.camera_make.clone()),
            image_camera_model: content.image_exif.as_ref().and_then(|value|value.camera_model.clone()),
            image_lens_model: content.image_exif.as_ref().and_then(|value|value.lens_model.clone()),
            image_taken_at_unix: content.image_exif.as_ref().and_then(|value|value.taken_at_unix),
            image_iso: content.image_exif.as_ref().and_then(|value|value.iso.map(|value|value as i32)),
            ..Default::default()
        };
        members.push(RawContainerMember { entries: member.entries.clone(), document,
            coverage: content.coverage.clone() });
    }
    Ok(Some(ContainerDocument::Root { source: source.clone(), coverage, members }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crisp_index_protocol::{ContainerCoverage,ContainerEntry,ContainerEntryKind,ContainerKind,ContainerSourceReceipt};
    #[test]
    fn native_adapter_indexes_own_bodies_and_preserves_leaf_metadata() {
        let leaf=ExtractedDocument {full_text:"leaf transcript".into(),ext:"txt".into(),language:Some("en".into()),
            translated_text:Some("übersetzter Inhalt".into()),translated_to_lang:Some("de".into()),
            audio:Some(crate::audio::probe::AudioMetadata {duration_seconds:Some(0.0),codec:Some("test".into()),sample_rate_hz:None,channels:None,bitrate_kbps:None}),
            ..Default::default()};
        let extraction=ExtractedDocument {full_text:"preview envelope plus leaf transcript".into(),own_text:Some("envelope only".into()),
            container_source:Some(ContainerSourceReceipt {kind:ContainerKind::Zip,sha256:"a".repeat(64),size:50}),
            coverage:Some(ContainerCoverage {discovered:1,extracted:1,..Default::default()}),
            members:vec![crate::extractors::container::ExtractedMember {entries:vec![ContainerEntry {kind:ContainerEntryKind::Zip,index:3,occurrence:0,name:"folder/leaf.txt".into(),sha256:"b".repeat(64),size:20}],document:leaf}],
            ..Default::default()};
        let mut roots=vec![RawDocument {full_text:indexed_text(&extraction).to_owned(),container:from_extraction(&extraction).unwrap(),
            owner_id:"owner".into(),location_uri:"/records/input.zip".into(),source_hash:"a".repeat(64),parent_dir:Some("/records".into()),volume_id:Some("disk".into()),..Default::default()}];
        crate::index::container_ingest::expand_roots(&mut roots).unwrap();
        assert_eq!(roots.len(),2);assert_eq!(roots[0].full_text,"envelope only");assert_eq!(roots[1].full_text,"leaf transcript");
        assert_eq!(roots[1].translated_to_lang.as_deref(),Some("de"));assert_eq!(roots[1].audio_duration_seconds,Some(0.0));
        assert_eq!(roots[1].filename,"leaf.txt");assert_eq!(roots[1].volume_id.as_deref(),Some("disk"));
        assert_eq!(roots[1].source_hash,"b".repeat(64));
    }
}
