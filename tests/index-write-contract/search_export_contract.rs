//! Complete reports use real retrieval, bounded staging and atomic publication.
use std::{fs, path::PathBuf, sync::Arc};
use crate::index::{ingest::chunk_row_id, schema::{DocumentChunk, SearchFilters}, FtsIndex, LocalIndex,
    search_summary::{self, ExportOptions, ReportFormat, ReportSort, ScanCancellation}};

fn chunk(id: &str, text: &str) -> DocumentChunk {
    serde_json::from_value(serde_json::json!({
        "id": chunk_row_id(id, 0), "doc_id": id, "owner_id": "reader",
        "location_uri": format!("/library/{id}.txt"), "filename": format!("{id}.txt"), "title": id,
        "full_text": text, "chunk_index": 0, "chunk_total": 1,
        "source_hash": format!("hash-{id}"), "indexed_at": 1, "tags": ["reviewed", "reviewed"],
        "ext": "txt", "language": "en", "year": 2026,
    })).unwrap()
}

async fn indexes(root: &tempfile::TempDir) -> (Arc<LocalIndex>, Arc<FtsIndex>) {
    let directory = root.path().join("index");
    let local = Arc::new(LocalIndex::open_or_create(&directory, 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&directory.join("fts")).unwrap());
    (local, fts)
}

fn options(path: PathBuf, format: ReportFormat, sort: ReportSort) -> ExportOptions {
    ExportOptions { path, format, sort, expected_generation: None, protected_roots: vec![] }
}

#[tokio::test]
async fn complete_json_streams_more_than_the_interactive_limit_in_global_metadata_order() {
    let root = tempfile::tempdir().unwrap(); let (local, fts) = indexes(&root).await;
    let rows: Vec<_> = (0..633).rev().map(|number| chunk(&format!("doc-{number:04}"), "needle")).collect();
    local.ingest_batch(&rows).await.unwrap();
    let path = root.path().join("complete.json"); fs::write(&path, b"old report").unwrap();
    let prepared = search_summary::prepare_export(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(),
        options(path.clone(), ReportFormat::Json, ReportSort::Filename), None).await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"old report", "Preparing a complete file must not truncate the old report");
    let receipt = prepared.publish().await.unwrap();
    assert_eq!(receipt.documents, 633);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(value["matchingDocuments"], 633); assert_eq!(value["complete"], true);
    assert_eq!(value["scope"], "local_indexed_documents"); assert_eq!(value["sort"], "filename");
    let documents = value["documents"].as_array().unwrap(); assert_eq!(documents.len(), 633);
    assert_eq!(documents[0]["docId"], "doc-0000"); assert_eq!(documents[632]["docId"], "doc-0632");
    assert!(documents.iter().all(|row| row["tags"] == serde_json::json!(["reviewed"])));
    assert!(documents.iter().all(|row| row["ownerId"] == "reader"));
    assert!(value.get("relevanceOrder").is_none());
}

#[tokio::test]
async fn metadata_sort_has_deterministic_unknown_years_and_csv_escapes_literal_values() {
    let root = tempfile::tempdir().unwrap(); let (local, fts) = indexes(&root).await;
    let mut early = chunk("early", "needle"); early.year = Some(2000);
    early.title = Some("=SUM(1,2)\n\"quoted\"".into());
    let mut late = chunk("late", "needle"); late.year = Some(2026);
    let mut unknown = chunk("unknown", "needle"); unknown.year = None;
    local.ingest_batch(&[unknown, late, early]).await.unwrap();
    for (sort, expected) in [(ReportSort::YearAsc, vec!["early", "late", "unknown"]),
        (ReportSort::YearDesc, vec!["late", "early", "unknown"])] {
        let path = root.path().join("years.json");
        search_summary::prepare_export(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(),
            options(path.clone(), ReportFormat::Json, sort), None).await.unwrap().publish().await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(value["documents"].as_array().unwrap().iter().map(|row| row["docId"].as_str().unwrap()).collect::<Vec<_>>(), expected);
    }
    let path = root.path().join("literal.csv");
    let result = search_summary::prepare_export(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(),
        options(path.clone(), ReportFormat::Csv, ReportSort::Filename), None).await.unwrap().publish().await.unwrap();
    assert_eq!(result.documents, 3);
    let csv = fs::read_to_string(path).unwrap();
    assert!(csv.starts_with("record_type,owner_id,doc_id,title,"));
    assert!(csv.contains("\"'=SUM(1,2)\n\"\"quoted\"\"\""));
    assert!(csv.contains("local_indexed_documents"));
    assert!(csv.contains("\"report\",\"\""));
    let empty_path = root.path().join("zero.json");
    let empty = search_summary::prepare_export(local.clone(), fts.clone(), "absent".into(), SearchFilters::default(),
        options(empty_path.clone(), ReportFormat::Json, ReportSort::Title), None).await.unwrap().publish().await.unwrap();
    assert_eq!(empty.documents, 0);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(empty_path).unwrap()).unwrap();
    assert_eq!(value["documents"], serde_json::json!([])); assert_eq!(value["matchingDocuments"], 0);
    let zero_csv = root.path().join("zero.csv");
    let empty = search_summary::prepare_export(local, fts, "absent".into(), SearchFilters::default(),
        options(zero_csv.clone(), ReportFormat::Csv, ReportSort::Title), None).await.unwrap().publish().await.unwrap();
    let csv = fs::read_to_string(zero_csv).unwrap();
    assert_eq!(csv.lines().count(), 2);
    assert!(csv.contains("\"absent\",\"absent\",\"local_indexed_documents\""));
    assert!(csv.contains(&format!("\"{}\",\"0\",\"title\"", empty.generation)));
}

