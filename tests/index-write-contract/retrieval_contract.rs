//! Regression tests for real document hydration, independent of embedding models.
use crate::index::{ingest::chunk_row_id, schema::DocumentChunk, LocalIndex};
use std::collections::HashMap;

fn chunk(id: &str, index: i32, total: i32, text: &str) -> DocumentChunk {
    serde_json::from_value(serde_json::json!({
        "id": chunk_row_id(id, index), "doc_id": id,
        "location_uri": format!("crisp+local://machine/owner/{id}.txt"),
        "owner_id": "owner", "filename": format!("{id}.txt"), "title": id,
        "full_text": text, "chunk_index": index, "chunk_total": total,
        "source_hash": id, "indexed_at": 1, "tags": [],
    })).unwrap()
}

#[tokio::test]
async fn long_document_cannot_hide_later_ranked_documents_or_its_late_match() {
    let dir = tempfile::tempdir().unwrap();
    let index = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
    // Forty chunks exceed the previous shared 3 * 8 row allowance.
    let mut chunks: Vec<_> = (0..40).map(|n| chunk("long", n, 40,
        if n == 39 { "needle late passage" } else { "unrelated introduction" })).collect();
    chunks.push(chunk("short", 0, 1, "needle short passage"));
    chunks.push(chunk("metadata", -1, 0, ""));
    index.ingest_batch(&chunks).await.unwrap();
    let ids = vec!["short".into(), "long".into(), "metadata".into()];
    let scores = HashMap::from([("short".into(), 3.0), ("long".into(), 2.0), ("metadata".into(), 1.0)]);
    let hits = index.fetch_best_chunk_per_doc(&ids, &["needle"], &scores).await.unwrap();
    assert_eq!(hits.iter().map(|r| r.doc_id.as_str()).collect::<Vec<_>>(), ["short", "long", "metadata"]);
    assert_eq!(hits.iter().map(|r| r.score).collect::<Vec<_>>(), [3.0, 2.0, 1.0]);
    assert_eq!(hits[1].chunk_index, 39);
    assert!(hits[1].snippet.contains("late passage"));
    assert_eq!(hits[2].chunk_index, -1);
}

#[tokio::test]
async fn hydration_ties_are_stable_and_an_empty_term_set_keeps_metadata_documents() {
    let dir = tempfile::tempdir().unwrap();
    let index = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
    // Deliberately insert the later chunk first; storage ordering is not rank.
    index.ingest_batch(&[
        chunk("document", 1, 2, "needle later"),
        chunk("document", 0, 2, "needle first"),
        chunk("metadata", -1, 0, ""),
    ]).await.unwrap();
    let ids = vec!["metadata".into(), "document".into(), "document".into()];
    for terms in [vec![], vec!["needle"]] {
        let hits = index.fetch_best_chunk_per_doc(&ids, &terms, &HashMap::new()).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc_id, "metadata");
        assert_eq!(hits[1].doc_id, "document");
        assert_eq!(hits[1].chunk_index, 0);
    }
    assert!(index.fetch_best_chunk_per_doc(&[], &["needle"], &HashMap::new()).await.unwrap().is_empty());
}

#[tokio::test]
async fn semantic_candidates_count_documents_and_preserve_the_best_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let index = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
    let mut chunks: Vec<_> = (0..90).map(|n| {
        let mut row = chunk("long", n, 90, "many similar passages");
        row.embedding = Some(if n == 89 { vec![1.0, 0.0, 0.0, 0.0] } else { vec![0.99, 0.01, 0.0, 0.0] });
        row.ext = Some("pdf".into()); row
    }).collect();
    let mut short = chunk("short", 0, 1, "independent document");
    short.embedding = Some(vec![0.8, 0.2, 0.0, 0.0]); short.ext = Some("pdf".into());
    let mut excluded = chunk("excluded", 0, 1, "different file type");
    excluded.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]); excluded.ext = Some("txt".into());
    chunks.extend([short, excluded]);
    index.ingest_batch(&chunks).await.unwrap();
    let filters = crate::index::schema::SearchFilters { ext: vec!["pdf".into()], ..Default::default() };
    for hits in [
        index.search_vector(&[1.0, 0.0, 0.0, 0.0], &filters, 2).await.unwrap(),
        index.search_vector_column(&[1.0, 0.0, 0.0, 0.0], "embedding", &filters, 2).await.unwrap(),
    ] {
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].doc_id, "long");
        assert_eq!(hits[0].chunk_index, 89);
        assert_eq!(hits[1].doc_id, "short");
    }
    assert!(index.search_vector(&[1.0, 0.0, 0.0, 0.0], &filters, 0).await.unwrap().is_empty());
}

#[tokio::test]
async fn initial_passage_uses_positive_whole_tokens_with_the_index_analyzer() {
    let dir = tempfile::tempdir().unwrap();
    let index = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
    index.ingest_batch(&[
        chunk("unicode", 0, 2, "and not draft"),
        chunk("unicode", 1, 2, "café 東京"),
        chunk("boundary", 0, 2, "party partner"),
        chunk("boundary", 1, 2, "art"),
    ]).await.unwrap();
    for (id, query) in [("unicode", "cafe AND 東京 NOT draft"), ("boundary", "art")] {
        let terms = crate::index::fts_query::literal_body_terms(query).unwrap();
        let refs: Vec<_> = terms.iter().map(String::as_str).collect();
        let hits = index.fetch_best_chunk_per_doc(&[id.into()], &refs, &HashMap::new()).await.unwrap();
        assert_eq!(hits[0].chunk_index, 1);
    }
}
