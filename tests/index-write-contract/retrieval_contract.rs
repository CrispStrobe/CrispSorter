//! Regression tests for real document hydration, independent of embedding models.
use crate::index::{ingest::chunk_row_id, schema::DocumentChunk, LocalIndex};
use std::collections::HashMap;

#[tokio::test]
async fn natural_hybrid_keeps_semantic_candidates_filters_and_separate_cached_intents() {
    use crate::index::{FtsIndex, search::SearchEngine, schema::SearchFilters};
    use std::sync::Arc;
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalIndex::open_or_create(directory.path(), 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&directory.path().join("fts")).unwrap());
    let mut rows = Vec::new();
    for (id, body, owner, ext, embedding) in [
        ("lexical", "alpha contract", "owner", "pdf", vec![0.0, 1.0, 0.0, 0.0]),
        ("semantic", "related topic", "owner", "pdf", vec![1.0, 0.0, 0.0, 0.0]),
        ("other-owner", "alpha contract", "elsewhere", "pdf", vec![1.0, 0.0, 0.0, 0.0]),
        ("other-format", "alpha contract", "owner", "txt", vec![1.0, 0.0, 0.0, 0.0]),
    ] {
        let mut row = chunk(id, 0, 1, body);
        row.owner_id = owner.into(); row.ext = Some(ext.into()); row.embedding = Some(embedding);
        rows.push(row);
    }
    local.ingest_batch(&rows).await.unwrap();
    let engine = SearchEngine::new(fts.clone(), local.clone(),
        Some(Arc::new(tokio::sync::Mutex::new(crate::index::embedder::Embedder))));
    let advanced = SearchFilters { owner_id: Some("owner".into()), ext: vec!["pdf".into()], ..Default::default() };
    let natural = SearchFilters { query_intent: Some("natural".into()), ..advanced.clone() };
    let raw = "alpha AND warranty";
    assert!(engine.search_hybrid(raw, &advanced, 10).await.unwrap().is_empty());
    let hits = engine.search_hybrid(raw, &natural, 10).await.unwrap();
    let mut ids: Vec<_> = hits.iter().map(|hit| hit.doc_id.as_str()).collect(); ids.sort();
    assert_eq!(ids, ["lexical", "semantic"]);
    assert!(engine.search_hybrid(raw, &advanced, 10).await.unwrap().is_empty(), "intent changes cannot reuse a cached answer");
    assert_eq!(engine.search_hybrid("body:alpha", &advanced, 10).await.unwrap()[0].doc_id, "lexical");
    let text = engine.search_text("alpha: AND warranty?", &natural, 10).await.unwrap();
    assert_eq!(text.len(), 1); assert_eq!(text[0].doc_id, "lexical");
    let summary = crate::index::search_summary::summarize(local, fts,
        "alpha: AND warranty?".into(), natural, None).await.unwrap();
    assert_eq!(summary.matching_documents, 1);
    assert_eq!(summary.query, "alpha: AND warranty?");
    assert_eq!(summary.effective_query, "alpha OR \"and\" OR warranty");
}

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

#[tokio::test]
async fn exported_archive_searches_late_passages_and_preserves_source_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let index = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
    let mut chunks = Vec::new();
    for (number, start, body) in [(0, 0, "alpha beta "), (1, 6, "beta gamma "), (2, 11, "gamma archivalmarker")] {
        let mut row = chunk("book", number, 3, body);
        row.chunk_start_char = Some(start); row.chunk_end_char = Some(start + body.len() as i32);
        row.volume_id = Some("selected".into());
        row.headings_text = Some(if number == 2 { "Ending section" } else { "Opening" }.into());
        row.text_translated = Some("translatedarchiveword".into()); row.text_translated_lang = Some("en".into());
        chunks.push(row);
    }
    for (number, body) in [(0, "red"), (1, "fox")] {
        let mut row = chunk("separate", number, 2, body);
        row.volume_id = Some("selected".into()); chunks.push(row);
    }
    let mut other = chunk("other-volume", 0, 1, "excludedarchivemarker");
    other.volume_id = Some("other".into()); chunks.push(other);
    index.ingest_batch(&chunks).await.unwrap();
    let destination = dir.path().join("volume.cidx");
    assert_eq!(index.export_cidx(&destination, Some("selected"), false, true).await.unwrap(), 5);
    let fts = crate::index::FtsIndex::open_or_create(&destination.join("fts")).unwrap();
    let filters = crate::index::schema::SearchFilters::default();
    for query in ["archivalmarker", "\"alpha beta gamma\"", "headings:ending", "translated:translatedarchiveword"] {
        let hits = fts.search(query, &filters, 20).unwrap();
        assert_eq!(hits.len(), 1, "{query}"); assert_eq!(hits[0].doc_id, "book");
    }
    assert_eq!(fts.search("red AND fox", &filters, 20).unwrap().len(), 1);
    for query in ["\"red fox\"", "red w/1024 fox", "excludedarchivemarker"] {
        assert!(fts.search(query, &filters, 20).unwrap().is_empty(), "{query}");
    }
    let archive = LocalIndex::open_cidx(&destination).await.unwrap();
    let results = archive.fetch_best_chunk_per_doc(&["book".into()], &["archivalmarker"], &HashMap::new()).await.unwrap();
    assert_eq!(results[0].chunk_index, 2);
    assert!(results[0].snippet.contains("archivalmarker"));
}

