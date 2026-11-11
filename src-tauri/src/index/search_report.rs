//! Streaming metadata reports shared by local and remote complete searches.
use std::{cmp::Ordering as CompareOrder, io::Write, path::PathBuf, sync::{Arc, atomic::{AtomicU8, Ordering}}};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Default)]
pub struct ScanCancellation {
    phase: AtomicU8,
    changed: tokio::sync::Notify,
}
impl ScanCancellation {
    pub(crate) fn is_cancelled(&self) -> bool { self.phase.load(Ordering::Acquire) == 1 }
    /// Once the atomic publication begins it must report its actual outcome;
    /// callers cannot claim cancellation after the destination was replaced.
    pub fn cancel(&self) -> bool {
        match self.phase.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => { self.changed.notify_waiters(); true },
            Err(1) => true,
            Err(_) => false,
        }
    }
    pub(crate) fn begin_publish(&self) -> Result<()> {
        self.phase.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("The report was cancelled before publication"))?;
        Ok(())
    }
    pub(crate) fn finish_publish(&self) { self.phase.store(3, Ordering::Release); }
    pub(crate) async fn cancelled(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.phase.load(Ordering::Acquire) == 1 { return; }
            changed.await;
        }
    }
}

pub(crate) fn check_cancelled(cancellation: &Option<Arc<ScanCancellation>>) -> Result<()> {
    ensure!(!cancellation.as_ref().is_some_and(|flag| flag.phase.load(Ordering::Acquire) == 1),
        "The matching-document scan was cancelled");
    Ok(())
}



