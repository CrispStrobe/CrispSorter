//! Tauri commands for the SyncManager (P11 Pillar 6) + cloud-backup
//! sync target (P13.7 Step 5).
//!
//! The P11 surface (`sync_status`/`sync_push`/`sync_pull`/`sync_enqueue`/
//! `sync_clear_failed`) talks to `crisp-index-server`.  The
//! cloud-backup surface (`sync_cb_*`) talks to the FastAPI module
//! shipped in `../../cloud-backup/api/app.py`, reusing the same
//! `sync_state` KV table for watermark persistence so the same
//! Settings panel can show "pulled 5 min ago" for both backends.

use tauri::State;
use crate::AppState;
use super::cloud_backup::{
    CloudBackupClient, EmbeddingRow, HealthResponse, ManifestRow,
};
use super::secret;
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

// ── P13.7 Step 5 — cloud-backup target commands ──────────────────────────
//
// Watermark keys in `sync_state` (the same KV table the P11 surface
// uses for `last_pull_ts` / `last_push_ts` against crisp-index-server):
//
//   "cb_last_manifest_push_ts"   — epoch-ms; advanced after a successful push
//   "cb_last_manifest_pull_ts"   — epoch-ms; max(indexed_at) observed on last pull
//   "cb_last_embeddings_push_ts" — epoch-ms; advanced after embeddings push
//
// Auth: the bearer token lives in the OS keychain under the
// `CrispSorter.CloudBackup` service (see `super::secret`) keyed by
// URL.  The token is set via `sync_cb_set_token` and never persisted
// to `index_config.json`.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudBackupStatus {
    pub configured: bool,
    pub url: String,
    pub token_present: bool,
    pub health: Option<HealthResponse>,
    pub last_manifest_push_ts: Option<i64>,
    pub last_manifest_pull_ts: Option<i64>,
    pub last_embeddings_push_ts: Option<i64>,
    pub pending_rows: i64,
    /// Diagnostic when health probe failed.  Empty on success.
    #[serde(default)]
    pub error: String,
}

/// Resolve the configured cloud-backup URL (returning empty string
/// when unset — same convention as the CrispLens commands).
async fn cb_url(state: &State<'_, AppState>) -> String {
    let idx = state.index.lock().await;
    idx.config.cloud_backup_url.clone().unwrap_or_default()
}

/// Read the in-memory `IndexConfig` and the on-disk keychain to
/// produce a snapshot.  Cheap; safe to call from the UI's status
/// poll loop.
#[tauri::command]
pub async fn sync_cb_status(
    state: State<'_, AppState>,
) -> Result<CloudBackupStatus, String> {
    let url = cb_url(&state).await;
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;

    if url.is_empty() {
        return Ok(CloudBackupStatus {
            configured: false,
            url: String::new(),
            token_present: false,
            health: None,
            last_manifest_push_ts: None,
            last_manifest_pull_ts: None,
            last_embeddings_push_ts: None,
            pending_rows: 0,
            error: String::new(),
        });
    }

    let token = match secret::get_token_for_url(&url) {
        Ok(t) => t,
        Err(e) => {
            return Ok(CloudBackupStatus {
                configured: true,
                url: url.clone(),
                token_present: false,
                health: None,
                last_manifest_push_ts: None,
                last_manifest_pull_ts: None,
                last_embeddings_push_ts: None,
                pending_rows: 0,
                error: format!("keychain: {e}"),
            });
        }
    };

    // Read state watermarks from the SyncManager KV.
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
    let push_ts = mgr.get_state("cb_last_manifest_push_ts").ok()
        .flatten().and_then(|s| s.parse::<i64>().ok());
    let pull_ts = mgr.get_state("cb_last_manifest_pull_ts").ok()
        .flatten().and_then(|s| s.parse::<i64>().ok());
    let emb_ts  = mgr.get_state("cb_last_embeddings_push_ts").ok()
        .flatten().and_then(|s| s.parse::<i64>().ok());

    // Probe health (5 s timeout in the client; never blocks the UI long).
    let mut health = None;
    let mut error = String::new();
    if let Some(ref t) = token {
        if let Ok(cli) = CloudBackupClient::new(&url, t) {
            match cli.health().await {
                Ok(h) => health = Some(h),
                Err(e) => error = format!("{e}"),
            }
        }
    }

    Ok(CloudBackupStatus {
        configured: true,
        url,
        token_present: token.is_some(),
        health,
        last_manifest_push_ts: push_ts,
        last_manifest_pull_ts: pull_ts,
        last_embeddings_push_ts: emb_ts,
        // Outbox depth is the crisp-index-server queue today; cloud-backup
        // is push-on-demand (not outboxed) so this is informational.
        pending_rows: mgr.pending_count().unwrap_or(0) as i64,
        error,
    })
}

