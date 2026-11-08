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
