//! Opening a member always starts with owner-scoped persisted metadata. The
//! caller cannot supply a locator or an arbitrary extraction/output path.
use std::{path::PathBuf, sync::Arc};
use crisp_index_protocol::ContainerMemberLocator;
use tauri::{Manager, State};

use crate::AppState;
use super::{BackendType, FtsIndex, IndexBackend, IndexState, LocalIndex, remote_client::RemoteClient};
#[path = "container_cache.rs"]
mod cache;
#[path = "../../../shared/source_paths.rs"]
mod source_paths;

static MATERIALIZATIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedContainerMember {
    path: String,
    filename: String,
    root_location_uri: String,
    root_sha256: String,
    owner_id: String,
    doc_id: String,
    source_hash: String,
}

enum ActiveMemberIndex {
    Local(Arc<LocalIndex>, Arc<FtsIndex>),
    Remote { client: RemoteClient, backend: Arc<dyn IndexBackend>, url: String, key: String },
}
impl ActiveMemberIndex {
    fn is_active(&self, state: &IndexState) -> bool {
        if !state.config.enabled { return false; }
        match self {
            Self::Local(local,fts) => state.config.backend_type != BackendType::Remote
                && state.local.as_ref().is_some_and(|active| Arc::ptr_eq(active,local))
                && state.fts.as_ref().is_some_and(|active| Arc::ptr_eq(active,fts)),
            Self::Remote { backend,url,key,.. } => state.config.backend_type == BackendType::Remote
                && state.config.remote_url.as_deref() == Some(url.as_str())
                && state.config.remote_api_key.as_deref().unwrap_or_default() == key
                && state.backend.as_ref().is_some_and(|active| Arc::ptr_eq(active,backend)),
        }
    }
}

async fn selected_index(state: &AppState) -> Result<ActiveMemberIndex,String> {
    let lock = state.index.lock().await;
    if !lock.config.enabled { return Err("Enable the index before opening an indexed container member".into()); }
    if lock.config.backend_type == BackendType::Remote {
        let backend = lock.backend.clone().ok_or("The remote index is not initialized")?;
        let url = lock.config.remote_url.clone().ok_or("The remote index URL is missing")?;
        let key = lock.config.remote_api_key.clone().unwrap_or_default();
        drop(lock);
        let proxy = crate::sync::tauri_commands::proxy_config_for_app_state(state).await?;
        let client = RemoteClient::new_with_proxy(&url,&key,&proxy).map_err(|error| error.to_string())?;
        Ok(ActiveMemberIndex::Remote { client,backend,url,key })
    } else {
        Ok(ActiveMemberIndex::Local(lock.local.clone().ok_or("The local index is not ready")?,
            lock.fts.clone().ok_or("The local text index is not ready")?))
    }
}

fn check_selected(locator: &ContainerMemberLocator, owner_id: &str, doc_id: &str) -> Result<(),String> {
    locator.validate_identity(owner_id,doc_id,&locator.member_uri()?).map_err(|error| error.to_string())
}

async fn prepare_copy(root: PathBuf, locator: ContainerMemberLocator) -> Result<cache::PreparedMemberCache,String> {
    tauri::async_runtime::spawn_blocking(move || {
        let candidates = source_paths::local_source_paths(&locator.root_location_uri);
        let mut last_error = None;
        for path in candidates.into_iter().rev().filter(|path| path.is_absolute()) {
            if !path.is_file() { continue; }
            match crate::extractors::container::materialize_member(&path,&locator) {
                Ok(member) => return cache::PreparedMemberCache::prepare(&root,&locator,member.file,&member.extension,&member.sha256,member.size)
                    .map_err(|error| format!("{error:#}")),
                Err(error) => last_error = Some(format!("{error:#}")),
            }
        }
        Err(last_error.unwrap_or_else(|| "The source container is unavailable on this device. Restore or mount its recorded local source, then try again.".into()))
    }).await.map_err(|error| format!("Container member worker failed: {error}"))?
}

/// Return a verified read-only extracted copy for preview or explicit opening.
#[tauri::command]
pub async fn index_open_container_member(app: tauri::AppHandle, state: State<'_, AppState>,
    owner_id: String, doc_id: String) -> Result<OpenedContainerMember,String> {
    if [&owner_id,&doc_id].iter().any(|value| value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control)) {
        return Err("Choose a container member with its exact owner and document identity".into());
    }
    let _slot = MATERIALIZATIONS.acquire().await.map_err(|error| error.to_string())?;
    let index = selected_index(&state).await?;
    let cache_root = app.path().app_data_dir().map_err(|error| error.to_string())?.join("container-cache");
    let locator;
    let published;
    match &index {
        ActiveMemberIndex::Local(local,fts) => {
            // Keep the paired read lease through metadata validation, bounded
            // extraction and publication. Root replacement cannot race opening.
            let _lease = local.read_guard(fts).await.map_err(|error| error.to_string())?;
            locator = local.container_member_locator(&owner_id,&doc_id).await.map_err(|error| error.to_string())?
                .ok_or("The selected document is not an indexed container member")?;
            check_selected(&locator,&owner_id,&doc_id)?;
            let prepared = prepare_copy(cache_root,locator.clone()).await?;
            // index_init reserves its slot then releases state.index before
            // init_index/pipeline.ensure_ready can wait on this paired lease.
            // index_set_config (including disable) only swaps configuration;
            // neither path holds state.index while awaiting the writer gate.
            let lock = state.index.lock().await;
            if !index.is_active(&lock) { return Err("The active index changed while the member was opening. Search again.".into()); }
            published = tauri::async_runtime::spawn_blocking(move || prepared.publish()).await
                .map_err(|error| format!("Member cache worker failed: {error}"))?.map_err(|error| format!("{error:#}"))?;
            drop(lock);
        }
        ActiveMemberIndex::Remote { client,.. } => {
            locator = client.read_container_member(&doc_id,&owner_id).await.map_err(|error| error.to_string())?
                .ok_or("The selected document is not an indexed container member")?;
            check_selected(&locator,&owner_id,&doc_id)?;
            let prepared = prepare_copy(cache_root,locator.clone()).await?;
            // This recheck validates the captured remote locator, not a remote
            // live read lease. The returned hashes describe the verified copy.
            let current = client.read_container_member(&doc_id,&owner_id).await.map_err(|error| error.to_string())?;
            if current.as_ref() != Some(&locator) { return Err("The indexed source changed while the member was opening. Search again.".into()); }
            let lock = state.index.lock().await;
            if !index.is_active(&lock) { return Err("The configured remote index changed while the member was opening. Search again.".into()); }
            published = tauri::async_runtime::spawn_blocking(move || prepared.publish()).await
                .map_err(|error| format!("Member cache worker failed: {error}"))?.map_err(|error| format!("{error:#}"))?;
            drop(lock);
        }
    }
    let leaf = locator.entries.last().ok_or("The indexed member has no source receipt")?;
    Ok(OpenedContainerMember { path: published.to_string_lossy().into_owned(),
        filename: leaf.name.rsplit('/').next().unwrap_or(&leaf.name).to_owned(), root_location_uri: locator.root_location_uri,
        root_sha256: locator.root_sha256,
        owner_id,doc_id,source_hash:leaf.sha256.clone() })
}
