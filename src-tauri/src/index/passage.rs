//! Stored-passage inspection with navigation over the complete indexed document.
//! The caller holds the paired index read guard for both queries.
use anyhow::{bail, Context, Result};
use arrow_array::{Array, Int32Array, StringArray, TimestampMillisecondArray};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use serde::Serialize;

pub const MAX_PASSAGE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct InspectedPassage {
    pub text: String,
    pub start_char: Option<i32>,
    pub end_char: Option<i32>,
    pub offset_unit: &'static str,
    pub truncated: bool,
    pub chunk_index: i32,
    pub passage_number: usize,
    pub chunk_count: usize,
    pub previous_chunk_index: Option<i32>,
    pub next_chunk_index: Option<i32>,
    pub source_hash: Option<String>,
    pub indexed_at: Option<i64>,
}

pub async fn read_passage(table: &lancedb::Table, doc_id: &str, chunk_index: i32) -> Result<InspectedPassage> {
    if doc_id.trim().is_empty() || doc_id.len() > 4096 || chunk_index < 0 {
        bail!("Choose an indexed text passage; filename-only results have no stored passage");
    }
    let doc = doc_id.replace('\'', "''");
    let mut rows = table.query()
        .only_if(format!("doc_id = '{doc}' AND chunk_index = {chunk_index}"))
        .select(Select::Columns(["full_text", "chunk_start_char", "chunk_end_char", "source_hash", "indexed_at"]
            .into_iter().map(str::to_owned).collect()))
        .limit(1).execute().await.context("Reading the stored passage")?;
    let mut inspected = None;
    while let Some(batch) = rows.try_next().await? {
        if batch.num_rows() == 0 { continue; }
        let string = |name: &str| batch.column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .filter(|column| !column.is_null(0)).map(|column| column.value(0));
        let text = string("full_text").context("This result has no stored text. Extract or OCR the source first.")?;
        let mut end = text.len().min(MAX_PASSAGE_BYTES);
        while !text.is_char_boundary(end) { end -= 1; }
        let offset = |name: &str| batch.column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
            .filter(|column| !column.is_null(0)).map(|column| column.value(0));
        let indexed_at = batch.column_by_name("indexed_at")
            .and_then(|column| column.as_any().downcast_ref::<TimestampMillisecondArray>())
            .filter(|column| !column.is_null(0)).map(|column| column.value(0));
        inspected = Some(InspectedPassage {
            text: text[..end].to_owned(), start_char: offset("chunk_start_char"), end_char: offset("chunk_end_char"),
            offset_unit: "utf8_bytes", truncated: end < text.len(), chunk_index,
            passage_number: 1, chunk_count: 0, previous_chunk_index: None, next_chunk_index: None,
            source_hash: string("source_hash").filter(|hash| !hash.is_empty()).map(str::to_owned), indexed_at,
        });
        break;
    }
    let mut inspected = inspected.context("This indexed passage is no longer available. Search again to refresh the results.")?;
    // Read only chunk IDs for navigation: never load all document bodies or
    // assume contiguous IDs, insertion order, or a trustworthy legacy total.
    let mut chunks = table.query()
        .only_if(format!("doc_id = '{doc}' AND chunk_index >= 0"))
        .select(Select::Columns(vec!["chunk_index".into()]))
        .execute().await.context("Reading indexed passage navigation")?;
    while let Some(batch) = chunks.try_next().await? {
        let indices = batch.column_by_name("chunk_index")
            .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
            .context("The stored passage index is invalid")?;
        for index in indices.iter().flatten() {
            inspected.chunk_count += 1;
            if index < chunk_index {
                inspected.passage_number += 1;
                inspected.previous_chunk_index = Some(inspected.previous_chunk_index.map_or(index, |previous| previous.max(index)));
            } else if index > chunk_index {
                inspected.next_chunk_index = Some(inspected.next_chunk_index.map_or(index, |next| next.min(index)));
            }
        }
    }
    Ok(inspected)
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
}
