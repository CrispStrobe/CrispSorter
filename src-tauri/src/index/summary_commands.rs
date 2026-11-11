//! Desktop adapters for exact local and remote lexical snapshots.
use std::{collections::HashMap, sync::{Arc, Mutex, OnceLock}};
use tauri::{Manager, State};

use super::{schema::SearchFilters, search_summary, remote_client::{RemoteClient, complete_search_scope}, remote_summary, FtsIndex, LocalIndex};
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

enum ActiveSearchIndex {
    Local(Arc<LocalIndex>, Arc<FtsIndex>),
    Remote(RemoteSelection),
}
struct RemoteSelection {
    client: Arc<RemoteClient>,
    backend: Arc<dyn super::IndexBackend>,
    url: String,
    api_key: String,
}
impl RemoteSelection {
    fn is_active(&self, state: &super::IndexState) -> bool {
        state.config.enabled && matches!(state.config.backend_type, super::BackendType::Remote)
            && state.config.remote_url.as_deref() == Some(self.url.as_str())
            && state.config.remote_api_key.as_deref().unwrap_or_default() == self.api_key
            && state.backend.as_ref().is_some_and(|backend| Arc::ptr_eq(backend, &self.backend))
    }
}

async fn active_search_index(state: &AppState) -> Result<ActiveSearchIndex, String> {
    let lock = state.index.lock().await;
    if !lock.config.enabled { return Err("Enable the text index to count all lexical matches".into()); }
    if matches!(lock.config.backend_type, super::BackendType::Remote) {
        let backend = lock.backend.clone().ok_or("The remote index is not initialized")?;
        let url = lock.config.remote_url.clone().ok_or("The remote index URL is missing")?;
        let api_key = lock.config.remote_api_key.clone().unwrap_or_default();
        drop(lock);
        let proxy = crate::sync::tauri_commands::proxy_config_for_app_state(state).await?;
        let client = Arc::new(RemoteClient::new_with_proxy(&url, &api_key, &proxy).map_err(|error| error.to_string())?);
        return Ok(ActiveSearchIndex::Remote(RemoteSelection { client, backend, url, api_key }));
    }
    if lock.engine.is_none() { return Err("The local full-text search engine is not initialized".into()); }
    Ok(ActiveSearchIndex::Local(lock.local.clone().ok_or("The local text index is not initialized")?,
        lock.fts.clone().ok_or("The local full-text index is not initialized")?))
}