/// Persist the bearer token for the configured URL in the OS
/// keychain.  Settings UI calls this on save; the value never
/// lands in `index_config.json`.
#[tauri::command]
pub async fn sync_cb_set_token(
    state: State<'_, AppState>,
    token: String,
) -> Result<(), String> {
    let url = cb_url(&state).await;
    if url.is_empty() {
        return Err("cloud_backup_url not configured".into());
    }
    if token.trim().is_empty() {
        // Empty token = clear (idempotent delete).
        secret::clear_token_for_url(&url).map_err(|e| e.to_string())?;
        return Ok(());
    }
    secret::set_token_for_url(&url, token.trim())
        .map_err(|e| e.to_string())
}

/// Wipe the stored token without touching the URL.  Used by the
/// Settings UI "Forget API key" affordance and by the CLI
/// `sync cloud-backup logout` subcommand.
#[tauri::command]
pub async fn sync_cb_clear_token(
    state: State<'_, AppState>,
) -> Result<(), String> {
    let url = cb_url(&state).await;
    if url.is_empty() {
        return Ok(()); // nothing to clear
    }
    secret::clear_token_for_url(&url).map_err(|e| e.to_string())
}

/// Build a [`CloudBackupClient`] from the current state.  Returns
/// an `Err` when the URL or token are missing — every push/pull
/// command threads through this so the failure mode is uniform.
async fn make_cb_client(state: &State<'_, AppState>) -> Result<CloudBackupClient, String> {
    let url = cb_url(state).await;
    if url.is_empty() {
        return Err("cloud_backup_url not configured — set it in Settings".into());
    }
    let token = secret::get_token_for_url(&url)
        .map_err(|e| format!("keychain: {e}"))?
        .ok_or_else(|| "no cloud-backup API key — call sync_cb_set_token first".to_string())?;
    CloudBackupClient::new(url, token).map_err(|e| e.to_string())
}

