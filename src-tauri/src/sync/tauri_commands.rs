//! Tauri commands for the SyncManager (P11 Pillar 6).

use tauri::State;
use crate::AppState;
use super::{SyncManager, SyncStatus};

/// Return sync status: pending count, last push/pull timestamps, online state.
#[tauri::command]
pub async fn sync_status(
    state: State<'_, AppState>,
) -> Result<SyncStatus, String> {
    let (data_dir, remote_url, api_key) = {
        let dd = state.data_dir.lock().await.clone();
        let idx = state.index.lock().await;
        let url = idx.config.remote_url.clone();
        let key = idx.config.remote_api_key.clone().unwrap_or_default();
        (dd, url, key)
    };
    let data_dir = data_dir.ok_or("data_dir not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
    let mut status = mgr.status(remote_url.as_deref());

    // Async ping to check online state.
    if let Some(ref url) = remote_url {
        status.remote_online = SyncManager::is_remote_online(url).await;
    }
    let _ = api_key;
    Ok(status)
}

/// Manually trigger a push of all pending outbox entries.
#[tauri::command]
pub async fn sync_push(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let (data_dir, remote_url, api_key) = {
        let dd = state.data_dir.lock().await.clone();
        let idx = state.index.lock().await;
        let url = idx.config.remote_url.clone()
            .ok_or_else(|| "remote_url not configured".to_string())?;
        let key = idx.config.remote_api_key.clone().unwrap_or_default();
        (dd, url, key)
    };
    let data_dir = data_dir.ok_or("data_dir not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
    let (pushed, failed) = mgr.push_pending(&remote_url, &api_key)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "pushed": pushed, "failed": failed }))
}

/// Enqueue a manual outbox entry (primarily for testing the sync path).
#[tauri::command]
pub async fn sync_enqueue(
    state: State<'_, AppState>,
    op: String,
    payload: String,
) -> Result<i64, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
    mgr.enqueue(&op, &payload).map_err(|e| e.to_string())
}

/// Pull rows from the remote server that have changed since `last_pull_ts`.
/// First-cut: just refreshes `last_pull_ts` so the chip shows progress;
/// applying remote rows to the local LanceDB is a follow-up task.
#[tauri::command]
pub async fn sync_pull(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let (data_dir, remote_url, api_key) = {
        let dd = state.data_dir.lock().await.clone();
        let idx = state.index.lock().await;
        let url = idx.config.remote_url.clone()
            .ok_or_else(|| "remote_url not configured".to_string())?;
        let key = idx.config.remote_api_key.clone().unwrap_or_default();
        (dd, url, key)
    };
    let data_dir = data_dir.ok_or("data_dir not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
    let (pulled, max_ts) = mgr.pull_pending(&remote_url, &api_key, 200)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "pulled": pulled, "max_indexed_at": max_ts }))
}

/// Clear all permanently-failed outbox entries (retries ≥ 10).
#[tauri::command]
pub async fn sync_clear_failed(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
    mgr.clear_failed().map_err(|e| e.to_string())
}