#[tauri::command]
pub async fn index_search_summary_generation(state: State<'_, AppState>, include_unmounted: Option<bool>,
    query: Option<String>, mode: Option<String>, owner_id: Option<String>, fuzzy: Option<bool>,
    doc_id_scope: Option<Vec<String>>, tag: Option<String>, synonyms: Option<bool>, search_filters: Option<SearchFilters>,
) -> Result<String, String> {
    if mode.as_deref().is_some_and(|mode| mode != "text") { return Err("Complete snapshots are available only for Full text search".into()); }
    match active_search_index(&state).await? {
        ActiveSearchIndex::Local(local, fts) => {
            let volumes = mounted_volumes(include_unmounted.unwrap_or(false)).await?;
            search_summary::generation(&local, &fts, volumes.as_deref()).await.map_err(|error| error.to_string())
        }
        ActiveSearchIndex::Remote(remote) => {
            let filters = configured_filters(search_filters, owner_id, fuzzy, synonyms, doc_id_scope, tag)?;
            let scope = complete_search_scope(&query.ok_or("The executed query is required to capture a remote snapshot")?, &filters)
                .map_err(|error| error.to_string())?;
            let receipt = remote.client.complete_generation(&scope).await.map_err(|error| error.to_string())?;
            let lock = state.index.lock().await;
            if !remote.is_active(&lock) { return Err("The active index changed while capturing its snapshot. Search again.".into()); }
            Ok(remote.client.bind_generation(&receipt.generation))
        }
    }
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
    if request.mode != "text" { return Err("Complete reports are available only for Full text search".into()); }
    let scan = ScanLease::new(request.request_id.clone())?;
    let mut filters = configured_filters(request.search_filters, request.owner_id, request.fuzzy,
        request.synonyms, request.doc_id_scope, request.tag)?;
    let active = active_search_index(&state).await?;
    if let ActiveSearchIndex::Remote(remote) = active {
        let lock = state.index.lock().await;
        let (local, fts) = (lock.local.clone(), lock.fts.clone());
        let mut roots = vec![app.path().app_data_dir().map_err(|error| error.to_string())?];
        if let Some(root) = local.as_ref().and_then(|local| local.ingest_journal_path().parent()) { roots.push(root.to_owned()); }
        drop(lock);
        let prepared = remote_summary::prepare_export(remote.client.clone(), request.query, filters,
            remote_summary::RemoteExportOptions { request_id: request.request_id, path: request.path,
                format: request.format, sort: request.sort,
                expected_generation: request.expected_generation.ok_or("Capture the remote snapshot before choosing a report destination")?,
                protected_roots: roots }, scan.cancellation.clone()).await.map_err(|error| error.to_string())?;
        // A retained local index can still contain source files and application
        // data. Protect it without holding state.index while acquiring its gate.
        let mut guard = None;
        if let (Some(local), Some(fts)) = (&local, &fts) {
            guard = Some(remote_summary::cancellable(&scan.cancellation, local.read_guard(fts)).await.map_err(|error| error.to_string())?);
            search_summary::protect_index_sources(local, prepared.destination().to_owned(), &Some(scan.cancellation.clone()))
                .await.map_err(|error| error.to_string())?;
        }
        let lock = state.index.lock().await;
        if !remote.is_active(&lock) { return Err("The active remote index changed during export. The previous report was preserved.".into()); }
        let result = prepared.publish(|| Ok(())).await.map_err(|error| error.to_string());
        drop(guard);
        return result;
    }
    let ActiveSearchIndex::Local(local, fts) = active else { unreachable!() };
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
    let active = active_search_index(&state).await?;
    if let ActiveSearchIndex::Remote(remote) = active {
        let scope = complete_search_scope(&query, &filters).map_err(|error| error.to_string())?;
        let summary = remote_summary::summarize(remote.client.clone(), scope, scan.request_id.clone(), scan.cancellation.clone())
            .await.map_err(|error| error.to_string())?;
        let lock = state.index.lock().await;
        if !remote.is_active(&lock) { return Err("The active remote index changed while counting matches. Search again.".into()); }
        let facet = |value: crisp_index_protocol::CompleteFacet| search_summary::FacetSummary {
            values: value.values.into_iter().map(|value| search_summary::FacetValue { value: value.value, documents: value.documents }).collect(),
            distinct_values: value.distinct_values, omitted_values: value.omitted_values, missing_documents: value.missing_documents,
        };
        return Ok(search_summary::SearchSummary { version: 1, scope: "remote_indexed_documents", query,
            effective_query: summary.query, generation: remote.client.bind_generation(&summary.generation),
            matching_documents: summary.matching_documents, scanned_rows: summary.scanned_rows,
            facet_value_limit: summary.facet_value_limit, facets: search_summary::SearchFacets {
                extensions: facet(summary.facets.extensions), languages: facet(summary.facets.languages),
                years: facet(summary.facets.years), tags: facet(summary.facets.tags),
            } });
    }
    let ActiveSearchIndex::Local(local, fts) = active else { unreachable!() };
    let volumes = mounted_volumes(include_unmounted.unwrap_or(false)).await?;
    filters.mounted_volume_ids = volumes.clone();
    let _foreground = crate::bg_ingest::ForegroundGuard::new(state.foreground_active.clone());
    let summary = search_summary::summarize(local, fts, query, filters, Some(scan.cancellation.clone())).await.map_err(|error| error.to_string())?;
    if mounted_volumes(include_unmounted.unwrap_or(false)).await? != volumes {
        return Err("Mounted sources changed while counting matches. Search again to refresh the summary.".into());
    }
    Ok(summary)
}