/// Walk the local index for rows newer than the
/// `cb_last_manifest_push_ts` watermark, batch them, and push to
/// `/api/manifest/push`.  Advances the watermark on success.
///
/// `limit` caps how many rows leave per invocation; callers can
/// loop on the return value's `pushed` field if they want to drain
/// the catalog fully.
#[tauri::command]
pub async fn sync_cb_manifest_push(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(200).clamp(1, 2000);
    let cli = make_cb_client(&state).await?;
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let local = {
        let idx = state.index.lock().await;
        idx.local.clone()
    };
    let local = local.ok_or("Local index not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;

    let last_ts: i64 = mgr.get_state("cb_last_manifest_push_ts").ok().flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Pull L1 rows from LocalIndex.  We use `list_documents` (only
    // chunk_index <= 0 rows) so duplicates per chunk don't end up
    // multiplying the push.  Then filter client-side to those with
    // indexed_at > last_ts.  At catalog scale (≤ 10⁵ docs) this is
    // fast; a future scalar-range pre-filter via list_failed
    // pattern would scale further but is out of scope here.
    let docs = local.list_documents(limit * 4).await.map_err(|e| e.to_string())?;
    let mut rows: Vec<ManifestRow> = Vec::new();
    let mut max_ts = last_ts;
    for d in docs {
        // metadata_json carries `fs_mtime` + `fs_size`; the schema
        // doesn't expose `indexed_at` on `SearchResult`, so we use
        // metadata_json's `indexed_at` as the watermark when
        // present, falling back to 0.
        let meta = d.metadata_json.as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let indexed_at = meta.get("indexed_at").and_then(|v| v.as_i64()).unwrap_or(0);
        if indexed_at <= last_ts { continue; }
        let fs_size = meta.get("fs_size").and_then(|v| v.as_i64()).unwrap_or(0);
        let fs_mtime = meta.get("fs_mtime").and_then(|v| v.as_f64())
            .or_else(|| meta.get("fs_mtime").and_then(|v| v.as_i64()).map(|i| i as f64))
            .unwrap_or(0.0);
        let parent_dir = meta.get("parent_dir").and_then(|v| v.as_str())
            .unwrap_or("").to_string();
        let path = meta.get("fs_path").and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| Some(d.location_uri.clone()))
            .unwrap_or_default();
        max_ts = max_ts.max(indexed_at);
        rows.push(ManifestRow {
            path,
            size_bytes: fs_size,
            sha256: meta.get("source_hash").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            mtime_unix: fs_mtime,
            owner_id: d.owner_id.clone(),
            filename: d.filename.clone().unwrap_or_default(),
            ext: d.ext.clone().unwrap_or_default(),
            parent_dir,
            language: d.language.clone(),
            title: d.title.clone(),
            author: d.author.clone(),
            year: d.year,
        });
        if rows.len() >= limit { break; }
    }

    if rows.is_empty() {
        return Ok(serde_json::json!({
            "pushed": 0, "accepted": 0,
            "watermark": last_ts, "more_available": false
        }));
    }

    let resp = cli.manifest_push(&rows).await.map_err(|e| e.to_string())?;
    let pushed = rows.len();
    if pushed > 0 {
        mgr.set_state("cb_last_manifest_push_ts", &max_ts.to_string())
            .map_err(|e| e.to_string())?;
    }
    Ok(serde_json::json!({
        "pushed": pushed,
        "accepted": resp.accepted,
        "watermark": max_ts,
        "more_available": pushed == limit
    }))
}

/// Pull rows newer than the local `cb_last_manifest_pull_ts`
/// watermark and write them as L1 metadata-only rows
/// (chunk_index = -1 sentinel) into the local LanceDB.  At-least-once
/// semantics: watermark advances only after the local writes succeed,
/// so a crash mid-apply re-fetches the same rows next time.
#[tauri::command]
pub async fn sync_cb_manifest_pull(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let limit = limit.unwrap_or(200).clamp(1, 2000);
    let cli = make_cb_client(&state).await?;
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let local = {
        let idx = state.index.lock().await;
        idx.local.clone()
    };
    let local = local.ok_or("Local index not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;

    let last_ts: i64 = mgr.get_state("cb_last_manifest_pull_ts").ok().flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let pulled = cli.manifest_pull(last_ts, limit).await
        .map_err(|e| e.to_string())?;

    if pulled.rows.is_empty() {
        return Ok(serde_json::json!({
            "pulled": 0, "applied": 0,
            "watermark": last_ts, "has_more": pulled.has_more
        }));
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;

    // Translate each PullRow → DocumentChunk (chunk_index = -1 L1 sentinel).
    let chunks: Vec<crate::index::schema::DocumentChunk> = pulled.rows.iter().map(|r| {
        // Build a stable doc_id from the hash so re-pulls of the same
        // file are idempotent (LanceDB upsert by row id).
        let doc_id = if r.sha256.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else { r.sha256.clone() };
        let row_id = crate::index::ingest::chunk_row_id(&doc_id, -1);
        crate::index::schema::DocumentChunk {
            id: row_id,
            doc_id: doc_id.clone(),
            // Use crisp+local:// for now; future improvement is to
            // emit crisp+cb-archive:// when archived_in is Some.
            location_uri: r.path.clone(),
            owner_id: r.owner_id.clone(),
            filename: Some(r.filename.clone()),
            title: r.title.clone(),
            author: r.author.clone(),
            year: r.year,
            ext: Some(r.ext.clone()),
            language: r.language.clone(),
            page_count: None,
            headings_text: None,
            full_text: None,
            full_text_md: None,
            embedding: None,
            embedding_sparse: None,
            embedding_model: None,
            chunk_index: -1,
            chunk_total: 0,
            chunk_start_char: None,
            chunk_end_char: None,
            indexed_at: now_ms,
            source_hash: r.sha256.clone(),
            tags: vec![],
            metadata_json: Some(format!(
                r#"{{"level":1,"source":"cb_sync_pull","cb_indexed_at":{}}}"#,
                r.indexed_at
            )),
            parent_dir: if r.parent_dir.is_empty() { None } else { Some(r.parent_dir.clone()) },
            volume_id: None,
            text_translated: None,
            text_translated_lang: None,
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
    }).collect();

    let applied = chunks.len();
    local.ingest_batch(&chunks).await.map_err(|e| e.to_string())?;

    let new_watermark = pulled.max_indexed_at;
    mgr.set_state("cb_last_manifest_pull_ts", &new_watermark.to_string())
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "pulled": pulled.rows.len(),
        "applied": applied,
        "watermark": new_watermark,
        "has_more": pulled.has_more,
    }))
}

/// `POST /api/index/push-embeddings` — push a caller-provided list
/// of embeddings.  The CLI surface walks the LocalIndex chunk
/// rows; this command is the low-level wire entrypoint exposed for
/// future GUI batch wiring + direct scripting.
#[tauri::command]
pub async fn sync_cb_embeddings_push(
    state: State<'_, AppState>,
    rows: Vec<EmbeddingRow>,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    let resp = cli.embeddings_push(&rows).await.map_err(|e| e.to_string())?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_millis() as i64;
    if resp.accepted > 0 {
        let data_dir = state.data_dir.lock().await.clone()
            .ok_or("data_dir not initialised")?;
        let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
        let _ = mgr.set_state("cb_last_embeddings_push_ts", &now_ms.to_string());
    }
    Ok(serde_json::json!({
        "accepted": resp.accepted,
        "rejected": resp.rejected,
        "errors":   resp.errors,
    }))
}
