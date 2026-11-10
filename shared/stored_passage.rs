//! Shared Lance reader for exact stored passages. Callers hold their index read guard.
use anyhow::{anyhow, ensure, Context, Result};
use arrow_array::{Array, Int32Array, StringArray, TimestampMillisecondArray, UInt64Array};
use crisp_index_protocol::{StoredPassage, StoredPassageRequest, MAX_STORED_DOCUMENT_CHUNKS,
    MAX_STORED_PASSAGE_BYTES, STORED_PASSAGE_VERSION};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use std::collections::BTreeSet;

#[derive(Debug)]
pub enum PassageReadError { Unavailable, Ambiguous }
impl std::fmt::Display for PassageReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "This indexed passage is no longer available. Search again to refresh the results.",
            Self::Ambiguous => "This document identity refers to multiple owners or source locations. Select an exact source occurrence before inspecting its text.",
        })
    }
}
impl std::error::Error for PassageReadError {}

pub async fn read_passage(table: &lancedb::Table, doc_id: &str, owner_id: Option<&str>, chunk_index: i32) -> Result<StoredPassage> {
    let mut request = StoredPassageRequest { passage_version: STORED_PASSAGE_VERSION,
        doc_id: doc_id.to_owned(), owner_id: owner_id.unwrap_or_default().to_owned(), chunk_index };
    request.validate().map_err(|error| anyhow!(error))?;
    let doc = doc_id.replace('\'', "''");
    let mut scope = format!("doc_id = '{doc}'");
    if let Some(owner) = owner_id {
        scope.push_str(&format!(" AND owner_id = '{}'", owner.replace('\'', "''")));
    }
    // Metadata-only scanning keeps navigation independent of incorrect legacy
    // totals, duplicate rows, sparse indices, and the selected result window.
    let mut navigation = table.query().only_if(format!("({scope}) AND chunk_index >= 0"))
        .select(Select::columns(&["owner_id", "location_uri", "chunk_index", "indexed_at"]))
        .with_row_id().limit(MAX_STORED_DOCUMENT_CHUNKS + 1).execute().await
        .context("Reading indexed passage navigation")?;
    let mut indices = BTreeSet::new();
    let mut identity = None::<(String, String)>;
    let mut latest = None::<(i64, u64)>;
    let mut rows = 0usize;
    while let Some(batch) = navigation.try_next().await? {
        rows += batch.num_rows();
        ensure!(rows <= MAX_STORED_DOCUMENT_CHUNKS, "Stored document exceeds the 65536-row passage navigation limit");
        let owners = batch.column_by_name("owner_id").and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .context("The stored owner identity is invalid")?;
        let locations = batch.column_by_name("location_uri").and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .context("The stored source identity is invalid")?;
        let chunks = batch.column_by_name("chunk_index").and_then(|column| column.as_any().downcast_ref::<Int32Array>())
            .context("The stored passage index is invalid")?;
        let times = batch.column_by_name("indexed_at").and_then(|column| column.as_any().downcast_ref::<TimestampMillisecondArray>())
            .context("The stored passage timestamp is invalid")?;
        let row_ids = batch.column_by_name("_rowid").and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
            .context("The stored passage row identity is invalid")?;
        for row in 0..batch.num_rows() {
            ensure!(!owners.is_null(row) && !locations.is_null(row) && !chunks.is_null(row)
                && !row_ids.is_null(row), "The stored passage identity is incomplete");
            let current = (owners.value(row), locations.value(row));
            if let Some((owner, location)) = &identity {
                if (owner.as_str(), location.as_str()) != current { return Err(PassageReadError::Ambiguous.into()); }
            } else {
                identity = Some((current.0.to_owned(), current.1.to_owned()));
            }
            let index = chunks.value(row);
            indices.insert(index);
            if index == chunk_index {
                let candidate = (if times.is_null(row) { i64::MIN } else { times.value(row) }, row_ids.value(row));
                latest = Some(latest.map_or(candidate, |previous| previous.max(candidate)));
            }
        }
    }
    let (_, row_id) = latest.ok_or(PassageReadError::Unavailable)?;
    request.owner_id = identity.context("Missing stored owner identity")?.0;
    // Select the same canonical row used by navigation, including deterministic
    // ordering when append-only legacy writes share a timestamp.
    let mut selected = table.query()
        .only_if(format!("({scope}) AND chunk_index = {chunk_index} AND _rowid = {row_id}"))
        .select(Select::columns(&["full_text", "chunk_start_char", "chunk_end_char", "source_hash", "indexed_at"]))
        .with_row_id().limit(1).execute().await.context("Reading the stored passage")?;
    while let Some(batch) = selected.try_next().await? {
        if batch.num_rows() == 0 { continue; }
        let string = |name: &str| batch.column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .filter(|column| !column.is_null(0)).map(|column| column.value(0));
        let text = string("full_text").filter(|text| !text.is_empty())
            .context("This result has no stored text. Extract or OCR the source first.")?;
        let mut end = text.len().min(MAX_STORED_PASSAGE_BYTES);
        while !text.is_char_boundary(end) { end -= 1; }
        let offset = |name: &str| batch.column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
            .filter(|column| !column.is_null(0)).map(|column| column.value(0));
        let indexed_at = batch.column_by_name("indexed_at")
            .and_then(|column| column.as_any().downcast_ref::<TimestampMillisecondArray>())
            .filter(|column| !column.is_null(0)).map(|column| column.value(0));
        let result = StoredPassage {
            passage_version: STORED_PASSAGE_VERSION, doc_id: doc_id.to_owned(), owner_id: request.owner_id.clone(),
            text: text[..end].to_owned(), start_char: offset("chunk_start_char"), end_char: offset("chunk_end_char"),
            offset_unit: "utf8_bytes".into(), truncated: end < text.len(), chunk_index,
            passage_number: indices.range(..chunk_index).count() + 1, chunk_count: indices.len(),
            previous_chunk_index: indices.range(..chunk_index).next_back().copied(),
            next_chunk_index: indices.range((std::ops::Bound::Excluded(chunk_index), std::ops::Bound::Unbounded)).next().copied(),
            source_hash: string("source_hash").filter(|hash| !hash.is_empty()).map(str::to_owned), indexed_at,
        };
        result.validate_for(&request).map_err(|error| anyhow!(error))?;
        return Ok(result);
    }
    Err(PassageReadError::Unavailable.into())
}
