//! Desktop adapters for exact local lexical summaries.
use std::{collections::HashMap, sync::{Arc, Mutex, OnceLock}};
use tauri::{Manager, State};

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
    cancellation.cancel()
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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LexicalReportRequest {
    query: String,
    mode: String,
    request_id: String,
    path: std::path::PathBuf,
    format: search_summary::ReportFormat,
    sort: search_summary::ReportSort,
    expected_generation: Option<String>,
    owner_id: Option<String>,
    include_unmounted: Option<bool>,
    fuzzy: Option<bool>,
    doc_id_scope: Option<Vec<String>>,
    tag: Option<String>,
    synonyms: Option<bool>,
    search_filters: Option<SearchFilters>,
}

fn configured_filters(search_filters: Option<SearchFilters>, owner_id: Option<String>, fuzzy: Option<bool>,
    synonyms: Option<bool>, doc_id_scope: Option<Vec<String>>, tag: Option<String>) -> Result<SearchFilters, String> {
    let mut filters = search_filters.unwrap_or_default();
    filters.owner_id = owner_id.or(filters.owner_id);
    if let Some(value) = fuzzy { filters.fuzzy = value; }
    if let Some(value) = synonyms { filters.synonyms = value; }
    if let Some(value) = doc_id_scope { filters.doc_id_scope = value; }
    filters.tag = tag.or(filters.tag);
    super::tauri_commands::validate_search_filters(&filters)?;
    Ok(filters)
}

#[tauri::command]
pub async fn index_export_lexical_report(
    app: tauri::AppHandle, state: State<'_, AppState>, request: LexicalReportRequest,
) -> Result<search_summary::ReportReceipt, String> {
    if request.mode != "text" { return Err("Complete reports are available only for local Full text search".into()); }
    let scan = ScanLease::new(request.request_id)?;
    let mut filters = configured_filters(request.search_filters, request.owner_id, request.fuzzy,
        request.synonyms, request.doc_id_scope, request.tag)?;
    let (local, fts) = active_local(&state).await?;
    let volumes = mounted_volumes(request.include_unmounted.unwrap_or(false)).await?;
    filters.mounted_volume_ids = volumes.clone();
    let roots = vec![app.path().app_data_dir().map_err(|error| error.to_string())?];
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    let prepared = search_summary::prepare_export(local.clone(), fts.clone(), request.query, filters,
        search_summary::ExportOptions { path: request.path, format: request.format, sort: request.sort,
            expected_generation: request.expected_generation, protected_roots: roots }, Some(scan.cancellation.clone()))
        .await.map_err(|error| error.to_string())?;
    if mounted_volumes(request.include_unmounted.unwrap_or(false)).await? != volumes {
        return Err("Mounted sources changed during export. The previous report was preserved; search again.".into());
    }
    // Retain the configuration lock through publication, so a backend switch
    // cannot relabel this captured index as the currently selected source.
    let lock = state.index.lock().await;
    if !lock.config.enabled || matches!(lock.config.backend_type, super::BackendType::Remote)
        || !lock.local.as_ref().is_some_and(|active| Arc::ptr_eq(active, &local))
        || !lock.fts.as_ref().is_some_and(|active| Arc::ptr_eq(active, &fts)) {
        return Err("The active index changed during export. The previous report was preserved.".into());
    }
    prepared.publish_with_validation(move || {
        if let Some(expected) = &volumes {
            let mut current: Vec<_> = crate::volume::list_mounted_volumes().into_iter().map(|volume| volume.id).collect();
            current.sort(); current.dedup();
            anyhow::ensure!(&current == expected, "Mounted sources changed before publication. The previous report was preserved; search again.");
        }
        Ok(())
    }).await.map_err(|error| error.to_string())
}

/// Returned-window exports share the atomic writer. The frontend has already
/// captured query, selection and report content before opening the Save dialog.
#[tauri::command]
pub async fn index_write_search_report(
    app: tauri::AppHandle, state: State<'_, AppState>, path: std::path::PathBuf,
    content: String, protected_source_uris: Vec<String>,
) -> Result<std::path::PathBuf, String> {
    if content.len() > 32 * 1024 * 1024 { return Err("The returned-window report exceeds 32 MiB. Use a streamed complete report.".into()); }
    if protected_source_uris.len() > 10000 { return Err("The returned-window source list is too large. Use a streamed complete report.".into()); }
    let lock = state.index.lock().await;
    let (local, fts) = (lock.local.clone(), lock.fts.clone());
    let mut roots = vec![app.path().app_data_dir().map_err(|error| error.to_string())?];
    if let Some(directory) = local.as_ref().and_then(|local| local.ingest_journal_path().parent()) { roots.push(directory.to_owned()); }
    drop(lock);
    let mut guard = None;
    if let (Some(local), Some(fts)) = (&local, &fts) {
        guard = Some(local.read_guard(fts).await.map_err(|error| error.to_string())?);
        search_summary::protect_index_sources(local, path.clone(), &None).await.map_err(|error| error.to_string())?;
    }
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        super::report_output::write_captured_report(&path, &content, &roots, &protected_source_uris)
    }).await.map_err(|error| error.to_string())?.map_err(|error| error.to_string())
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
    let mut filters = configured_filters(search_filters, owner_id, fuzzy, synonyms, doc_id_scope, tag)?;
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