#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReportFormat { Csv, Json }

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReportSort { Filename, Title, YearAsc, YearDesc, Location }
impl ReportSort {
    pub(crate) fn sql(self) -> &'static str {
        match self {
            Self::Filename => "sort_filename COLLATE NOCASE ASC, owner ASC, doc_id ASC",
            Self::Title => "sort_title COLLATE NOCASE ASC, owner ASC, doc_id ASC",
            Self::YearAsc => "sort_year IS NULL ASC, sort_year ASC, owner ASC, doc_id ASC",
            Self::YearDesc => "sort_year IS NULL ASC, sort_year DESC, owner ASC, doc_id ASC",
            Self::Location => "sort_location COLLATE NOCASE ASC, owner ASC, doc_id ASC",
        }
    }

    /// SQLite NOCASE folds ASCII only. Use the same ordering when checking a
    /// remote page boundary rather than an OS locale or Unicode collation.
    pub(crate) fn compare(self, left: &ReportDocument, right: &ReportDocument) -> CompareOrder {
        let text = |left: &str, right: &str| left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase());
        let year = |descending: bool| match (left.year, right.year) {
            (Some(left), Some(right)) => if descending { right.cmp(&left) } else { left.cmp(&right) },
            (Some(_), None) => CompareOrder::Less,
            (None, Some(_)) => CompareOrder::Greater,
            (None, None) => CompareOrder::Equal,
        };
        let primary = match self {
            Self::Filename => text(left.filename.as_deref().unwrap_or_default(), right.filename.as_deref().unwrap_or_default()),
            Self::Title => text(left.title.as_deref().or(left.filename.as_deref()).unwrap_or_default(),
                right.title.as_deref().or(right.filename.as_deref()).unwrap_or_default()),
            Self::YearAsc => year(false), Self::YearDesc => year(true),
            Self::Location => text(&left.location_uri, &right.location_uri),
        };
        primary.then_with(|| left.owner_id.cmp(&right.owner_id)).then_with(|| left.doc_id.cmp(&right.doc_id))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportReceipt {
    pub path: PathBuf,
    pub documents: u64,
    pub scope: &'static str,
    pub generation: String,
    pub format: ReportFormat,
    pub sort: ReportSort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportDocument {
    pub doc_id: String,
    pub owner_id: String,
    #[serde(rename = "representativeChunkIndex")]
    pub chunk_index: i32,
    pub title: Option<String>,
    pub filename: Option<String>,
    pub author: Option<String>,
    pub location_uri: String,
    pub url: Option<String>,
    pub source_hash: Option<String>,
    pub indexed_at: Option<i64>,
    pub extension: Option<String>,
    pub language: Option<String>,
    pub year: Option<i32>,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_member: Option<crisp_index_protocol::ContainerMemberLocator>,
}

pub struct ReportMetadata {
    pub scope: &'static str,
    pub query: String,
    pub effective_query: String,
    pub filters: serde_json::Value,
    pub generation: String,
    pub matching_documents: u64,
    pub sort: ReportSort,
}

pub struct ReportStream {
    metadata: ReportMetadata,
    format: ReportFormat,
    filters_json: String,
    sort_name: String,
    written: u64,
}
impl ReportStream {
    pub fn start(output: &mut impl Write, metadata: ReportMetadata, format: ReportFormat) -> Result<Self> {
        ensure!(matches!(metadata.scope, "local_indexed_documents" | "remote_indexed_documents")
            && !metadata.generation.is_empty(), "The complete report has no valid source snapshot");
        let header = serde_json::json!({ "schema": "CrispSorter.lexical-report", "version": 1,
            "scope": metadata.scope, "complete": true, "matchingDocuments": metadata.matching_documents,
            "query": metadata.query, "effectiveQuery": metadata.effective_query, "filters": metadata.filters,
            "generation": metadata.generation, "sort": metadata.sort,
            "ordering": "global metadata order; owner and document ID break ties; unknown years follow known years",
            "representative": "lowest matching stored chunk; tags are the per-document union of matching stored rows",
            "excludedChannels": ["catalog_filename_references", "semantic_similarity"],
            "exportedAtUnixMs": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis(),
        });
        let filters_json = serde_json::to_string(&metadata.filters)?;
        let sort_name = serde_json::to_string(&metadata.sort)?.trim_matches('"').to_owned();
        if format == ReportFormat::Json {
            let mut prefix = serde_json::to_vec(&header)?; prefix.pop();
            output.write_all(&prefix)?; output.write_all(b",\"documents\":[")?;
        } else {
            output.write_all(b"record_type,owner_id,doc_id,title,filename,author,year,extension,language,location_uri,source_url,source_hash,indexed_at,representative_chunk_index,tags_json,query,effective_query,scope,generation,total_documents,sort,filters_json,container_root_uri,container_path,container_member_json\r\n")?;
            let mut values = vec!["report".to_owned()]; values.extend(vec![String::new(); 14]);
            values.extend([metadata.query.clone(), metadata.effective_query.clone(), metadata.scope.into(), metadata.generation.clone(),
                metadata.matching_documents.to_string(), sort_name.clone(), filters_json.clone()]);
            values.extend([String::new(),String::new(),String::new()]);
            write_csv_row(output, &values)?;
        }
        Ok(Self { metadata, format, filters_json, sort_name, written: 0 })
    }

    pub fn push(&mut self, output: &mut impl Write, document: &ReportDocument) -> Result<()> {
        ensure!(self.written < self.metadata.matching_documents, "The complete report contains more documents than its snapshot total");
        if let Some(member) = &document.container_member {
            member.validate_identity(&document.owner_id,&document.doc_id,&document.location_uri).map_err(anyhow::Error::msg)?;
            ensure!(member.entries.last().is_some_and(|entry| document.source_hash.as_ref().is_some_and(|hash| entry.sha256.eq_ignore_ascii_case(hash))),
                "The report member fingerprint differs from its source locator");
        } else { ensure!(!document.location_uri.starts_with("crisp+member:"), "The report member is missing its typed source locator"); }
        if self.format == ReportFormat::Json {
            if self.written > 0 { output.write_all(b",")?; }
            serde_json::to_writer(output, document)?;
        } else {
            let values = vec!["document".into(), document.owner_id.clone(), document.doc_id.clone(), document.title.clone().unwrap_or_default(),
                document.filename.clone().unwrap_or_default(), document.author.clone().unwrap_or_default(),
                document.year.map(|value| value.to_string()).unwrap_or_default(), document.extension.clone().unwrap_or_default(),
                document.language.clone().unwrap_or_default(), document.location_uri.clone(), document.url.clone().unwrap_or_default(),
                document.source_hash.clone().unwrap_or_default(), document.indexed_at.map(|value| value.to_string()).unwrap_or_default(),
                document.chunk_index.to_string(), serde_json::to_string(&document.tags)?, self.metadata.query.clone(), self.metadata.effective_query.clone(),
                self.metadata.scope.into(), self.metadata.generation.clone(), self.metadata.matching_documents.to_string(), self.sort_name.clone(), self.filters_json.clone(),
                document.container_member.as_ref().map(|member| member.root_location_uri.clone()).unwrap_or_default(),
                document.container_member.as_ref().map(member_breadcrumb).unwrap_or_default(),
                document.container_member.as_ref().map(serde_json::to_string).transpose()?.unwrap_or_default()];
            write_csv_row(output, &values)?;
        }
        self.written += 1;
        Ok(())
    }

    pub fn finish(self, output: &mut impl Write) -> Result<u64> {
        ensure!(self.written == self.metadata.matching_documents, "The complete report row count does not match its snapshot total");
        if self.format == ReportFormat::Json { output.write_all(b"]}\n")?; }
        output.flush().context("Flushing the complete matching-document report")?;
        Ok(self.written)
    }
}

fn member_breadcrumb(member: &crisp_index_protocol::ContainerMemberLocator) -> String {
    std::iter::once(member.root_location_uri.clone()).chain(member.entries.iter().map(|entry| {
        if entry.occurrence > 0 { format!("{} (occurrence {})",entry.name,entry.occurrence+1) } else { entry.name.clone() }
    })).collect::<Vec<_>>().join(" › ")
}

fn write_csv_row(output: &mut impl Write, values: &[String]) -> Result<()> {
    output.write_all(values.iter().map(|value| csv_cell(value)).collect::<Vec<_>>().join(",").as_bytes())?;
    output.write_all(b"\r\n")?;
    Ok(())
}

pub(crate) fn csv_cell(value: &str) -> String {
    let prefix = if value.trim_start().starts_with(['=', '+', '-', '@']) || value.starts_with(['\t', '\r', '\n']) { "'" } else { "" };
    format!("\"{prefix}{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod member_report_tests {
    use super::*;
    use crisp_index_protocol::{ContainerEntry, ContainerEntryKind, ContainerMemberLocator};

    fn document() -> ReportDocument {
        let locator = ContainerMemberLocator { version:1,root_owner_id:"reader".into(),root_doc_id:"root".into(),
            root_location_uri:"/mail/source.zip".into(),root_sha256:"a".repeat(64),entries:vec![
                ContainerEntry {kind:ContainerEntryKind::Zip,index:2,occurrence:1,name:"letters/receipt.txt".into(),sha256:"b".repeat(64),size:20}] };
        serde_json::from_value(serde_json::json!({"docId":locator.member_id().unwrap(),"ownerId":"reader","representativeChunkIndex":2,
            "locationUri":locator.member_uri().unwrap(),"sourceHash":"b".repeat(64),"tags":[],"containerMember":locator})).unwrap()
    }
    fn metadata() -> ReportMetadata {
        ReportMetadata {scope:"local_indexed_documents",query:"receipt".into(),effective_query:"receipt".into(),
            filters:serde_json::json!({}),generation:"snapshot".into(),matching_documents:1,sort:ReportSort::Filename}
    }
    #[test]
    fn complete_reports_keep_readable_member_paths_and_structured_byte_receipts() {
        let doc=document();
        for format in [ReportFormat::Json,ReportFormat::Csv] {
            let mut output=Vec::new();let mut writer=ReportStream::start(&mut output,metadata(),format).unwrap();
            writer.push(&mut output,&doc).unwrap();writer.finish(&mut output).unwrap();
            let text=String::from_utf8(output).unwrap();
            if format==ReportFormat::Json {
                let value:serde_json::Value=serde_json::from_str(&text).unwrap();
                assert_eq!(value["documents"][0]["containerMember"]["entries"][0]["occurrence"],1);
                assert_eq!(value["documents"][0]["containerMember"]["rootSha256"],"a".repeat(64));
            } else {
                assert!(text.contains("container_root_uri,container_path,container_member_json"));
                assert!(text.contains("/mail/source.zip › letters/receipt.txt (occurrence 2)"));
                assert!(text.contains("rootSha256"));
            }
        }
    }
    #[test]
    fn complete_reports_refuse_substituted_or_missing_member_metadata() {
        for invalid in 0..3 {
            let mut doc=document();
            match invalid {0=>doc.owner_id="other".into(),1=>doc.container_member=None,_=>doc.source_hash=Some("c".repeat(64))}
            let mut output=Vec::new();let mut writer=ReportStream::start(&mut output,metadata(),ReportFormat::Json).unwrap();
            assert!(writer.push(&mut output,&doc).is_err());
        }
    }
}
