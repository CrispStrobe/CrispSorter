//! Desktop adapters for exact local lexical summaries.
use std::{collections::HashMap, sync::{Arc, Mutex, OnceLock}};
use tauri::State;

use super::{schema::SearchFilters, search_summary, FtsIndex, LocalIndex};
use crate::AppState;

static SCANS: OnceLock<Mutex<HashMap<String, Arc<search_summary::ScanCancellation>>>> = OnceLock::new();

struct ScanLease { request_id: String, cancellation: Arc<search_summary::ScanCancellation> }
impl ScanLease {
    fn new(request_id: String) -> Result<Self, String> {
        if request_id.is_empty() || request_id.len() > 128 { return Err("A valid summary request identity is required".into()); }
        let mut scans = SCANS.get_or_init(Mutex::default).lock().map_err(|error| error.to_string())?;
        if scans.len() >= 16 { return Err("Too many matching-document scans are active. Cancel a scan and try again.".into()); }
        let cancellation = Arc::new(search_summary::ScanCancellation::default());
        if let Some(previous) = scans.insert(request_id.clone(), cancellation.clone()) { previous.cancel(); }
        Ok(Self { request_id, cancellation })
    }
}
impl Drop for ScanLease {
    fn drop(&mut self) {
        if let Some(mut scans) = SCANS.get().and_then(|scans| scans.lock().ok()) {
            if scans.get(&self.request_id).is_some_and(|flag| Arc::ptr_eq(flag, &self.cancellation)) {
                scans.remove(&self.request_id);
            }
        }
    }
}

#[tauri::command]
pub fn index_cancel_search_summary(request_id: String) -> bool {
    let Some(scans) = SCANS.get().and_then(|scans| scans.lock().ok()) else { return false; };
    let Some(cancellation) = scans.get(&request_id) else { return false; };
    cancellation.cancel();
    true
}

async fn active_local(state: &AppState) -> Result<(Arc<LocalIndex>, Arc<FtsIndex>), String> {
    let lock = state.index.lock().await;
    if !lock.config.enabled { return Err("Enable the local text index to count all lexical matches".into()); }
    if matches!(lock.config.backend_type, super::BackendType::Remote) {
        return Err("Complete lexical summaries require the local index; remote totals are unavailable".into());
    }
    if lock.engine.is_none() { return Err("Complete lexical summaries require the local index; remote and semantic totals are unavailable".into()); }
    Ok((lock.local.clone().ok_or("The local text index is not initialized")?,
        lock.fts.clone().ok_or("The local full-text index is not initialized")?))
}

#[tauri::command]
pub async fn index_search_summary_generation(state: State<'_, AppState>, include_unmounted: Option<bool>) -> Result<String, String> {
    let (local, fts) = active_local(&state).await?;
    let volumes = mounted_volumes(include_unmounted.unwrap_or(false)).await?;
    search_summary::generation(&local, &fts, volumes.as_deref()).await.map_err(|error| error.to_string())
}

async fn mounted_volumes(include_unmounted: bool) -> Result<Option<Vec<String>>, String> {
    if include_unmounted { return Ok(None); }
    let mut ids: Vec<_> = tokio::task::spawn_blocking(crate::volume::list_mounted_volumes).await
        .map_err(|error| error.to_string())?.into_iter().map(|volume| volume.id).collect();
    ids.sort(); ids.dedup(); Ok(Some(ids))
}

/// Argument compatibility deliberately follows index_search, excluding its
/// interactive limit. This command never counts semantic or catalog matches.
#[tauri::command]
pub async fn index_search_summary(
    state: State<'_, AppState>, query: String, mode: String, request_id: String,
    owner_id: Option<String>, include_unmounted: Option<bool>, fuzzy: Option<bool>,
    doc_id_scope: Option<Vec<String>>, tag: Option<String>, synonyms: Option<bool>,
    search_filters: Option<SearchFilters>,
) -> Result<search_summary::SearchSummary, String> {
    if mode != "text" { return Err("Complete match totals are available only for Full text search".into()); }
    let scan = ScanLease::new(request_id)?;
    let mut filters = search_filters.unwrap_or_default();
    filters.owner_id = owner_id.or(filters.owner_id);
    if let Some(value) = fuzzy { filters.fuzzy = value; }
    if let Some(value) = synonyms { filters.synonyms = value; }
    if let Some(value) = doc_id_scope { filters.doc_id_scope = value; }
    filters.tag = tag.or(filters.tag);
    super::tauri_commands::validate_search_filters(&filters)?;
    let (local, fts) = active_local(&state).await?;
    let volumes = mounted_volumes(include_unmounted.unwrap_or(false)).await?;
    filters.mounted_volume_ids = volumes.clone();
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    let summary = search_summary::summarize(local, fts, query, filters, Some(scan.cancellation.clone())).await.map_err(|error| error.to_string())?;
    if mounted_volumes(include_unmounted.unwrap_or(false)).await? != volumes {
        return Err("Mounted sources changed while counting matches. Search again to refresh the summary.".into());
    }
    Ok(summary)
}
