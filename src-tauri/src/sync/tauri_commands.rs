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

/// Pull rows from the remote server that have changed since `last_pull_ts`,
/// then apply them to the local LanceDB as L1 metadata-only rows
/// (no full text, no embedding — those are fetched on demand via "Promote
/// to L3", same UX as cb-archive rows).
///
/// At-least-once semantics: `last_pull_ts` only advances after the
/// LanceDB writes succeed. A mid-apply crash will re-fetch the same rows
/// on the next pull (LanceDB add-with-existing-id is idempotent because
/// our `id = doc_id + ":" + chunk_index` row PK is stable).
#[tauri::command]
pub async fn sync_pull(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let (data_dir, remote_url, api_key, local) = {
        let dd  = state.data_dir.lock().await.clone();
        let idx = state.index.lock().await;
        let url = idx.config.remote_url.clone()
            .ok_or_else(|| "remote_url not configured".to_string())?;
        let key = idx.config.remote_api_key.clone().unwrap_or_default();
        let local = idx.local.clone();
        (dd, url, key, local)
    };
    let data_dir = data_dir.ok_or("data_dir not initialised")?;
    let local = local.ok_or("Local index not initialised — Hybrid mode requires a local cache")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;

    let (rows, max_ts) = mgr.pull_pending(&remote_url, &api_key, 200)
        .await
        .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        return Ok(serde_json::json!({ "pulled": 0, "applied": 0, "max_indexed_at": max_ts }));
    }

    // Convert each SearchHit → L1 DocumentChunk (chunk_index = -1 sentinel,
    // matching how the L1 ingest path / L2 fallback path build their rows).
    let chunks: Vec<crate::index::schema::DocumentChunk> = rows.iter()
        .map(|hit| {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let row_id = crate::index::ingest::chunk_row_id(&hit.doc_id, -1);
            crate::index::schema::DocumentChunk {
                id:                 row_id,
                doc_id:             hit.doc_id.clone(),
                location_uri:       hit.location_uri.clone(),
                owner_id:           hit.owner_id.clone(),
                filename:           hit.filename.clone(),
                title:              hit.title.clone(),
                author:             hit.author.clone(),
                year:               hit.year,
                ext:                hit.ext.clone(),
                language:           hit.language.clone(),
                page_count:         None,
                headings_text:      None,
                full_text:          None,
                full_text_md:       None,
                embedding:          None,
                embedding_sparse:   None,
                embedding_model:    None,
                chunk_index:        -1,
                chunk_total:        0,
                chunk_start_char:   None,
                chunk_end_char:     None,
                indexed_at:         now_ms,
                source_hash:        hit.doc_id.clone(),
                tags:               vec![],
                metadata_json:      Some(r#"{"level":1,"source":"sync_pull"}"#.to_owned()),
                parent_dir:         None,
                volume_id:          None,
                // Sync-pull L1 metadata rows don't carry translations.
                text_translated:    None,
                text_translated_lang: None,
                // L1 metadata-only — no audio probe data.  Step 8
                // promote can populate these on transcribe.
                audio_duration_seconds: None,
                audio_codec: None,
                audio_sample_rate_hz: None,
                audio_channels: None,
                audio_bitrate_kbps: None,
                image_camera_make: None,
                image_camera_model: None,
                image_lens_model: None,
                image_taken_at_unix: None,
                image_iso: None,
            }
        })
        .collect();

    let applied = chunks.len();
    local.ingest_batch(&chunks).await.map_err(|e| e.to_string())?;

    // Only after a successful apply do we advance the watermark.
    mgr.set_state("last_pull_ts", &max_ts.to_string()).map_err(|e| e.to_string())?;

    Ok(serde_json::json!({ "pulled": rows.len(), "applied": applied, "max_indexed_at": max_ts }))
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
