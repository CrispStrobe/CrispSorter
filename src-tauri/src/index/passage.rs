//! Stored-passage inspection shared with the remote index service.
//! The caller holds the paired index read guard for all storage queries.
use anyhow::Result;
pub use crisp_index_protocol::StoredPassage as InspectedPassage;
pub const MAX_PASSAGE_BYTES: usize = crisp_index_protocol::MAX_STORED_PASSAGE_BYTES;
#[path = "../../../shared/stored_passage.rs"]
mod stored_passage;

pub async fn read_passage(table: &lancedb::Table, doc_id: &str, chunk_index: i32) -> Result<InspectedPassage> {
    read_passage_scoped(table, doc_id, None, chunk_index).await
}

pub async fn read_passage_scoped(table: &lancedb::Table, doc_id: &str, owner_id: Option<&str>, chunk_index: i32) -> Result<InspectedPassage> {
    stored_passage::read_passage(table, doc_id, owner_id, chunk_index).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{ingest::chunk_row_id, schema::DocumentChunk, LocalIndex};

    fn chunk(doc_id: &str, index: i32, text: &str) -> DocumentChunk {
        serde_json::from_value(serde_json::json!({
            "id": chunk_row_id(doc_id, index), "doc_id": doc_id, "owner_id": "reader",
            "location_uri": "/library/book.txt", "filename": "book.txt", "title": "Book",
            "full_text": text, "chunk_index": index, "chunk_total": 900,
            "chunk_start_char": 37 + index.max(0) as usize * 100, "chunk_end_char": 37 + index.max(0) as usize * 100 + text.len(),
            "source_hash": "current-fingerprint", "indexed_at": 1234, "tags": [],
        })).unwrap()
    }

    #[tokio::test]
    async fn navigation_uses_actual_sparse_chunks_and_isolates_quoted_document_ids() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
        let id = "book' OR 1=1 --";
        local.ingest_batch(&[chunk(id, 9, "last"), chunk(id, 2, "first"), chunk(id, 5, "middle"),
            chunk(id, -1, ""), chunk("unrelated", 4, "private")]).await.unwrap();
        let middle = read_passage(local.table_ref(), id, 5).await.unwrap();
        assert_eq!(middle.text, "middle");
        assert_eq!((middle.passage_number, middle.chunk_count), (2, 3));
        assert_eq!((middle.previous_chunk_index, middle.next_chunk_index), (Some(2), Some(9)));
        assert_eq!(middle.source_hash.as_deref(), Some("current-fingerprint"));
        assert_eq!(middle.indexed_at, Some(1234));
        assert_eq!((middle.start_char, middle.end_char), (Some(537), Some(543)));
        let first = read_passage(local.table_ref(), id, 2).await.unwrap();
        assert_eq!(first.previous_chunk_index, None);
        assert_eq!(first.passage_number, 1);
        assert_eq!(read_passage(local.table_ref(), id, 9).await.unwrap().next_chunk_index, None);
        assert!(read_passage(local.table_ref(), id, 4).await.unwrap_err().to_string().contains("no longer available"));
        assert!(read_passage(local.table_ref(), id, -1).await.is_err());
    }

    #[tokio::test]
    async fn large_unicode_passage_is_capped_at_a_character_boundary_with_original_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
        let text = "東".repeat(MAX_PASSAGE_BYTES / 3 + 10);
        local.ingest_batch(&[chunk("unicode", 0, &text)]).await.unwrap();
        let passage = read_passage(local.table_ref(), "unicode", 0).await.unwrap();
        assert!(passage.truncated);
        assert!(passage.text.len() <= MAX_PASSAGE_BYTES);
        assert!(passage.text.len() >= MAX_PASSAGE_BYTES - 3);
        assert_eq!(passage.end_char, Some(37 + text.len() as i32));
        assert_eq!((passage.passage_number, passage.chunk_count), (1, 1));
    }

    #[tokio::test]
    async fn scoped_passages_isolate_legacy_owners_and_deduplicate_latest_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
        let mut original = chunk("shared", 2, "original");
        original.owner_id = "owner' OR 1=1 --".into();
        local.ingest_batch(&[original.clone()]).await.unwrap();
        let mut foreign = chunk("shared", 2, "foreign-private");
        foreign.owner_id = "other-owner".into();
        foreign.location_uri = "/another/source.txt".into();
        local.append_legacy_rows_for_test(&[foreign]).await.unwrap();
        assert!(read_passage(local.table_ref(), "shared", 2).await.unwrap_err().to_string().contains("multiple owners"));
        assert!(read_passage_scoped(local.table_ref(), "shared", Some("missing-owner"), 2).await.is_err());
        assert_eq!(read_passage_scoped(local.table_ref(), "shared", Some(&original.owner_id), 2).await.unwrap().text, "original");
        let mut updated = chunk("shared", 2, "newer text");
        updated.owner_id = original.owner_id.clone(); updated.indexed_at = 1235;
        updated.source_hash = "newer-fingerprint".into();
        let mut later = chunk("shared", 8, "later"); later.owner_id = original.owner_id.clone();
        local.append_legacy_rows_for_test(&[updated, later]).await.unwrap();
        let passage = read_passage_scoped(local.table_ref(), "shared", Some(&original.owner_id), 2).await.unwrap();
        assert_eq!(passage.text, "newer text");
        assert_eq!(passage.source_hash.as_deref(), Some("newer-fingerprint"));
        assert_eq!((passage.passage_number, passage.chunk_count), (1, 2));
        assert_eq!(passage.next_chunk_index, Some(8));
    }
}
