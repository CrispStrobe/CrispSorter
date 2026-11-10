//! Exercise literal structured predicates against the actual query engine.
use crate::index::{ingest::chunk_row_id, schema::{DocumentChunk, SearchFilters}, LocalIndex};
use arrow_array::{Array, StringArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

fn chunk(id: &str, tags: &[&str], parent: &str, camera: &str, hash: &str) -> DocumentChunk {
    serde_json::from_value(serde_json::json!({
        "id": chunk_row_id(id, 0), "doc_id": id, "owner_id": "reader",
        "location_uri": format!("{parent}/{id}.txt"), "filename": format!("{id}.txt"), "title": id,
        "full_text": "needle", "chunk_index": 0, "chunk_total": 1,
        "source_hash": hash, "indexed_at": 1, "tags": tags,
        "parent_dir": parent, "image_camera_make": camera, "image_camera_model": camera,
    })).unwrap()
}

async fn ids(index: &LocalIndex, filters: SearchFilters) -> Vec<String> {
    let mut rows = index.table_ref().query().only_if(filters.to_lance_sql().unwrap())
        .select(Select::Columns(vec!["doc_id".into()])).execute().await.unwrap();
    let mut result = Vec::new();
    while let Some(batch) = rows.try_next().await.unwrap() {
        let values = batch.column_by_name("doc_id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
        result.extend(values.iter().flatten().map(str::to_owned));
    }
    result.sort(); result
}

#[tokio::test]
async fn all_required_tags_are_applied_together_and_quotes_are_literal() {
    let dir = tempfile::tempdir().unwrap();
    let index = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
    index.ingest_batch(&[
        chunk("all", &["reviewed", "research", "reader's"], "/docs", "Camera", "abcd"),
        chunk("partial", &["reviewed", "research"], "/docs", "Camera", "abcd"),
        chunk("one", &["reviewed"], "/docs", "Camera", "abcd"),
    ]).await.unwrap();
    assert_eq!(ids(&index, SearchFilters { tags: vec!["reviewed".into(), "research".into()],
        tag: Some("reader's".into()), ..Default::default() }).await, ["all"]);
}

#[tokio::test]
async fn wildcard_characters_in_filter_values_do_not_expand_matching_sources() {
    let dir = tempfile::tempdir().unwrap();
    let index = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
    index.ingest_batch(&[
        chunk("literal", &[], "/docs/100%_ready!", "Maker_100%!", "abcdef"),
        chunk("broad", &[], "/docs/100anythingXready!", "MakerX100anything!", "aaaaff"),
    ]).await.unwrap();
    for filters in [
        SearchFilters { parent_dir_prefix: Some("/docs/100%_ready!".into()), ..Default::default() },
        SearchFilters { image_camera_make: Some("Maker_100%!".into()), ..Default::default() },
        SearchFilters { image_camera_model: Some("Maker_100%!".into()), ..Default::default() },
        SearchFilters { source_hash_prefix: Some("ABCD".into()), ..Default::default() },
    ] { assert_eq!(ids(&index, filters).await, ["literal"]); }
    assert!(ids(&index, SearchFilters { source_hash_prefix: Some("%".into()), ..Default::default() }).await.is_empty());
}