#[tokio::test]
async fn cancellation_changed_generation_and_destination_changes_preserve_existing_reports() {
    let root = tempfile::tempdir().unwrap(); let (local, fts) = indexes(&root).await;
    local.ingest_batch(&[chunk("a", "needle")]).await.unwrap();
    let path = root.path().join("report.json"); fs::write(&path, b"previous complete report").unwrap();
    let cancellation = Arc::new(ScanCancellation::default());
    let prepared = search_summary::prepare_export(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(),
        options(path.clone(), ReportFormat::Json, ReportSort::Filename), Some(cancellation.clone())).await.unwrap();
    cancellation.cancel(); assert!(prepared.publish().await.unwrap_err().to_string().contains("cancelled"));
    assert_eq!(fs::read(&path).unwrap(), b"previous complete report");

    let prepared = search_summary::prepare_export(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(),
        options(path.clone(), ReportFormat::Json, ReportSort::Filename), None).await.unwrap();
    let failed_scope = prepared.publish_with_validation(|| anyhow::bail!("Mounted sources changed before publication")).await.unwrap_err();
    assert!(failed_scope.to_string().contains("Mounted sources changed"));
    assert_eq!(fs::read(&path).unwrap(), b"previous complete report");

    let prepared = search_summary::prepare_export(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(),
        options(path.clone(), ReportFormat::Json, ReportSort::Filename), None).await.unwrap();
    // Deliberately bypass the application lease to exercise the final guard
    // against another process or a legacy writer changing full-text storage.
    let mut writer = fts.writer().unwrap();
    fts.delete_document(&mut writer, "a").unwrap(); writer.commit().unwrap();
    assert!(prepared.publish().await.unwrap_err().to_string().contains("changed during export"));
    assert_eq!(fs::read(&path).unwrap(), b"previous complete report");

    let mut stale = options(path.clone(), ReportFormat::Json, ReportSort::Filename);
    stale.expected_generation = Some("obsolete snapshot".into());
    assert!(search_summary::prepare_export(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(), stale, None).await.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"previous complete report");
    let prepared = search_summary::prepare_export(local, fts, "needle".into(), SearchFilters::default(),
        options(path.clone(), ReportFormat::Json, ReportSort::Filename), None).await.unwrap();
    fs::write(&path, b"another writer's complete report").unwrap();
    assert!(prepared.publish().await.unwrap_err().to_string().contains("destination changed"));
    assert_eq!(fs::read(&path).unwrap(), b"another writer's complete report");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2, "All temporary report files are removed");
}

#[tokio::test]
async fn complete_export_rejects_nonmatching_sources_and_index_storage_as_destinations() {
    let root = tempfile::tempdir().unwrap(); let (local, fts) = indexes(&root).await;
    let source = root.path().join("original.txt"); fs::write(&source, b"original document").unwrap();
    let mut row = chunk("nonmatch", "unrelated content"); row.location_uri = format!("file://{}", source.display());
    local.ingest_batch(&[row, chunk("match", "needle")]).await.unwrap();
    let error = search_summary::prepare_export(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(),
        options(source.clone(), ReportFormat::Json, ReportSort::Filename), None).await.err().unwrap();
    assert!(error.to_string().contains("cannot replace an indexed source"));
    assert_eq!(fs::read(&source).unwrap(), b"original document");
    let protected = root.path().join("index").join("report.json");
    let error = search_summary::prepare_export(local, fts, "needle".into(), SearchFilters::default(),
        options(protected.clone(), ReportFormat::Json, ReportSort::Filename), None).await.err().unwrap();
    assert!(error.to_string().contains("outside application and index storage"));
    assert!(!protected.exists());
}

#[tokio::test]
async fn complete_export_preserves_member_provenance_without_duplicate_parent_text() {
    use crisp_index_protocol::{ContainerEntry,ContainerEntryKind,ContainerMemberLocator};
    let root=tempfile::tempdir().unwrap();let (local,fts)=indexes(&root).await;
    let mut parent=chunk("container-root","Archive coverage summary");parent.source_hash="a".repeat(64);
    parent.location_uri=root.path().join("mail.zip").to_string_lossy().into_owned();
    let locator=ContainerMemberLocator {version:1,root_owner_id:parent.owner_id.clone(),root_doc_id:parent.doc_id.clone(),
        root_location_uri:parent.location_uri.clone(),root_sha256:parent.source_hash.clone(),entries:vec![
            ContainerEntry {kind:ContainerEntryKind::Zip,index:2,occurrence:0,name:"notes/receipt.txt".into(),sha256:"b".repeat(64),size:20}]};
    let id=locator.member_id().unwrap();let mut child=chunk(&id,"needle member body");
    child.location_uri=locator.member_uri().unwrap();child.source_hash="b".repeat(64);
    child.metadata_json=Some(serde_json::json!({"container_member":locator}).to_string());
    local.ingest_batch(&[parent,child]).await.unwrap();
    let path=root.path().join("members.json");
    let receipt=search_summary::prepare_export(local,fts,"needle".into(),SearchFilters::default(),
        options(path.clone(),ReportFormat::Json,ReportSort::Filename),None).await.unwrap().publish().await.unwrap();
    assert_eq!(receipt.documents,1);
    let value:serde_json::Value=serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(value["documents"][0]["containerMember"]["rootLocationUri"],locator.root_location_uri);
    assert_eq!(value["documents"][0]["containerMember"]["entries"][0]["name"],"notes/receipt.txt");
    assert_eq!(value["documents"][0]["sourceHash"],"b".repeat(64));
}
