//! Bounded retrieval of real indexed passages for cited document chat.
use tauri::State;
use serde::Serialize;
use crate::AppState;
use super::{ingest::chunk_row_id, local_index::Passage, SearchFilters, SearchResult};

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub document: SearchResult,
    pub passage: Passage,
}

/// A bounded snapshot of one indexed chunk, with its real byte offsets.
#[derive(Debug, Clone, Serialize)]
pub struct InspectedPassage {
    pub text: String,
    pub start_char: Option<i32>,
    pub end_char: Option<i32>,
    pub offset_unit: &'static str,
    pub truncated: bool,
}

#[tauri::command]
pub async fn index_read_passage(
    state: State<'_, AppState>,
    doc_id: String,
    chunk_index: i32,
) -> Result<InspectedPassage, String> {
    use arrow_array::{Array, Int32Array, StringArray};
    use futures::TryStreamExt;
    use lancedb::query::{ExecutableQuery, QueryBase, Select};
    if doc_id.trim().is_empty() || doc_id.len() > 4096 || chunk_index < 0 {
        return Err("Choose an indexed text passage; filename-only results have no stored passage".into());
    }
    let lock = state.index.lock().await;
    if !lock.config.enabled { return Err("Enable the search index to inspect stored passages".into()); }
    let local = lock.local.clone().ok_or("Stored passage inspection requires a local index")?;
    drop(lock);
    let id = chunk_row_id(&doc_id, chunk_index).replace('\'', "''");
    let batches: Vec<arrow_array::RecordBatch> = local.table_ref().query()
        .only_if(format!("id = '{id}'"))
        .select(Select::Columns(vec!["full_text".into(), "chunk_start_char".into(), "chunk_end_char".into()]))
        .limit(1).execute().await.map_err(|e| e.to_string())?
        .try_collect().await.map_err(|e| e.to_string())?;
    for batch in batches {
        if batch.num_rows() == 0 { continue; }
        let text = batch.column_by_name("full_text").and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .filter(|column| !column.is_null(0)).map(|column| column.value(0))
            .ok_or("This result has no stored text. Extract or OCR the source first.")?;
        const MAX_BYTES: usize = 256 * 1024;
        let truncated = text.len() > MAX_BYTES;
        let mut end = text.len().min(MAX_BYTES);
        while !text.is_char_boundary(end) { end -= 1; }
        let offset = |name: &str| batch.column_by_name(name)
            .and_then(|column| column.as_any().downcast_ref::<Int32Array>())
            .filter(|column| !column.is_null(0)).map(|column| column.value(0));
        return Ok(InspectedPassage { text: text[..end].to_owned(), start_char: offset("chunk_start_char"),
            end_char: offset("chunk_end_char"), offset_unit: "utf8_bytes", truncated });
    }
    Err("This indexed passage is no longer available. Search again to refresh the results.".into())
}

#[tauri::command]
pub async fn index_filename_only_count(state: State<'_, AppState>) -> Result<usize, String> {
    let lock = state.index.lock().await;
    let Some(local) = lock.local.clone().filter(|_| lock.config.enabled) else { return Ok(0); };
    drop(lock);
    local.filename_only_count().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn index_retrieve_passages(
    state: State<'_, AppState>,
    query: String,
    doc_ids: Option<Vec<String>>,
    limit: Option<usize>,
) -> Result<Vec<Evidence>, String> {
    if query.trim().is_empty() || query.chars().count() > 4096 {
        return Err("Enter a question of at most 4096 characters".into());
    }
    let ids = doc_ids.unwrap_or_default();
    if ids.len() > 500 { return Err("Select at most 500 documents".into()); }
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    let lock = state.index.lock().await;
    if !lock.config.enabled { return Err("Enable the search index to ask your library".into()); }
    let engine = lock.engine.clone().ok_or("Search index is not ready")?;
    let local = lock.local.clone().ok_or("Passage retrieval currently requires a local index")?;
    drop(lock);
    let filters = SearchFilters { doc_id_scope: ids, ..Default::default() };
    let limit = limit.unwrap_or(12).clamp(1, 32);
    let results = match engine.search_hybrid(&query, &filters, limit).await {
        Ok(results) => results,
        Err(e) => {
            log::warn!("Semantic evidence retrieval unavailable; using full-text search: {e}");
            engine.search_text(&query, &filters, limit).await.map_err(|e| e.to_string())?
        }
    };
    let mut passages = local.fetch_passages(&results).await.map_err(|e| e.to_string())?;
    Ok(results.into_iter().filter_map(|document| {
        passages.remove(&chunk_row_id(&document.doc_id, document.chunk_index))
            .map(|passage| Evidence { document, passage })
    }).collect())
}
