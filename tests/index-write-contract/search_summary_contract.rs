//! Exact lexical totals use the production indexes and transaction guard.
use std::sync::Arc;
use crate::index::{ingest::chunk_row_id, schema::{DocumentChunk, SearchFilters}, search_summary::{self, SearchSummary}, FtsIndex, LocalIndex};

fn chunk(id: &str, text: &str) -> DocumentChunk {
    serde_json::from_value(serde_json::json!({
        "id": chunk_row_id(id, 0), "doc_id": id, "owner_id": "reader",
        "location_uri": format!("/library/{id}.txt"), "filename": format!("{id}.txt"), "title": id,
        "full_text": text, "chunk_index": 0, "chunk_total": 1,
        "source_hash": format!("aabb{id}"), "indexed_at": 1, "tags": ["reviewed", "reviewed"],
        "ext": "txt", "language": "en", "year": 2026, "parent_dir": "/library",
    })).unwrap()
}

async fn summary(local: &Arc<LocalIndex>, fts: &Arc<FtsIndex>, query: &str, filters: SearchFilters) -> SearchSummary {
    search_summary::summarize(local.clone(), fts.clone(), query.into(), filters, None).await.unwrap()
}

#[tokio::test]
async fn summary_counts_beyond_interactive_windows_and_reports_facet_truncation() {
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalIndex::open_or_create(directory.path(), 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&directory.path().join("fts")).unwrap());
    let mut rows: Vec<_> = (0..633).map(|number| {
        let mut row = chunk(&format!("document-{number:04}"), "complete needle result");
        row.tags.push(format!("topic-{:02}", number % 31)); row
    }).collect();
    // A second passage cannot increase the document or tag count.
    rows[0].chunk_total = 2;
    let mut second = rows[0].clone(); second.id = chunk_row_id(&second.doc_id, 1); second.chunk_index = 1;
    rows.push(second);
    local.ingest_batch(&rows).await.unwrap();
    let counted = summary(&local, &fts, "needle", SearchFilters::default()).await;
    assert_eq!(counted.matching_documents, 633);
    assert_eq!(counted.scanned_rows, 634);
    assert_eq!(counted.scope, "local_indexed_documents");
    assert_eq!(counted.facets.tags.values[0].value, "reviewed");
    assert_eq!(counted.facets.tags.values[0].documents, 633);
    assert_eq!(counted.facets.tags.distinct_values, 32);
    assert_eq!(counted.facets.tags.values.len(), 25);
    assert_eq!(counted.facets.tags.omitted_values, 7);
    assert_eq!(counted.facets.extensions.values[0].documents, 633);
    assert_eq!(counted.facets.years.missing_documents, 0);
}

#[tokio::test]
async fn grammar_owner_and_metadata_predicates_are_applied_before_counting() {
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalIndex::open_or_create(directory.path(), 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&directory.path().join("fts")).unwrap());
    let a = chunk("a", "red fox jumps beyond moon");
    let mut b = chunk("b", "red agile fox draft"); b.owner_id = "other".into(); b.ext = Some("pdf".into());
    let mut c = chunk("c", "silver moon and quiet fox"); c.language = Some("de".into()); c.year = Some(2020);
    let mut d = chunk("d", "red fox archived"); d.volume_id = Some("offline".into());
    local.ingest_batch(&[a, b, c, d]).await.unwrap();
    for (query, expected) in [("red AND NOT draft", 2), ("\"red fox\"", 2), ("red w/1 fox", 3),
        ("title:a", 1), ("arch*", 1), ("arhived~1", 1), ("NOT red", 1)] {
        assert_eq!(summary(&local, &fts, query, SearchFilters::default()).await.matching_documents, expected, "{query}");
    }
    let filters = SearchFilters { owner_id: Some("reader".into()), ext: vec!["txt".into()],
        year_min: Some(2026), language: Some("en".into()), tag: Some("reviewed".into()),
        parent_dir_prefix: Some("/library".into()), mounted_volume_ids: Some(vec![]),
        doc_id_scope: vec!["a".into(), "b".into(), "c".into(), "d".into()], ..Default::default() };
    assert_eq!(summary(&local, &fts, "fox", filters).await.matching_documents, 1);
    assert!(search_summary::summarize(local, fts, "red AND".into(), SearchFilters::default(), None).await.is_err());
}

#[tokio::test]
async fn duplicate_rows_and_equal_ids_in_other_owners_do_not_inflate_or_borrow_matches() {
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalIndex::open_or_create(directory.path(), 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&directory.path().join("fts")).unwrap());
    let mut original = chunk("shared", "needle true source"); original.tags = vec!["x".into(), "x".into()];
    local.ingest_batch(&[original.clone()]).await.unwrap();
    let mut duplicate = original.clone(); duplicate.id = "legacy duplicate row".into(); duplicate.tags.push("y".into());
    let mut other = original; other.id = "other owner row".into(); other.owner_id = "outsider".into(); other.tags = vec!["private".into()];
    local.append_legacy_rows_for_test(&[duplicate, other]).await.unwrap();
    let counted = summary(&local, &fts, "needle", SearchFilters::default()).await;
    assert_eq!(counted.matching_documents, 1);
    assert_eq!(counted.facets.tags.values.iter().map(|value| (value.value.as_str(), value.documents)).collect::<Vec<_>>(), [("x", 1), ("y", 1)]);
    assert_eq!(summary(&local, &fts, "needle", SearchFilters { owner_id: Some("outsider".into()), ..Default::default() }).await.matching_documents, 0);
}