#[tokio::test]
async fn legacy_owner_collisions_remain_isolated_through_real_search_and_reranking() {
    use crate::index::{fts_index::{FtsIndex, TantivyInput}, search::SearchEngine, schema::SearchFilters};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let local = Arc::new(LocalIndex::open_or_create(dir.path(), 4).await.unwrap());
    let fts = Arc::new(FtsIndex::open_or_create(&dir.path().join("fts")).unwrap());
    let mut first = chunk("shared", 3, 9, "alpha public passage");
    first.owner_id = "owner'a".into(); first.location_uri = "file://a/shared".into(); first.ext = Some("pdf".into());
    first.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
    first.embedding_sparse = Some(r#"{"indices":[1],"values":[0.25]}"#.into());
    first.multivec_packed = Some([0.25_f32, 0.0].iter().flat_map(|value| value.to_le_bytes()).collect());
    first.multivec_n_tokens = Some(1);
    let mut second = chunk("shared", 3, 9, "alpha beta private other owner");
    second.owner_id = "owner-b".into(); second.location_uri = "file://b/shared".into(); second.ext = Some("txt".into());
    second.embedding = Some(vec![0.8, 0.2, 0.0, 0.0]);
    second.embedding_sparse = Some(r#"{"indices":[1],"values":[0.9]}"#.into());
    second.multivec_packed = Some([0.9_f32, 0.0].iter().flat_map(|value| value.to_le_bytes()).collect());
    second.multivec_n_tokens = Some(1);
    // Model legacy data with equal document and hashed chunk IDs. New writes
    // reject this ambiguous identity, but reads must still isolate owners.
    local.append_legacy_rows_for_test(&[first.clone(), second.clone()]).await.unwrap();
    let mut writer = fts.writer().unwrap();
    for row in [&first, &second] {
        fts.add_document(&mut writer, TantivyInput { doc_id: &row.doc_id, owner_id: &row.owner_id,
            language: "en", title: "", headings: "", body: row.full_text.as_deref().unwrap(), body_translated: None }).unwrap();
    }
    writer.commit().unwrap(); drop(writer);
    let engine = SearchEngine::new(fts.clone(), local.clone(),
        Some(Arc::new(tokio::sync::Mutex::new(crate::index::embedder::Embedder))));
    let filters = SearchFilters { owner_id: Some(first.owner_id.clone()), ext: vec!["pdf".into()], ..Default::default() };
    let scoped = engine.search_text("alpha OR beta", &filters, 10).await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].owner_id, first.owner_id);
    assert_eq!(scoped[0].chunk_index, 3, "sparse stored chunks remain eligible");
    assert_eq!(scoped[0].snippet, "alpha public passage");
    let all = engine.search_text("alpha OR beta", &Default::default(), 10).await.unwrap();
    assert_eq!(all.len(), 2, "equal document IDs are distinct owner identities");
    let pdf = SearchFilters { ext: vec!["pdf".into()], ..Default::default() };
    assert!(engine.search_text("body:beta", &pdf, 10).await.unwrap().is_empty(), "another owner cannot lend its lexical match to a matching PDF row");
    let strict = engine.search_hybrid("body:beta", &Default::default(), 10).await.unwrap();
    assert_eq!(strict.len(), 1);
    assert_eq!(strict[0].owner_id, second.owner_id, "the higher ANN score cannot bypass an exact-owner lexical condition");
    let hybrid = engine.search_hybrid("alpha OR beta", &Default::default(), 10).await.unwrap();
    assert_eq!(hybrid.len(), 2, "fusion cannot collapse equal document IDs across owners");

    let passages = local.fetch_passages(&all).await.unwrap();
    let first_key = crate::index::local_index::passage_key(&first.owner_id, "shared", 3);
    let second_key = crate::index::local_index::passage_key(&second.owner_id, "shared", 3);
    assert_eq!(passages[&first_key].text, "alpha public passage");
    assert_eq!(passages[&second_key].text, "alpha beta private other owner");
    let one = local.fetch_passages(&scoped).await.unwrap();
    assert_eq!(one.len(), 1); assert!(!one.contains_key(&second_key));
    let reranked = local.rerank_with_colbert(scoped.clone(), &[vec![1.0, 0.0]], 10).await.unwrap();
    assert!((reranked[0].score - 0.25).abs() < 1e-6);
    let both = local.rerank_with_colbert(all, &[vec![1.0, 0.0]], 10).await.unwrap();
    assert_eq!(both.len(), 2); assert_eq!(both[0].owner_id, second.owner_id);
    let sparse = crate::index::embedder::SparseVector { indices: vec![1], values: vec![1.0] };
    let sparse_hits = local.search_sparse_in_pool(&sparse, &[(first.owner_id.clone(), "shared".into())], &Default::default(), 10).await.unwrap();
    assert_eq!(sparse_hits.len(), 1); assert_eq!(sparse_hits[0].owner_id, first.owner_id);
    assert!((sparse_hits[0].score - 0.25).abs() < 1e-6);
    assert!(local.fetch_best_chunk_per_doc(&["shared".into()], &["alpha", "beta"], &HashMap::new()).await.is_err());
}