#[tokio::test]
async fn missing_values_and_empty_queries_have_real_zero_counts() {
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalIndex::open_or_create(directory.path(), 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&directory.path().join("fts")).unwrap());
    let mut row = chunk("unknown", "needle"); row.ext = None; row.language = None; row.year = None; row.tags.clear();
    local.ingest_batch(&[row]).await.unwrap();
    let counted = summary(&local, &fts, "needle", SearchFilters::default()).await;
    assert_eq!(counted.facets.tags.missing_documents, 1);
    assert_eq!(counted.facets.years.missing_documents, 1);
    assert_eq!(counted.facets.languages.missing_documents, 1);
    let empty = summary(&local, &fts, "absent", SearchFilters::default()).await;
    assert_eq!(empty.matching_documents, 0);
    assert_eq!(empty.facets.tags.missing_documents, 0);
    assert_eq!(empty.facets.tags.distinct_values, 0);
    assert!(search_summary::summarize(local, fts, String::new(), SearchFilters::default(), None).await.is_err());
}

#[tokio::test]
async fn generation_changes_on_mutation_and_scan_takes_the_real_shared_guard() {
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalIndex::open_or_create(directory.path(), 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&directory.path().join("fts")).unwrap());
    local.ingest_batch(&[chunk("a", "needle")]).await.unwrap();
    let before = search_summary::generation(&local, &fts, None).await.unwrap();
    assert_ne!(before, search_summary::generation(&local, &fts, Some(&[])).await.unwrap());
    assert_eq!(search_summary::generation(&local, &fts, Some(&["a".into(), "b".into()])).await.unwrap(),
        search_summary::generation(&local, &fts, Some(&["b".into(), "a".into(), "a".into()])).await.unwrap());
    assert_ne!(search_summary::generation(&local, &fts, Some(&["a".into()])).await.unwrap(),
        search_summary::generation(&local, &fts, Some(&["b".into()])).await.unwrap());
    let guard = local.read_guard(&fts).await.unwrap();
    let (worker_local, worker_fts) = (local.clone(), fts.clone());
    let worker = tokio::spawn(async move { summary(&worker_local, &worker_fts, "needle", SearchFilters::default()).await });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert!(!worker.is_finished(), "A summary must use the application's read lease");
    drop(guard);
    assert_eq!(worker.await.unwrap().generation, before);
    local.delete_document_with_fts("a", &fts).await.unwrap();
    let after = search_summary::generation(&local, &fts, None).await.unwrap();
    assert_ne!(before, after);
    assert_eq!(summary(&local, &fts, "needle", SearchFilters::default()).await.matching_documents, 0);
    let cancellation = Arc::new(search_summary::ScanCancellation::default()); cancellation.cancel();
    let stopped = search_summary::summarize(local, fts, "needle".into(), SearchFilters::default(), Some(cancellation)).await.unwrap_err();
    assert!(stopped.to_string().contains("cancelled"));
}

#[tokio::test]
async fn sparse_stored_chunks_count_once_and_waiting_scans_cancel_without_the_read_lease() {
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalIndex::open_or_create(directory.path(), 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&directory.path().join("fts")).unwrap());
    let rows: Vec<_> = [9, 2, 5].into_iter().map(|index| {
        let mut row = chunk("sparse", "needle"); row.chunk_index = index; row.chunk_total = 20;
        row.id = chunk_row_id("sparse", index); row.year = Some(2000 + index); row
    }).collect();
    local.ingest_batch(&rows).await.unwrap();
    let counted = summary(&local, &fts, "needle", SearchFilters::default()).await;
    assert_eq!(counted.matching_documents, 1);
    assert_eq!(counted.scanned_rows, 3);
    assert_eq!(counted.facets.years.values[0].value, "2002");
    let guard = local.read_guard(&fts).await.unwrap();
    let cancellation = Arc::new(search_summary::ScanCancellation::default());
    let task = tokio::spawn(search_summary::summarize(local.clone(), fts.clone(), "needle".into(), SearchFilters::default(), Some(cancellation.clone())));
    tokio::task::yield_now().await;
    cancellation.cancel();
    let stopped = tokio::time::timeout(std::time::Duration::from_secs(1), task).await.unwrap().unwrap().unwrap_err();
    assert!(stopped.to_string().contains("cancelled"));
    drop(guard);
}
