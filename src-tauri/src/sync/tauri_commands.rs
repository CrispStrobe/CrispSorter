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
    AdminKeyInfo, AdminMintResponse, AdminRevokeResponse,
    CloudBackupClient, EmbeddingRow, FederatedHit, HealthResponse,
    HybridSearchFilters, HybridSearchRequest, ManifestRow,
};
use super::secret;
use super::{SyncManager, SyncStatus};

/// List persisted scheduled/local-cloud backup definitions.
#[tauri::command]
pub async fn backup_job_list(
    state: State<'_, AppState>,
) -> Result<Vec<crate::sync::backup_state::BackupJob>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    crate::sync::backup_state::BackupState::open(&data_dir)
        .map_err(|e| e.to_string())?
        .list_jobs()
        .map_err(|e| e.to_string())
}

/// Create or update backup configuration. This stores policy only; a future
/// scheduler owns execution and will record run status separately.
#[tauri::command]
pub async fn backup_job_upsert(
    state: State<'_, AppState>,
    job: crate::sync::backup_state::BackupJob,
) -> Result<crate::sync::backup_state::BackupJob, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    crate::sync::backup_state::BackupState::open(&data_dir)
        .map_err(|e| e.to_string())?
        .upsert_job(job)
        .map_err(|e| e.to_string())
}

/// Remove backup configuration without deleting local or remote data.
#[tauri::command]
pub async fn backup_job_delete(
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    crate::sync::backup_state::BackupState::open(&data_dir)
        .map_err(|e| e.to_string())?
        .delete_job(&id)
        .map_err(|e| e.to_string())
}

/// Return recent execution history for one backup job.
#[tauri::command]
pub async fn backup_job_runs(
    state: State<'_, AppState>,
    job_id: String,
    limit: Option<usize>,
) -> Result<Vec<crate::sync::backup_state::BackupRun>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    crate::sync::backup_state::BackupState::open(&data_dir)
        .map_err(|e| e.to_string())?
        .list_runs(&job_id, limit.unwrap_or(20).min(100))
        .map_err(|e| e.to_string())
}

/// Return enabled interval/daily jobs that are due at the current UTC time.
/// The scheduler owns execution; this command is intentionally read-only.
#[tauri::command]
pub async fn backup_job_due(
    state: State<'_, AppState>,
) -> Result<Vec<crate::sync::backup_state::BackupJob>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64).unwrap_or(0);
    crate::sync::backup_state::BackupState::open(&data_dir)
        .map_err(|e| e.to_string())?
        .due_jobs(now)
        .map_err(|e| e.to_string())
}

/// Return due job IDs and the next future scheduler wake-up time.
#[tauri::command]
pub async fn backup_job_scheduler_snapshot(
    state: State<'_, AppState>,
) -> Result<crate::sync::backup_scheduler::BackupSchedulerSnapshot, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64).unwrap_or(0);
    let store = crate::sync::backup_state::BackupState::open(&data_dir).map_err(|e| e.to_string())?;
    crate::sync::backup_scheduler::BackupScheduler::snapshot(&store, now)
        .map_err(|e| e.to_string())
}

/// Inventory files in a dated backup snapshot for GUI restore selection.
#[tauri::command]
pub async fn backup_job_snapshot_list(
    state: State<'_, AppState>,
    job_id: String,
    snapshot: String,
) -> Result<Vec<crate::sync::backup_state::BackupSnapshotEntry>, String> {
    if snapshot.len() != 10 || snapshot.as_bytes().get(4) != Some(&b'-')
        || snapshot.as_bytes().get(7) != Some(&b'-')
        || !snapshot.chars().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return Err("snapshot must be a YYYY-MM-DD directory name".into());
    }
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let job = crate::sync::backup_state::BackupState::open(&data_dir)
        .map_err(|e| e.to_string())?
        .job(&job_id).map_err(|e| e.to_string())?
        .ok_or_else(|| format!("backup job '{job_id}' not found"))?;
    let registry = crate::drives::DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = registry.drives.iter().find(|drive| drive.id == job.drive_id)
        .ok_or_else(|| format!("drive '{}' not found", job.drive_id))?;
    let drive = crate::drives::DriveRegistry::instantiate_with_proxy(
        config,
        &proxy_config(&state).await?,
    )
    .map_err(|e| e.to_string())?;
    if !drive.probed_capabilities().list { return Err("drive lacks list capability".into()); }
    let root = std::path::Path::new(&job.remote_root).join(&snapshot);
    let mut errors = Vec::new();
    let entries = crate::drives::walk(drive.as_ref(), &root, None, &mut |path, error| {
        errors.push(format!("{}: {error}", path.display()));
    });
    if !errors.is_empty() { return Err(errors.join("; ")); }
    Ok(entries.into_iter().map(|entry| crate::sync::backup_state::BackupSnapshotEntry {
        relative_path: entry.path.strip_prefix(&root).unwrap_or(&entry.path)
            .to_string_lossy().replace('\\', "/"),
        size: entry.size,
        mtime_unix: entry.mtime_unix,
    }).collect())
}

/// Restore one selected snapshot file to a local destination atomically.
#[tauri::command]
pub async fn backup_job_restore(
    state: State<'_, AppState>,
    job_id: String,
    snapshot: String,
    remote_path: String,
    destination: String,
) -> Result<serde_json::Value, String> {
    if snapshot.len() != 10 || snapshot.as_bytes().get(4) != Some(&b'-')
        || snapshot.as_bytes().get(7) != Some(&b'-')
        || !snapshot.chars().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    { return Err("snapshot must be a YYYY-MM-DD directory name".into()); }
    let relative = std::path::Path::new(&remote_path);
    use std::path::Component;
    if relative.is_absolute() || relative.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("remote path must be relative and contain no '.', '..', or root components".into());
    }
    if destination.trim().is_empty() { return Err("destination is required".into()); }
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let job = crate::sync::backup_state::BackupState::open(&data_dir)
        .map_err(|e| e.to_string())?.job(&job_id).map_err(|e| e.to_string())?
        .ok_or_else(|| format!("backup job '{job_id}' not found"))?;
    let registry = crate::drives::DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = registry.drives.iter().find(|drive| drive.id == job.drive_id)
        .ok_or_else(|| format!("drive '{}' not found", job.drive_id))?;
    let drive: std::sync::Arc<dyn crate::drives::CloudDrive> = std::sync::Arc::from(
        crate::drives::DriveRegistry::instantiate_with_proxy(
            config,
            &proxy_config(&state).await?,
        )
        .map_err(|e| e.to_string())?,
    );
    if !drive.probed_capabilities().read { return Err("drive lacks read capability".into()); }
    let remote = std::path::Path::new(&job.remote_root).join(&snapshot).join(relative);
    let expected = drive.stat(&remote).map_err(|e| format!("stat remote file: {e}"))?.size;
    let transfer = state.transfer_queue.clone().submit_drive_download(
        job.drive_id.clone(), drive, remote, Some(expected),
    );
    let data = transfer.handle.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    if data.len() as u64 != expected {
        return Err(format!("restore verification failed: expected {expected} bytes, got {}", data.len()));
    }
    let destination = std::path::PathBuf::from(destination);
    if let Some(parent) = destination.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
    let partial = destination.with_extension(format!("restore-partial-{}", std::process::id()));
    std::fs::write(&partial, &data).map_err(|e| e.to_string())?;
    std::fs::rename(&partial, &destination).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"restored": relative, "snapshot": snapshot, "destination": destination, "bytes": data.len()}))
}

/// List persisted local-folder ↔ cloud-drive sync pairs.
#[tauri::command]
pub async fn sync_pair_list(
    state: State<'_, AppState>,
) -> Result<Vec<super::pairs::SyncPair>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    super::pairs::SyncPairStore::open(&data_dir)
        .map_err(|e| e.to_string())?
        .list()
        .map_err(|e| e.to_string())
}

/// Create or update a sync-pair definition. Execution is owned by the future
/// sync runner; this command only persists validated configuration.
#[tauri::command]
pub async fn sync_pair_upsert(
    state: State<'_, AppState>,
    pair: super::pairs::SyncPair,
) -> Result<super::pairs::SyncPair, String> {
    if pair.id.trim().is_empty() || pair.local_root.trim().is_empty()
        || pair.drive_id.trim().is_empty() || pair.remote_root.trim().is_empty()
    {
        return Err("sync pair id, local root, drive id, and remote root are required".into());
    }
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    super::pairs::SyncPairStore::open(&data_dir)
        .map_err(|e| e.to_string())?
        .upsert(pair)
        .map_err(|e| e.to_string())
}

/// Remove a persisted sync-pair definition without touching either endpoint.
#[tauri::command]
pub async fn sync_pair_delete(
    state: State<'_, AppState>,
    id: String,
) -> Result<bool, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    super::pairs::SyncPairStore::open(&data_dir)
        .map_err(|e| e.to_string())?
        .delete(&id)
        .map_err(|e| e.to_string())
}

/// Preview the local side of a sync pair without contacting the provider or
/// advancing its watermark.
#[tauri::command]
pub async fn sync_pair_plan(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<super::pairs::SyncPlanEntry>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let pairs = super::pairs::SyncPairStore::open(&data_dir)
        .map_err(|e| e.to_string())?
        .list()
        .map_err(|e| e.to_string())?;
    let pair = pairs
        .into_iter()
        .find(|pair| pair.id == id)
        .ok_or_else(|| format!("sync pair '{id}' not found"))?;
    super::pairs::plan_local(&pair).map_err(|e| e.to_string())
}

/// Return recent explicit sync-pair runs for UI/CLI audit surfaces.
#[tauri::command]
pub async fn sync_pair_runs(
    state: State<'_, AppState>,
    id: String,
    limit: Option<usize>,
) -> Result<Vec<super::pairs::SyncPairRun>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    super::pairs::SyncPairStore::open(&data_dir)
        .map_err(|e| e.to_string())?
        .list_runs(&id, limit.unwrap_or(20))
        .map_err(|e| e.to_string())
}

/// Inventory remote files for conflict comparison. This performs listing and
/// metadata/stat calls only; it does not download or mutate provider data.
#[tauri::command]
pub async fn sync_pair_remote_plan(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<super::pairs::SyncRemoteEntry>, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let pair = super::pairs::SyncPairStore::open(&data_dir)
        .map_err(|e| e.to_string())?
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|pair| pair.id == id)
        .ok_or_else(|| format!("sync pair '{id}' not found"))?;
    let registry = crate::drives::DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = registry.drives.iter().find(|drive| drive.id == pair.drive_id)
        .ok_or_else(|| format!("drive '{}' not found", pair.drive_id))?;
    let drive = crate::drives::DriveRegistry::instantiate_with_proxy(
        config,
        &proxy_config(&state).await?,
    )
    .map_err(|e| e.to_string())?;
    if !drive.capabilities().list || !drive.capabilities().stat {
        return Err(format!("{} does not support remote inventory", drive.drive_type().label()));
    }
    let mut out = Vec::new();
    inventory_remote(
        &*drive,
        std::path::Path::new(&pair.remote_root),
        "",
        &pair.include_globs,
        &pair.exclude_globs,
        &mut out,
    )
    .map_err(|e| e.to_string())?;
    out.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(out)
}

/// Compare local and remote metadata under an explicit conflict policy.
#[tauri::command]
pub async fn sync_pair_compare(
    state: State<'_, AppState>,
    id: String,
    policy: super::conflict::ConflictPolicy,
) -> Result<Vec<super::pairs::SyncComparisonEntry>, String> {
    let local = sync_pair_plan(state.clone(), id.clone()).await?;
    let remote = sync_pair_remote_plan(state, id).await?;
    Ok(super::pairs::compare_plans(&local, &remote, policy))
}

pub(crate) fn inventory_remote(
    drive: &dyn crate::drives::CloudDrive,
    directory: &std::path::Path,
    relative_prefix: &str,
    includes: &[String],
    excludes: &[String],
    out: &mut Vec<super::pairs::SyncRemoteEntry>,
) -> anyhow::Result<()> {
    for entry in drive.list_dir(directory)? {
        let relative = if relative_prefix.is_empty() {
            entry.name.clone()
        } else {
            format!("{relative_prefix}/{}", entry.name)
        };
        let child = directory.join(&entry.name);
        if entry.is_dir {
            inventory_remote(drive, &child, &relative, includes, excludes, out)?;
        } else if super::pairs::filter_matches(&relative, includes, excludes) {
            let mtime_unix = drive.stat(&child)?.mtime_unix;
            out.push(super::pairs::SyncRemoteEntry {
                relative_path: relative,
                size: entry.size.unwrap_or(0),
                mtime_unix,
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncPairPushResult {
    pub pair_id: String,
    pub dry_run: bool,
    pub planned: usize,
    pub uploaded: usize,
    pub watermark: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncPairPullResult {
    pub pair_id: String,
    pub dry_run: bool,
    pub planned: usize,
    pub downloaded: usize,
    pub watermark: i64,
}

/// Pull remote files into the local side. This destructive direction requires
/// an explicit remote-wins policy and is limited to pairs that permit local
/// writes (`ToLocal` or `TwoWay`).
#[tauri::command]
pub async fn sync_pair_pull(
    state: State<'_, AppState>,
    id: String,
    dry_run: Option<bool>,
    conflict_policy: Option<super::conflict::ConflictPolicy>,
) -> Result<SyncPairPullResult, String> {
    if conflict_policy != Some(super::conflict::ConflictPolicy::RemoteWins) {
        return Err("sync pair pull requires remote-wins conflict policy".into());
    }
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let store = super::pairs::SyncPairStore::open(&data_dir).map_err(|e| e.to_string())?;
    let mut pair = store.list().map_err(|e| e.to_string())?.into_iter()
        .find(|pair| pair.id == id)
        .ok_or_else(|| format!("sync pair '{id}' not found"))?;
    if matches!(pair.mode, super::pairs::SyncPairMode::ToCloud) {
        return Err("sync pair is configured for local-to-remote direction".into());
    }
    let registry = crate::drives::DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let config = registry.drives.iter().find(|drive| drive.id == pair.drive_id)
        .ok_or_else(|| format!("drive '{}' not found", pair.drive_id))?;
    let drive: std::sync::Arc<dyn crate::drives::CloudDrive> = std::sync::Arc::from(
        crate::drives::DriveRegistry::instantiate_with_proxy(
            config,
            &proxy_config(&state).await?,
        )
        .map_err(|e| e.to_string())?,
    );
    if !drive.capabilities().read || !drive.capabilities().list || !drive.capabilities().stat {
        return Err(format!("{} does not support remote pull", drive.drive_type().label()));
    }
    let mut remote = Vec::new();
    inventory_remote(&*drive, std::path::Path::new(&pair.remote_root), "", &pair.include_globs, &pair.exclude_globs, &mut remote)
        .map_err(|e| e.to_string())?;
    remote.retain(|entry| entry.mtime_unix.map(|mtime| mtime >= pair.watermark).unwrap_or(true));
    remote.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    let dry_run = dry_run.unwrap_or(false);
    let started_at = sync_pair_now_ms();
    if dry_run || remote.is_empty() {
        let _ = store.record_run(&super::pairs::SyncPairRun {
            id: 0, pair_id: pair.id.clone(),
            status: if dry_run { "dry_run_pull" } else { "no_changes_pull" }.into(),
            planned: remote.len(), uploaded: 0, downloaded: 0, watermark: pair.watermark,
            error: None, started_at, finished_at: sync_pair_now_ms(),
        });
        return Ok(SyncPairPullResult { pair_id: pair.id, dry_run, planned: remote.len(), downloaded: 0, watermark: pair.watermark });
    }
    let mut downloaded = 0usize;
    let mut watermark = pair.watermark;
    for entry in &remote {
        let remote_path = std::path::PathBuf::from(format!("{}/{}", pair.remote_root.trim_end_matches('/'), entry.relative_path));
        let local_path = std::path::Path::new(&pair.local_root).join(&entry.relative_path);
        let transfer = state.transfer_queue.clone().submit_drive_download(
            pair.drive_id.clone(), drive.clone(), remote_path, Some(entry.size),
        );
        let bytes = match transfer.handle.await {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(error)) => return Err(error.to_string()),
            Err(error) => return Err(format!("sync transfer task failed: {error}")),
        };
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&local_path, bytes).map_err(|e| e.to_string())?;
        downloaded += 1;
        if let Some(mtime) = entry.mtime_unix { watermark = watermark.max(mtime); }
    }
    pair.watermark = watermark;
    store.upsert(pair).map_err(|e| e.to_string())?;
    let _ = store.record_run(&super::pairs::SyncPairRun {
        id: 0, pair_id: id.clone(), status: "completed_pull".into(), planned: remote.len(),
        uploaded: 0, downloaded, watermark, error: None, started_at, finished_at: sync_pair_now_ms(),
    });
    Ok(SyncPairPullResult { pair_id: id, dry_run: false, planned: remote.len(), downloaded, watermark })
}

/// Push the local side of a `ToCloud`/`TwoWay` pair through the shared
/// transfer queue. Existing remote entries are intentionally overwritten;
/// conflict-aware two-way comparison remains a separate PLAN item.
#[tauri::command]
pub async fn sync_pair_push(
    state: State<'_, AppState>,
    id: String,
    dry_run: Option<bool>,
    conflict_policy: Option<super::conflict::ConflictPolicy>,
) -> Result<SyncPairPushResult, String> {
    let data_dir = state.data_dir.lock().await.clone().ok_or("data_dir not initialised")?;
    let store = super::pairs::SyncPairStore::open(&data_dir).map_err(|e| e.to_string())?;
    let mut pair = store
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|pair| pair.id == id)
        .ok_or_else(|| format!("sync pair '{id}' not found"))?;
    if matches!(pair.mode, super::pairs::SyncPairMode::ToLocal) {
        return Err("sync pair is configured for remote-to-local direction".into());
    }
    if let Some(policy) = conflict_policy {
        if policy != super::conflict::ConflictPolicy::LocalWins {
            return Err(format!(
                "sync pair push cannot apply {policy:?} without remote metadata; use local-wins or run a remote comparison first"
            ));
        }
    }
    let plan = super::pairs::plan_local_since(&pair).map_err(|e| e.to_string())?;
    let dry_run = dry_run.unwrap_or(false);
    let started_at = sync_pair_now_ms();
    if dry_run || plan.is_empty() {
        let _ = store.record_run(&super::pairs::SyncPairRun {
            id: 0,
            pair_id: pair.id.clone(),
            status: if dry_run { "dry_run" } else { "no_changes" }.into(),
            planned: plan.len(),
            uploaded: 0,
            downloaded: 0,
            watermark: pair.watermark,
            error: None,
            started_at,
            finished_at: sync_pair_now_ms(),
        });
        return Ok(SyncPairPushResult {
            pair_id: pair.id,
            dry_run,
            planned: plan.len(),
            uploaded: 0,
            watermark: pair.watermark,
        });
    }

    let registry = match crate::drives::DriveRegistry::open(&data_dir) {
        Ok(registry) => registry,
        Err(error) => {
            let message = error.to_string();
            record_sync_pair_failure(&store, &pair.id, plan.len(), 0, pair.watermark, started_at, &message);
            return Err(message);
        }
    };
    let config = match registry.drives.iter().find(|drive| drive.id == pair.drive_id) {
        Some(config) => config,
        None => {
            let message = format!("drive '{}' not found", pair.drive_id);
            record_sync_pair_failure(&store, &pair.id, plan.len(), 0, pair.watermark, started_at, &message);
            return Err(message);
        }
    };
    let drive: std::sync::Arc<dyn crate::drives::CloudDrive> = std::sync::Arc::from(
        crate::drives::DriveRegistry::instantiate_with_proxy(
            config,
            &proxy_config(&state).await?,
        )
        .map_err(|e| e.to_string())?,
    );
    if !drive.capabilities().write {
        let message = format!("{} does not support uploads", drive.drive_type().label());
        record_sync_pair_failure(&store, &pair.id, plan.len(), 0, pair.watermark, started_at, &message);
        return Err(message);
    }

    let mut uploaded = 0usize;
    let mut watermark = pair.watermark;
    for entry in &plan {
        let local_path = std::path::Path::new(&pair.local_root).join(&entry.relative_path);
        let bytes = match std::fs::read(&local_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                let message = format!("reading {} for sync pair '{}': {error}", local_path.display(), pair.id);
                record_sync_pair_failure(&store, &pair.id, plan.len(), uploaded, watermark, started_at, &message);
                return Err(message);
            }
        };
        let remote_path = std::path::PathBuf::from(format!(
            "{}/{}",
            pair.remote_root.trim_end_matches('/'),
            entry.relative_path
        ));
        let retry_data = bytes.clone();
        let retry_path = remote_path.clone();
        let retry_drive_id = pair.drive_id.clone();
        let transfer = state.transfer_queue.clone().submit_drive_upload(
            pair.drive_id.clone(), drive.clone(), remote_path, bytes,
        );
        match transfer.handle.await {
            Ok(Ok(_)) => {
                uploaded += 1;
                watermark = watermark.max(entry.mtime_unix);
            }
            Ok(Err(error)) => {
                let message = error.to_string();
                let queued = queue_failed_drive_upload(
                    &data_dir,
                    &retry_drive_id,
                    &retry_path,
                    &retry_data,
                    &error,
                );
                let message = match queued {
                    Ok(id) => format!("{message} (queued offline operation {id})"),
                    Err(queue_error) => format!(
                        "{message} (offline queue failed: {queue_error})"
                    ),
                };
                record_sync_pair_failure(&store, &pair.id, plan.len(), uploaded, watermark, started_at, &message);
                return Err(message);
            }
            Err(error) => {
                let transfer_error = anyhow::anyhow!("sync transfer task failed: {error}");
                let message = match queue_failed_drive_upload(
                    &data_dir,
                    &retry_drive_id,
                    &retry_path,
                    &retry_data,
                    &transfer_error,
                ) {
                    Ok(id) => format!("{transfer_error} (queued offline operation {id})"),
                    Err(queue_error) => format!(
                        "{transfer_error} (offline queue failed: {queue_error})"
                    ),
                };
                record_sync_pair_failure(&store, &pair.id, plan.len(), uploaded, watermark, started_at, &message);
                return Err(message);
            }
        }
    }
    pair.watermark = watermark;
    if let Err(error) = store.upsert(pair) {
        let message = error.to_string();
        record_sync_pair_failure(&store, &id, plan.len(), uploaded, watermark, started_at, &message);
        return Err(message);
    }
    let _ = store.record_run(&super::pairs::SyncPairRun {
        id: 0,
        pair_id: id.clone(),
        status: "completed".into(),
        planned: plan.len(),
        uploaded,
        downloaded: 0,
        watermark,
        error: None,
        started_at,
        finished_at: sync_pair_now_ms(),
    });
    Ok(SyncPairPushResult {
        pair_id: id,
        dry_run: false,
        planned: plan.len(),
        uploaded,
        watermark,
    })
}

fn record_sync_pair_failure(
    store: &super::pairs::SyncPairStore,
    pair_id: &str,
    planned: usize,
    uploaded: usize,
    watermark: i64,
    started_at: i64,
    error: &str,
) {
    let _ = store.record_run(&super::pairs::SyncPairRun {
        id: 0,
        pair_id: pair_id.to_owned(),
        status: "failed".into(),
        planned,
        uploaded,
        downloaded: 0,
        watermark,
        error: Some(error.to_owned()),
        started_at,
        finished_at: sync_pair_now_ms(),
    });
}

fn sync_pair_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Return the persisted conflict policy used by future sync mutations.
#[tauri::command]
pub async fn sync_get_conflict_policy(
    state: State<'_, AppState>,
) -> Result<super::conflict::ConflictPolicy, String> {
    Ok(state.index.lock().await.config.conflict_policy)
}

/// Persist the conflict policy shared by sync and file-manager integrations.
#[tauri::command]
pub async fn sync_set_conflict_policy(
    state: State<'_, AppState>,
    policy: super::conflict::ConflictPolicy,
) -> Result<super::conflict::ConflictPolicy, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let mut index = state.index.lock().await;
    index.config.conflict_policy = policy;
    crate::index::config_persist::save(&data_dir, &index.config)
        .map_err(|e| format!("persisting conflict policy: {e}"))?;
    Ok(policy)
}

#[tauri::command]
pub async fn sync_list_conflicts(
    state: State<'_, AppState>,
) -> Result<Vec<super::PendingConflict>, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    SyncManager::open(&data_dir).map_err(|e| e.to_string())?
        .pending_conflicts().map_err(|e| e.to_string())
}

/// Acknowledge a manual-review item after the caller applies a resolution.
#[tauri::command]
pub async fn sync_ack_conflict(
    state: State<'_, AppState>, id: i64,
) -> Result<bool, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    SyncManager::open(&data_dir).map_err(|e| e.to_string())?
        .remove_conflict(id).map_err(|e| e.to_string())
}

/// Rehydrate and apply the exact remote candidate for a manual conflict.
/// The server lookup is path+hash scoped and does not advance the pull
/// watermark, so accepting one item cannot skip unrelated manifest rows.
#[tauri::command]
pub async fn sync_accept_remote_conflict(
    state: State<'_, AppState>,
    id: i64,
) -> Result<serde_json::Value, String> {
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
    let conflict = mgr.pending_conflicts().map_err(|e| e.to_string())?
        .into_iter().find(|item| item.id == id)
        .ok_or_else(|| format!("conflict {id} not found"))?;
    let cli = make_cb_client(&state).await?;
    let resolved = cli.manifest_resolve(&conflict.path, &conflict.remote_hash, true)
        .await.map_err(|e| e.to_string())?;
    let remote = resolved.rows.into_iter().next()
        .ok_or_else(|| "remote conflict candidate is no longer available".to_string())?;
    if remote.path != conflict.path || remote.sha256 != conflict.remote_hash {
        return Err("remote conflict candidate failed identity validation".into());
    }
    let local = state.index.lock().await.local.clone()
        .ok_or("Local index not initialised")?;
    for row in local.conflict_rows_for_location(&conflict.path).await
        .map_err(|e| e.to_string())? {
        local.delete_doc(&row.doc_id).await.map_err(|e| e.to_string())?;
    }
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_millis() as i64;
    let doc_id = remote.sha256.clone();
    let chunk = crate::index::schema::DocumentChunk {
        id: crate::index::ingest::chunk_row_id(&doc_id, -1),
        doc_id,
        location_uri: remote.path.clone(), owner_id: remote.owner_id.clone(),
        filename: Some(remote.filename.clone()), title: remote.title.clone(),
        author: remote.author.clone(), year: remote.year, ext: Some(remote.ext.clone()),
        language: remote.language.clone(), page_count: None, headings_text: None,
        full_text: remote.full_text.clone(), full_text_md: remote.full_text.clone(),
        embedding: None, embedding_sparse: None, embedding_model: None,
        chunk_index: -1, chunk_total: 0, chunk_start_char: None, chunk_end_char: None,
        indexed_at: now_ms, source_hash: remote.sha256.clone(), tags: remote.tags.clone(),
        metadata_json: Some(format!(r#"{{"level":1,"source":"cb_conflict_accept","cb_indexed_at":{}}}"#, remote.indexed_at)),
        parent_dir: if remote.parent_dir.is_empty() { None } else { Some(remote.parent_dir.clone()) },
        volume_id: None, text_translated: None, text_translated_lang: None,
        audio_duration_seconds: None, audio_codec: None, audio_sample_rate_hz: None,
        audio_channels: None, audio_bitrate_kbps: None, image_camera_make: None,
        image_camera_model: None, image_lens_model: None, image_taken_at_unix: None,
        image_iso: None, multivec_packed: None, multivec_n_tokens: None,
        url: remote.url.clone(), embedding_omni: None, embedding_vit: None,
        summary: None, doc_status: None,
    };
    local.ingest_batch(&[chunk]).await.map_err(|e| e.to_string())?;
    mgr.remove_conflict(id).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"id": id, "path": conflict.path, "sha256": conflict.remote_hash, "accepted": true}))
}

async fn offline_queue(
    state: &State<'_, AppState>,
) -> Result<super::offline_queue::OfflineQueue, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    super::offline_queue::OfflineQueue::open(&data_dir).map_err(|e| e.to_string())
}

/// Add a replayable provider operation. The payload remains opaque JSON so
/// provider-specific resume state can evolve independently.
#[tauri::command]
pub async fn sync_offline_enqueue(
    state: State<'_, AppState>,
    op_type: String,
    payload: String,
    provider_id: String,
) -> Result<i64, String> {
    offline_queue(&state)
        .await?
        .enqueue(&op_type, &payload, &provider_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sync_offline_cancel(
    state: State<'_, AppState>,
    id: i64,
) -> Result<bool, String> {
    offline_queue(&state).await?.cancel(id).map_err(|e| e.to_string())
}

/// Snapshot the shared application transfer queue for the transfer drawer,
/// status bar, and headless UI integrations.
#[tauri::command]
pub async fn transfer_queue_status(
    state: State<'_, AppState>,
) -> Result<Vec<super::transfer_queue::TransferProgress>, String> {
    Ok(state.transfer_queue.snapshot())
}

/// Cancel a queued or active shared transfer by its job ID.
#[tauri::command]
pub async fn transfer_queue_cancel(
    state: State<'_, AppState>,
    job_id: u64,
) -> Result<bool, String> {
    Ok(state.transfer_queue.cancel(job_id))
}

/// Inspect the durable cloud-operation offline queue.
#[tauri::command]
pub async fn offline_queue_status(
    state: State<'_, AppState>,
) -> Result<super::offline_queue::QueueStats, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    super::offline_queue::OfflineQueue::open(&data_dir)
        .map_err(|e| e.to_string())?
        .stats()
        .map_err(|e| e.to_string())
}

/// Return persisted offline operations in FIFO order, including diagnostics.
#[tauri::command]
pub async fn offline_queue_list(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<super::offline_queue::QueuedOp>, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    super::offline_queue::OfflineQueue::open(&data_dir)
        .map_err(|e| e.to_string())?
        .list()
        .map(|ops| ops.into_iter().take(limit.unwrap_or(100).min(1000)).collect())
        .map_err(|e| e.to_string())
}

/// Reset permanently failed offline operations for another replay attempt.
#[tauri::command]
pub async fn offline_queue_retry_failed(
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    super::offline_queue::OfflineQueue::open(&data_dir)
        .map_err(|e| e.to_string())?
        .retry_all_failed()
        .map_err(|e| e.to_string())
}

/// Purge failed offline operations older than the requested number of days.
#[tauri::command]
pub async fn offline_queue_purge_failed(
    state: State<'_, AppState>,
    older_than_days: u64,
) -> Result<usize, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    super::offline_queue::OfflineQueue::open(&data_dir)
        .map_err(|e| e.to_string())?
        .purge_old(std::time::Duration::from_secs(older_than_days.saturating_mul(86_400)))
        .map_err(|e| e.to_string())
}

/// Persist the bytes for a failed drive write and enqueue a replay descriptor.
pub(crate) fn queue_failed_drive_upload(
    data_dir: &std::path::Path,
    drive_id: &str,
    remote_path: &std::path::Path,
    data: &[u8],
    error: &anyhow::Error,
) -> anyhow::Result<i64> {
    let staging = data_dir.join("offline_transfers");
    std::fs::create_dir_all(&staging)?;
    let id = uuid::Uuid::new_v4().to_string();
    let local_path = staging.join(format!("{id}.bin"));
    std::fs::write(&local_path, data)?;
    let payload = serde_json::json!({
        "drive_id": drive_id,
        "remote_path": remote_path,
        "local_path": local_path,
        "error": format!("{error:#}"),
    });
    super::offline_queue::OfflineQueue::open(data_dir)?.enqueue(
        "drive_upload",
        &payload.to_string(),
        drive_id,
    )
}

/// Replay pending staged drive uploads through the shared transfer queue.
#[tauri::command]
pub async fn offline_queue_replay(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    replay_offline_queue(&state, limit.unwrap_or(20).min(100)).await
}

/// Replay pending staged operations from the background maintenance loop.
pub async fn replay_offline_queue(
    state: &AppState,
    limit: usize,
) -> Result<serde_json::Value, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not initialised")?;
    let queue = super::offline_queue::OfflineQueue::open(&data_dir).map_err(|e| e.to_string())?;
    let entries = queue
        .dequeue_batch(limit)
        .map_err(|e| e.to_string())?;
    let registry = crate::drives::DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let mut replayed = 0usize;
    let mut failed = 0usize;
    for entry in entries {
        if entry.op_type != "drive_upload" {
            // Never silently drop a queued operation that this build cannot
            // replay.  Keeping it pending would make the maintenance loop
            // select it forever; recording a terminal failure preserves the
            // payload for inspection and lets the user retry after upgrading
            // to a build that understands the operation.
            let error = format!("unsupported offline operation type: {}", entry.op_type);
            queue.mark_failed(entry.id, &error).map_err(|e| e.to_string())?;
            failed += 1;
            continue;
        }
        let result: Result<(), String> = async {
            let payload: serde_json::Value = serde_json::from_str(&entry.payload)
                .map_err(|e| format!("invalid offline payload: {e}"))?;
            let drive_id = payload["drive_id"].as_str().ok_or("offline payload missing drive_id")?;
            let remote_path = payload["remote_path"].as_str().ok_or("offline payload missing remote_path")?;
            let local_path = payload["local_path"].as_str().ok_or("offline payload missing local_path")?;
            let config = registry.drives.iter().find(|d| d.id == drive_id)
                .ok_or_else(|| format!("drive '{drive_id}' no longer exists"))?;
            let drive: std::sync::Arc<dyn crate::drives::CloudDrive> =
                std::sync::Arc::from(
                    crate::drives::DriveRegistry::instantiate_with_proxy(
                        config,
                        &proxy_config_for_app_state(state).await?,
                    )
                    .map_err(|e| e.to_string())?,
                );
            let bytes = std::fs::read(local_path).map_err(|e| e.to_string())?;
            let transfer = state.transfer_queue.clone().submit_upload(
                drive_id.to_owned(),
                std::path::PathBuf::from(remote_path),
                bytes,
                move |path, data| drive.write_file(path, data),
            );
            match transfer.handle.await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        .await;
        match result {
            Ok(()) => {
                let payload: serde_json::Value = serde_json::from_str(&entry.payload)
                    .map_err(|e| e.to_string())?;
                if let Some(local_path) = payload["local_path"].as_str() {
                    let _ = std::fs::remove_file(local_path);
                }
                queue.mark_done(entry.id).map_err(|e| e.to_string())?;
                replayed += 1;
            }
            Err(error) => {
                queue.mark_failed(entry.id, &error).map_err(|e| e.to_string())?;
                failed += 1;
            }
        }
    }
    Ok(serde_json::json!({ "replayed": replayed, "failed": failed }))
}

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
                multivec_packed: None,
                multivec_n_tokens: None,
                url: None,
                embedding_omni: None,
                embedding_vit: None,
                summary: None,
                doc_status: None,
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
        match proxy_config(&state).await.and_then(|proxy| {
            CloudBackupClient::new_with_proxy(&url, t, &proxy).map_err(|e| e.to_string())
        }) {
            Ok(cli) => match cli.health().await {
                Ok(h) => health = Some(h),
                Err(e) => error = format!("{e}"),
            },
            Err(e) => error = e,
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

/// Store the proxy password in the OS keychain; an empty value clears it.
#[tauri::command]
pub async fn proxy_set_password(password: String) -> Result<(), String> {
    if password.trim().is_empty() {
        crate::sync::proxy_secret::clear().map_err(|e| e.to_string())
    } else {
        crate::sync::proxy_secret::set(&password).map_err(|e| e.to_string())
    }
}

/// Make a bounded request through the configured proxy. This validates both
/// URL/auth parsing and reachability without exposing the password to the UI.
#[tauri::command]
pub async fn proxy_test_connection(state: State<'_, AppState>) -> Result<String, String> {
    let config = state.index.lock().await.config.clone();
    let proxy = crate::sync::proxy::ProxyConfig {
        url: config.proxy_url,
        username: config.proxy_username,
        password: crate::sync::proxy_secret::get().map_err(|e| e.to_string())?,
    };
    if proxy.url.is_none() {
        return Err("proxy URL is not configured".into());
    }
    let client = crate::sync::proxy::build_async_client_with_timeout(
        &proxy,
        std::time::Duration::from_secs(10),
    )
    .map_err(|e| e.to_string())?;
    let response = client
        .head("https://www.google.com")
        .send()
        .await
        .map_err(|e| format!("proxy connection failed: {e}"))?;
    Ok(format!("Proxy connection succeeded (HTTP {})", response.status()))
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
    let proxy = proxy_config(state).await?;
    CloudBackupClient::new_with_proxy(url, token, &proxy).map_err(|e| e.to_string())
}

/// Resolve the shared proxy policy from the persisted index settings and
/// keychain-backed password.  The password never crosses the frontend IPC
/// boundary or enters serialized configuration.
async fn proxy_config(state: &State<'_, AppState>) -> Result<crate::sync::proxy::ProxyConfig, String> {
    proxy_config_for_app_state(&*state).await
}

pub(crate) async fn proxy_config_for_app_state(
    state: &AppState,
) -> Result<crate::sync::proxy::ProxyConfig, String> {
    let config = state.index.lock().await.config.clone();
    Ok(crate::sync::proxy::ProxyConfig {
        url: config.proxy_url,
        username: config.proxy_username,
        password: crate::sync::proxy_secret::get().map_err(|e| e.to_string())?,
    })
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

    // Pull push-candidates from LocalIndex via the dedicated
    // projection (Stage A — full_text included).  Pre-filters
    // `indexed_at > last_ts` server-side; we still cap rows
    // client-side at `limit`.
    let candidates = local.list_documents_for_push(last_ts, limit)
        .await.map_err(|e| e.to_string())?;
    let mut rows: Vec<ManifestRow> = Vec::new();
    let mut max_ts = last_ts;
    for c in &candidates {
        // metadata_json may carry the original filesystem path /
        // size / mtime — lift them when present (matches the
        // historical L1FileEntry shape).
        let meta = c.metadata_json.as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let fs_size = meta.get("fs_size").and_then(|v| v.as_i64()).unwrap_or(0);
        let fs_mtime = meta.get("fs_mtime").and_then(|v| v.as_f64())
            .or_else(|| meta.get("fs_mtime").and_then(|v| v.as_i64()).map(|i| i as f64))
            .unwrap_or(0.0);
        let path = meta.get("fs_path").and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| c.location_uri.clone());
        max_ts = max_ts.max(c.indexed_at);
        rows.push(ManifestRow {
            path,
            size_bytes: fs_size,
            sha256: c.source_hash.clone(),
            mtime_unix: fs_mtime,
            owner_id: c.owner_id.clone(),
            filename: c.filename.clone().unwrap_or_default(),
            ext: c.ext.clone().unwrap_or_default(),
            parent_dir: c.parent_dir.clone().unwrap_or_default(),
            language: c.language.clone(),
            title: c.title.clone(),
            author: c.author.clone(),
            year: c.year,
            full_text: c.full_text.clone(),
            collection_id: c.collection_id.clone(),
            archived_in: None,
            url: None,
            tags: vec![],
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
    let (local, include_full_text, conflict_policy) = {
        let idx = state.index.lock().await;
        (idx.local.clone(), idx.config.cloud_backup_pull_full_text_enabled,
            idx.config.conflict_policy)
    };
    let local = local.ok_or("Local index not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;

    let last_ts: i64 = mgr.get_state("cb_last_manifest_pull_ts").ok().flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // Tiered-cache model: default metadata-only pull.  Users who
    // want offline FTS over the full VPS catalog flip the
    // `cloud_backup_pull_full_text_enabled` Settings flag to opt
    // into body-text hydration on every pull.
    let pulled = cli.manifest_pull_with_options(last_ts, limit, include_full_text)
        .await
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

    // Translate each PullRow → DocumentChunk, resolving same-path changes
    // before they reach LanceDB.
    let mut chunks = Vec::with_capacity(pulled.rows.len());
    let mut skipped_local = 0usize;
    let mut queued_manual = 0usize;
    let mut replaced_remote = 0usize;
    let mut kept_both = 0usize;
    for r in &pulled.rows {
        // Build a stable doc_id from the hash so re-pulls of the same
        // file are idempotent (LanceDB upsert by row id).
        let base_doc_id = if r.sha256.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else { r.sha256.clone() };
        let local_rows = local.conflict_rows_for_location(&r.path)
            .await.map_err(|e| e.to_string())?;
        let matching = local_rows.iter().any(|row| row.source_hash == r.sha256);
        let mut doc_id = base_doc_id;
        if !matching {
            if let Some(local_row) = local_rows.first() {
                let resolution = crate::sync::conflict::resolve_conflict(
                    &crate::sync::conflict::ConflictSide {
                        doc_id: local_row.doc_id.clone(), source_hash: local_row.source_hash.clone(),
                        updated_at: local_row.indexed_at, title: local_row.title.clone(),
                    },
                    &crate::sync::conflict::ConflictSide {
                        doc_id: doc_id.clone(), source_hash: r.sha256.clone(),
                        updated_at: Some(r.indexed_at), title: r.title.clone(),
                    }, conflict_policy);
                match resolution {
                    crate::sync::conflict::Resolution::UseLocal => {
                        skipped_local += 1;
                        continue;
                    }
                    crate::sync::conflict::Resolution::UseRemote => {
                        for row in &local_rows {
                            local.delete_doc(&row.doc_id).await.map_err(|e| e.to_string())?;
                        }
                        replaced_remote += 1;
                    }
                    crate::sync::conflict::Resolution::KeepBoth { remote_doc_id } => {
                        doc_id = remote_doc_id;
                        kept_both += 1;
                    }
                    crate::sync::conflict::Resolution::NeedsManualReview => {
                        mgr.enqueue_conflict(&r.path, &local_row.doc_id, &local_row.source_hash,
                            &r.sha256, local_row.title.as_deref(), r.title.as_deref(), r.indexed_at)
                            .map_err(|e| e.to_string())?;
                        queued_manual += 1;
                        continue;
                    }
                }
            }
        }
        let row_id = crate::index::ingest::chunk_row_id(&doc_id, -1);
        chunks.push(crate::index::schema::DocumentChunk {
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
            // Stage A — copy body text from the server response so a
            // subsequent `crispsorter index search` finds remote rows
            // by body content, not just metadata.  `full_text_md`
            // mirrors `full_text` (no Markdown roundtrip across the
            // wire — same convention as audio L2 rows).
            full_text: r.full_text.clone(),
            full_text_md: r.full_text.clone(),
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
            multivec_packed: None,
            multivec_n_tokens: None,
            url: None,
            embedding_omni: None,
            embedding_vit: None,
            summary: None,
            doc_status: None,
        });
    }

    let applied = chunks.len();
    local.ingest_batch(&chunks).await.map_err(|e| e.to_string())?;

    let new_watermark = pulled.max_indexed_at;
    mgr.set_state("cb_last_manifest_pull_ts", &new_watermark.to_string())
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "pulled": pulled.rows.len(),
        "applied": applied,
        "skipped_local": skipped_local,
        "queued_manual": queued_manual,
        "replaced_remote": replaced_remote,
        "kept_both": kept_both,
        "watermark": new_watermark,
        "has_more": pulled.has_more,
    }))
}

/// Stage N — recompute the volume-proportional partition map for
/// `root_path`.  Walks the local LanceDB for L1 rows under the
/// root, sums sizes per group (depth-N path prefix), allocates
/// shards proportional to volume capped at `max_shards`, writes
/// the per-file `collection_id` assignments to the persistent
/// partition map at `<data-dir>/partition_map.db`.
///
/// Returns `{root, num_files, num_shards, sample_collections}`.
/// The next `sync_cb_manifest_push` (auto or manual) picks up the
/// new assignments via the bg_ingest lookup hook.
#[tauri::command]
pub async fn sync_cb_partition(
    state: State<'_, AppState>,
    root_path: String,
    max_shards: Option<usize>,
    group_depth: Option<usize>,
) -> Result<serde_json::Value, String> {
    use crate::sync::partition::{
        partition_assignments, FileSize, PartitionMap, PartitionOptions,
    };

    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let local = {
        let idx = state.index.lock().await;
        idx.local.clone()
    };
    let local = local.ok_or("Local index not initialised")?;

    let root_path = std::path::PathBuf::from(&root_path);
    // Scan all documents — partition is a periodic full-scan, not
    // incremental.  At catalog scale (≤ 10M docs) this is one
    // long-ish LanceDB query but fine to run on a "re-partition"
    // button.
    let candidates = local.list_documents_for_push(0, 10_000_000)
        .await.map_err(|e| e.to_string())?;

    // Filter to rows whose `path` is under the given root, and lift
    // (path, size) tuples for the algorithm.  Size comes from the
    // metadata_json `fs_size` field; missing → 0 (still counted,
    // just doesn't tilt the partition).
    let mut files: Vec<FileSize> = Vec::new();
    for c in &candidates {
        let meta = c.metadata_json.as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let fs_size = meta.get("fs_size").and_then(|v| v.as_i64()).unwrap_or(0) as u64;
        // Path resolution: prefer the fs_path metadata; fall back
        // to location_uri stripped of the scheme.
        let raw_path = meta.get("fs_path").and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| c.location_uri.clone());
        let file_path = std::path::PathBuf::from(&raw_path);
        if file_path.starts_with(&root_path) {
            files.push(FileSize { path: file_path, size: fs_size });
        }
    }

    let opts = PartitionOptions {
        max_shards: max_shards.unwrap_or(64).max(1),
        group_depth: group_depth.unwrap_or(1).max(1),
        min_fraction: 0.25,
    };
    let assignments = partition_assignments(&root_path, &files, &opts);
    let num_files = assignments.len();
    let num_shards = assignments.iter()
        .map(|a| a.collection_id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let sample: Vec<String> = assignments.iter()
        .take(8)
        .map(|a| a.collection_id.clone())
        .collect();

    // Persist.
    let map = PartitionMap::open(&data_dir).map_err(|e| e.to_string())?;
    map.write_batch(&assignments).map_err(|e| e.to_string())?;
    map.record_run(&root_path, num_files, num_shards, &opts)
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "root":                root_path.display().to_string(),
        "num_files":           num_files,
        "num_shards":          num_shards,
        "max_shards":          opts.max_shards,
        "group_depth":         opts.group_depth,
        "sample_collections":  sample,
    }))
}

/// `POST /api/v2/index/search` — hybrid LanceDB search across
/// every shard on the VPS.  Combines metadata filters + FTS over
/// `full_text` + vector k-NN (either client-supplied `vec` or
/// server-side `embed_text` inference).  This is the GUI's
/// "search remote" entry point when local cache misses warrant
/// escalation to the full corpus.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V2SearchParams {
    #[serde(default)] pub q: Option<String>,
    #[serde(default)] pub vec: Option<Vec<f32>>,
    #[serde(default)] pub embed_text: Option<String>,
    #[serde(default)] pub embed_model: Option<String>,
    #[serde(default)] pub filters: HybridSearchFilters,
    #[serde(default = "default_v2_limit")] pub limit: usize,
    #[serde(default = "default_rrf_k")]    pub rrf_k: usize,
}
fn default_v2_limit() -> usize { 50 }
fn default_rrf_k() -> usize { 60 }

#[tauri::command]
pub async fn sync_cb_v2_search(
    state: State<'_, AppState>,
    params: V2SearchParams,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    let req = HybridSearchRequest {
        q:           params.q.as_deref(),
        vec:         params.vec.as_deref(),
        embed_text:  params.embed_text.as_deref(),
        embed_model: params.embed_model.as_deref(),
        filters:     params.filters,
        limit:       params.limit.clamp(1, 500),
        rrf_k:       params.rrf_k,
    };
    let resp = cli.v2_search(&req).await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "rows":           resp.rows,
        "total":          resp.total,
        "used_text":      resp.used_text,
        "used_vector":    resp.used_vector,
        "shards_queried": resp.shards_queried,
    }))
}

/// `GET /api/index/embed-query?text=…&model=…` — compute the
/// embedding vector for `text` on the cloud-backup VPS (CPU
/// inference via fastembed).  Lets clients without a local
/// embedder (phone / web / headless) feed the returned vector
/// into `/api/index/by-embedding` for k-NN.
#[tauri::command]
pub async fn sync_cb_embed_query(
    state: State<'_, AppState>,
    text: String,
    model: Option<String>,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    let resp = cli.embed_query(&text, model.as_deref())
        .await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "model":     resp.model,
        "dim":       resp.dim,
        "embedding": resp.embedding,
    }))
}

/// `GET /api/index/embed-models` — list models the VPS embedder
/// supports + whether fastembed is installed at all.  Used by the
/// UI to decide whether to offer "embed remotely" as an option.
#[tauri::command]
pub async fn sync_cb_embed_models(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    let resp = cli.embed_models().await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "models":    resp.models,
        "default":   resp.default,
        "available": resp.available,
    }))
}

/// P13.7 Stage F — drain `cb_manifest_push` entries from the
/// sync_outbox by POSTing batches to `/api/manifest/push`.  Manual
/// trigger (CLI + Settings button); a background timer in lib.rs
/// setup runs it periodically when auto-push is on.  Returns
/// `{pushed, failed}` — pushed is the count of outbox entries
/// successfully drained; failed is mid-batch errors (retries bump).
#[tauri::command]
pub async fn sync_cb_drain(
    state: State<'_, AppState>,
    batch_size: Option<usize>,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let mgr = SyncManager::open(&data_dir).map_err(|e| e.to_string())?;
    let n = batch_size.unwrap_or(64).clamp(1, 1024);
    let (pushed, failed) = mgr.drain_cb_outbox(&cli, n).await
        .map_err(|e| e.to_string())?;
    // Stage U — also drain file uploads in the same call.
    let thin_enabled = !state.index.lock().await.config.local_extraction_enabled;
    let (uploaded, upload_failed) = if thin_enabled {
        mgr.drain_cb_file_uploads(&cli, n).await.unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    Ok(serde_json::json!({
        "pushed":         pushed,
        "failed":         failed,
        "uploaded":       uploaded,
        "upload_failed":  upload_failed,
        "drained":        pushed + uploaded,
    }))
}

/// `POST /api/files/by-hash/<sha>` — stream-upload `local_path` to
/// the cloud-backup VPS.  Server verifies the hash; idempotent on
/// re-upload.  Owner-scope: the caller must have a manifest row
/// referencing this sha (push via `sync_cb_manifest_push` first).
#[tauri::command]
pub async fn sync_cb_upload_file(
    state: State<'_, AppState>,
    sha256: String,
    local_path: String,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    let path = std::path::PathBuf::from(&local_path);
    if !path.exists() {
        return Err(format!("file not found: {local_path}"));
    }
    let resp = cli.upload_file_by_hash(&sha256, &path)
        .await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "sha256":          resp.sha256,
        "size_bytes":      resp.size_bytes,
        "stored":          resp.stored,
        "local_blob_path": resp.local_blob_path,
    }))
}

/// `GET /api/files/by-hash/<sha>` — stream bytes to `dest_path`
/// with sha-verify on the fly.  Returns the byte count written.
#[tauri::command]
pub async fn sync_cb_download_file(
    state: State<'_, AppState>,
    sha256: String,
    dest_path: String,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    let dest = std::path::PathBuf::from(&dest_path);
    // Create the parent dir if needed (matches the `tauri-plugin-fs`
    // convention; a missing parent dir would otherwise produce an
    // opaque "no such file or directory" error from File::create).
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let bytes = cli.download_file_by_hash(&sha256, &dest)
        .await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "sha256":     sha256,
        "dest_path":  dest_path,
        "bytes":      bytes,
    }))
}

/// `GET /api/search?q=&limit=` — server-side FTS5 query over
/// `file_references.full_text`.  Returns rows in the same shape
/// `sync_cb_manifest_pull` does; the GUI can lift a hit straight
/// into the local L1 store via the same DocumentChunk adapter.
#[tauri::command]
pub async fn sync_cb_search(
    state: State<'_, AppState>,
    q: String,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    // include_full_text=true: this command's hits can be lifted straight
    // into the local L1 store, where the body is the ingest payload.
    let resp = cli.search(&q, limit.unwrap_or(50).clamp(1, 500), true)
        .await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "rows":  resp.rows,
        "total": resp.total,
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

/// Stage O — unified backend health snapshot.  Polls cb-api,
/// CrispLens, and crisp-index-server in parallel, returns one
/// combined JSON object with per-backend reachability + auth state +
/// last-sync timestamps.  Individual backend failures are captured in
/// the response rather than propagated as errors so the GUI banner
/// can show a partial-degraded state.
#[tauri::command]
pub async fn sync_status_all(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    use tokio::join;

    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;

    // Snapshot config fields we need for the cb-api and crisp-index-server
    // probes, then drop the lock.  CrispLens settings live in their own
    // store (read inside status_blocking).
    let (remote_url, cb_url_val) = {
        let idx = state.index.lock().await;
        (
            idx.config.remote_url.clone(),
            idx.config.cloud_backup_url.clone().unwrap_or_default(),
        )
    };
    let proxy = proxy_config_for_app_state(&state).await?;

    // Probe cb-api.
    let cb_probe = async {
        if cb_url_val.is_empty() {
            return serde_json::json!({"configured": false});
        }
        let token = match super::secret::get_token_for_url(&cb_url_val) {
            Ok(Some(t)) => t,
            Ok(None)    => return serde_json::json!({"configured": true, "token_present": false}),
            Err(e)      => return serde_json::json!({"configured": true, "error": e.to_string()}),
        };
        match CloudBackupClient::new_with_proxy(&cb_url_val, &token, &proxy) {
            Ok(cli) => match cli.health().await {
                Ok(h)  => serde_json::json!({
                    "configured": true,
                    "token_present": true,
                    "ok": h.ok,
                    "version": h.version,
                    "lance_enabled": h.lance_enabled,
                }),
                Err(e) => serde_json::json!({"configured": true, "token_present": true, "ok": false, "error": e.to_string()}),
            },
            Err(e) => serde_json::json!({"configured": true, "error": e.to_string()}),
        }
    };

    // Probe crisp-index-server.
    let cis_probe = async {
        let url_str = remote_url.as_deref().unwrap_or("");
        if url_str.is_empty() {
            return serde_json::json!({"configured": false});
        }
        let online = SyncManager::is_remote_online(url_str).await;
        let mgr_res = SyncManager::open(&data_dir);
        let (push_ts, pull_ts) = mgr_res.map(|m| (
            m.get_state("last_push_ts").ok().flatten().and_then(|s| s.parse::<i64>().ok()),
            m.get_state("last_pull_ts").ok().flatten().and_then(|s| s.parse::<i64>().ok()),
        )).unwrap_or((None, None));
        serde_json::json!({
            "configured": true,
            "ok": online,
            "last_push_ts": push_ts,
            "last_pull_ts": pull_ts,
        })
    };

    // Probe CrispLens (blocking call, spawn to avoid blocking the async runtime).
    // status_blocking handles the "not configured" case internally.
    //
    // Behind `images-crisplens`: with the feature off there is no client to
    // probe, and the wire shape stays identical — `configured: false` is
    // already what an unconfigured install reports, so the UI needs no second
    // code path for "not compiled in". See PLAN.md P35.4.
    #[cfg(not(feature = "images-crisplens"))]
    let cl_probe = async { serde_json::json!({"configured": false}) };
    #[cfg(feature = "images-crisplens")]
    let cl_probe = {
        let data_dir2 = data_dir.clone();
        async move {
            let status = tauri::async_runtime::spawn_blocking(move || {
                crate::images::crisplens::tauri_commands::status_blocking(&data_dir2)
            }).await;
            match status {
                Ok(s) => serde_json::json!({
                    "configured": s.tier2_configured,
                    "ok": s.health_ok,
                    "version": s.health_version,
                    "model_ready": s.health_model_ready,
                    "authenticated": s.authenticated,
                    "username": s.username,
                    "error": s.error,
                }),
                Err(e) => serde_json::json!({"configured": false, "error": e.to_string()}),
            }
        }
    };

    let (cb, cis, cl) = join!(cb_probe, cis_probe, cl_probe);
    Ok(serde_json::json!({
        "cloud_backup": cb,
        "crisp_index_server": cis,
        "crisplens": cl,
    }))
}

/// Stage Q — back up VPS shards to a cloud drive from the GUI.
///
/// Mirrors `crispsorter sync cloud-backup backup-shards` but runs
/// in-process so the Settings "Backup now" button can invoke it
/// without spawning a subprocess.
#[tauri::command]
pub async fn sync_cb_backup_shards(
    state: State<'_, AppState>,
    drive_id: String,
    shard: Option<String>,
    force: bool,
    keep_daily: Option<usize>,
) -> Result<serde_json::Value, String> {
    use crate::drives::DriveRegistry;
    use crate::sync::backup_state::BackupState;
    use std::sync::Arc;

    let cli = make_cb_client(&state).await?;
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;

    let registry = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let drive_cfg = registry.drives.iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?
        .clone();
    let drive: Arc<dyn crate::drives::CloudDrive> =
        Arc::from(
            DriveRegistry::instantiate_with_proxy(&drive_cfg, &proxy_config(&state).await?)
                .map_err(|e| e.to_string())?,
        );
    let transfer_queue = state.transfer_queue.clone();

    let bs = BackupState::open(&data_dir).map_err(|e| e.to_string())?;
    let shard_list = cli.shard_list().await.map_err(|e| e.to_string())?;

    let shards_to_backup: Vec<_> = shard_list.shards.iter()
        .filter(|s| {
            if let Some(ref requested) = shard {
                if &s.prefix != requested { return false; }
            }
            if !force {
                if let Ok(Some(rec)) = bs.last_backup(&s.prefix) {
                    if rec.last_watermark >= s.max_indexed_at { return false; }
                }
            }
            true
        })
        .collect();

    if shards_to_backup.is_empty() {
        return Ok(serde_json::json!({ "backed_up": 0, "skipped": shard_list.shards.len() }));
    }

    // Date-stamped directory.
    let today = {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let days = secs / 86400;
        let z = days as i64 + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = z - era * 146097;
        let yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;
        let y   = yoe + era * 400;
        let doy = doe - (365*yoe + yoe/4 - yoe/100);
        let mp  = (5*doy + 2)/153;
        let d   = doy - (153*mp+2)/5 + 1;
        let m   = if mp < 10 { mp + 3 } else { mp - 9 };
        let y   = if m <= 2 { y + 1 } else { y };
        format!("{:04}-{:02}-{:02}", y, m, d)
    };
    let backup_dir = std::path::Path::new("cb-backups").join(&today);

    let mut backed_up = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for si in &shards_to_backup {
        let drive_path = backup_dir.join(format!("{}.tar.gz", si.prefix));
        match cli.shard_export(&si.prefix).await {
            Ok(data) => {
                let retry_data = data.clone();
                let retry_path = drive_path.clone();
                let drive_for_transfer = Arc::clone(&drive);
                let transfer = transfer_queue.submit_upload(
                    drive_id.clone(),
                    drive_path.clone(),
                    data,
                    move |path, bytes| drive_for_transfer.write_file(path, bytes),
                );
                match transfer.handle.await {
                    Ok(Ok(_)) => {
                        let _ = bs.record_backup(
                            &si.prefix,
                            si.max_indexed_at,
                            &drive_id,
                            &drive_path.to_string_lossy(),
                        );
                        backed_up += 1;
                    }
                    Ok(Err(e)) => {
                        let queued = crate::sync::tauri_commands::queue_failed_drive_upload(
                            &data_dir,
                            &drive_id,
                            &retry_path,
                            &retry_data,
                            &e,
                        );
                        errors.push(match queued {
                            Ok(id) => format!(
                                "write {}: {e} (queued offline operation {id})",
                                si.prefix
                            ),
                            Err(queue_error) => format!(
                                "write {}: {e} (offline queue failed: {queue_error})",
                                si.prefix
                            ),
                        });
                    }
                    Err(e) => {
                        let transfer_error = anyhow::anyhow!("write {} queue task: {e}", si.prefix);
                        let queued = crate::sync::tauri_commands::queue_failed_drive_upload(
                            &data_dir,
                            &drive_id,
                            &retry_path,
                            &retry_data,
                            &transfer_error,
                        );
                        errors.push(match queued {
                            Ok(id) => format!("{transfer_error} (queued offline operation {id})"),
                            Err(queue_error) => format!(
                                "{transfer_error} (offline queue failed: {queue_error})"
                            ),
                        });
                    }
                }
            }
            Err(e) => errors.push(format!("export {}: {e}", si.prefix)),
        }
    }

    // Retention.
    let keep = keep_daily.unwrap_or(7);
    if keep > 0 {
        let cb_root = std::path::Path::new("cb-backups");
        if let Ok(entries) = drive.list_dir(cb_root) {
            let mut dirs: Vec<String> = entries.iter()
                .filter(|e| e.is_dir).map(|e| e.name.clone()).collect();
            dirs.sort();
            let to_delete = dirs.len().saturating_sub(keep);
            for old_dir in dirs.iter().take(to_delete) {
                let old_path = cb_root.join(old_dir);
                if let Ok(files) = drive.list_dir(&old_path) {
                    for f in files { let _ = drive.delete(&old_path.join(&f.name)); }
                }
                let _ = drive.delete(&old_path);
            }
        }
    }

    Ok(serde_json::json!({
        "backed_up": backed_up,
        "errors":    errors,
        "date_dir":  today,
    }))
}

/// Restore a shard backup from a cloud drive into the cb-api VPS.
/// Mirrors the CLI `sync cloud-backup restore-shard` command.
#[tauri::command]
pub async fn sync_cb_restore_shard(
    state: State<'_, AppState>,
    prefix: String,
    drive_id: String,
    date: Option<String>,
) -> Result<serde_json::Value, String> {
    use crate::drives::DriveRegistry;
    use std::sync::Arc;

    let cli = make_cb_client(&state).await?;
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;

    let registry = DriveRegistry::open(&data_dir).map_err(|e| e.to_string())?;
    let drive_cfg = registry.drives.iter()
        .find(|d| d.id == drive_id)
        .ok_or_else(|| format!("drive '{drive_id}' not found"))?
        .clone();
    let drive: Arc<dyn crate::drives::CloudDrive> =
        Arc::from(
            DriveRegistry::instantiate_with_proxy(&drive_cfg, &proxy_config(&state).await?)
                .map_err(|e| e.to_string())?,
        );

    // Resolve the date dir: explicit or most-recent.
    let cb_root = std::path::Path::new("cb-backups");
    let date_dir = if let Some(d) = date {
        d
    } else {
        let mut dirs: Vec<String> = drive.list_dir(cb_root)
            .map_err(|e| format!("list cb-backups: {e}"))?
            .into_iter()
            .filter(|e| e.is_dir)
            .map(|e| e.name)
            .collect();
        dirs.sort();
        dirs.pop().ok_or("no backup directories found on drive")?
    };

    let tar_path = cb_root.join(&date_dir).join(format!("{prefix}.tar.gz"));
    let transfer_queue = state.transfer_queue.clone();
    let drive_for_transfer = Arc::clone(&drive);
    let transfer = transfer_queue.submit_download(
        drive_id.clone(),
        tar_path.clone(),
        None,
        move |path| drive_for_transfer.read_file(path),
    );
    let data = match transfer.handle.await {
        Ok(Ok(data)) => data,
        Ok(Err(e)) => return Err(format!("read {} from drive: {e}", tar_path.display())),
        Err(e) => return Err(format!("read {} queue task: {e}", tar_path.display())),
    };

    let byte_count = data.len();
    cli.shard_import(&prefix, data).await.map_err(|e| e.to_string())?;

    // Update backup state so next incremental backup knows about this.
    if let Ok(bs) = crate::sync::backup_state::BackupState::open(&data_dir) {
        let drive_path_str = tar_path.to_string_lossy().into_owned();
        let _ = bs.record_backup(&prefix, 0, &drive_id, &drive_path_str);
    }

    Ok(serde_json::json!({
        "restored": prefix,
        "from_drive": drive_id,
        "date": date_dir,
        "bytes": byte_count,
    }))
}

/// Stage R — one-shot import of `source_files` rows from a controller.py
/// manifest SQLite into the cb-api VPS via `/api/manifest/push`.
/// Resumable: a per-path watermark in `<data-dir>/manifest_import_state.db`
/// lets re-runs skip already-pushed rows.
///
/// All rusqlite I/O runs in `spawn_blocking` so the `!Send` Connection
/// never crosses an `.await` boundary.
#[tauri::command]
pub async fn sync_cb_import_from_manifest_db(
    state:      tauri::State<'_, crate::AppState>,
    path:       String,
    owner_id:   String,
    batch_size: usize,
    dry_run:    bool,
) -> Result<serde_json::Value, String> {
    use crate::sync::cloud_backup::ManifestRow;
    use std::path::PathBuf;

    let manifest_db = PathBuf::from(&path);
    if !manifest_db.exists() {
        return Err(format!("manifest_db not found: {path}"));
    }

    let cli = make_cb_client(&state).await?;
    let data_dir = state.data_dir.lock().await.clone()
        .ok_or("data_dir not initialised")?;
    let state_path = data_dir.join("manifest_import_state.db");
    let owner = if owner_id.is_empty() { "".to_string() } else { owner_id };

    // Read initial watermark in a blocking task.
    let db_key = manifest_db.canonicalize()
        .unwrap_or_else(|_| manifest_db.clone())
        .to_string_lossy()
        .into_owned();
    let mut watermark: i64 = {
        let sp = state_path.clone();
        let dk = db_key.clone();
        tokio::task::spawn_blocking(move || -> i64 {
            use rusqlite::Connection as RC;
            let Ok(c) = RC::open(&sp) else { return 0 };
            let _ = c.execute_batch(
                "CREATE TABLE IF NOT EXISTS manifest_imports \
                 (db_path TEXT PRIMARY KEY, last_source_id INTEGER NOT NULL DEFAULT 0)"
            );
            c.query_row(
                "SELECT last_source_id FROM manifest_imports WHERE db_path = ?",
                rusqlite::params![&dk], |r| r.get(0),
            ).unwrap_or(0i64)
        }).await.unwrap_or(0)
    };

    let mut total_imported = 0usize;
    let mut max_source_id  = watermark;

    loop {
        // Read one batch synchronously.
        type BatchRow = (i64, String, String, i64, f64, Option<i64>);
        let batch: Vec<BatchRow> = {
            let mb = manifest_db.clone();
            let wm = watermark;
            let bs = batch_size;
            tokio::task::spawn_blocking(move || -> Result<Vec<BatchRow>, String> {
                use rusqlite::{Connection as RC, OpenFlags};
                let c = RC::open_with_flags(&mb, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(|e| e.to_string())?;
                let mut s = c.prepare(
                    "SELECT source_id, file_path, file_hash, file_size_bytes, \
                            modified_time, archived_in \
                     FROM source_files \
                     WHERE source_id > ? AND file_hash IS NOT NULL \
                     ORDER BY source_id LIMIT ?"
                ).map_err(|e| e.to_string())?;
                let result: Vec<BatchRow> = s.query_map(
                    rusqlite::params![wm, bs as i64],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
                            r.get::<_, f64>(4).unwrap_or(0.0), r.get(5)?)),
                ).map_err(|e| e.to_string())?
                .filter_map(|r| r.ok())
                .collect();
                Ok(result)
            }).await.map_err(|e| e.to_string())??
        };

        if batch.is_empty() { break; }
        let new_max = batch.iter().map(|r| r.0).max().unwrap_or(watermark);
        let n = batch.len();

        let manifest_rows: Vec<ManifestRow> = batch.iter().map(|r| {
            let p = std::path::Path::new(&r.1);
            ManifestRow {
                path:        r.1.clone(), size_bytes: r.3, sha256: r.2.clone(),
                mtime_unix:  r.4, owner_id: owner.clone(),
                filename:    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                ext:         p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default(),
                parent_dir:  p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
                language: None, title: None, author: None, year: None,
                full_text: None, collection_id: None, archived_in: r.5,
                url: None,
                tags: vec![],
            }
        }).collect();

        if !dry_run {
            let resp = cli.manifest_push(&manifest_rows).await.map_err(|e| e.to_string())?;
            total_imported += resp.accepted;
            max_source_id = new_max;
            let sp = state_path.clone();
            let dk = db_key.clone();
            tokio::task::spawn_blocking(move || {
                use rusqlite::Connection as RC;
                let Ok(c) = RC::open(&sp) else { return };
                let _ = c.execute_batch(
                    "CREATE TABLE IF NOT EXISTS manifest_imports \
                     (db_path TEXT PRIMARY KEY, last_source_id INTEGER NOT NULL DEFAULT 0)"
                );
                let _ = c.execute(
                    "INSERT INTO manifest_imports (db_path, last_source_id) VALUES (?1, ?2) \
                     ON CONFLICT(db_path) DO UPDATE SET last_source_id = excluded.last_source_id",
                    rusqlite::params![&dk, new_max],
                );
            }).await.ok();
        }

        watermark = new_max;
        if n < batch_size { break; }
    }

    Ok(serde_json::json!({
        "imported":   total_imported,
        "watermark":  max_source_id,
        "dry_run":    dry_run,
    }))
}

// ── Stage S — Federated search ──────────────────────────────────────────────

/// RRF constant k (60 is the standard default).
pub(crate) const RRF_K: f32 = 60.0;

fn rrf_score(rank: usize) -> f32 {
    1.0 / (RRF_K + rank as f32)
}

/// Merge per-backend ranked lists into a single RRF-fused ranking.
///
/// `lists` is a slice of ranked `FederatedHit` vecs (best-first).
/// Deduplication key: sha256 when non-empty, otherwise the hit's `id`.
/// When two backends return the same file (same sha256), their RRF
/// contributions are summed and only one record is kept (the one with
/// the higher source-score).  Hits without a sha256 are keyed by `id`
/// and never merged across backends.
pub(crate) fn rrf_merge(lists: Vec<Vec<FederatedHit>>, limit: usize) -> Vec<FederatedHit> {
    use std::collections::HashMap;

    // Map merge_key → (accumulated_score, best hit so far)
    let mut scores: HashMap<String, (f32, FederatedHit)> = HashMap::new();

    for list in lists {
        for (rank0, mut hit) in list.into_iter().enumerate() {
            let contribution = rrf_score(rank0 + 1);
            let key = hit.sha256.as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
                .unwrap_or_else(|| hit.id.clone());
            let entry = scores.entry(key).or_insert_with(|| {
                hit.score = contribution;
                (0.0, hit.clone())
            });
            entry.0 += contribution;
            // Keep the hit with the higher source-score as the display record.
            if hit.score > entry.1.score {
                entry.1 = hit;
            }
            entry.1.score = entry.0;
        }
    }

    let mut merged: Vec<FederatedHit> = scores.into_values().map(|(_, h)| h).collect();
    merged.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(limit);
    for (i, h) in merged.iter_mut().enumerate() {
        h.rrf_rank = i + 1;
    }
    merged
}

/// Stage S — fan out a free-text query to all enabled backends in parallel,
/// normalise their payloads to `FederatedHit`, RRF-merge, and return the
/// union ranked list.
///
/// `backends` is a comma-separated list drawn from
/// `"local"`, `"cloud_backup"`, `"crisplens"`.  Omit or pass an empty
/// string to query all three.
///
/// Per-backend errors are swallowed and reported in the `errors` field
/// of the returned JSON so a degraded backend doesn't suppress results
/// from the others.
#[tauri::command]
pub async fn sync_federated_search(
    state: State<'_, AppState>,
    q: String,
    limit: Option<usize>,
    backends: Option<String>,
    ext: Option<Vec<String>>,
    lang: Option<String>,
    year_min: Option<i32>,
    year_max: Option<i32>,
    folder_prefix: Option<String>,
    url_domain: Option<String>,
    tag: Option<String>,
    audio_duration_min: Option<f64>,
    audio_duration_max: Option<f64>,
    image_camera_make: Option<String>,
    image_camera_model: Option<String>,
    colbert_rerank: Option<bool>,
    omni_search: Option<bool>,
) -> Result<serde_json::Value, String> {
    use tokio::join;

    let q = q.trim().to_owned();
    if q.is_empty() {
        return Ok(serde_json::json!({ "hits": [], "errors": {} }));
    }
    let limit = limit.unwrap_or(20).clamp(1, 200);

    let enabled: std::collections::HashSet<&str> = {
        let raw = backends.as_deref().unwrap_or("");
        if raw.is_empty() {
            ["local", "cloud_backup", "crisplens"].into()
        } else {
            raw.split(',').map(str::trim).collect()
        }
    };

    let want_local = enabled.contains("local");
    let want_cb    = enabled.contains("cloud_backup");
    let proxy = proxy_config_for_app_state(&state).await?;
    #[cfg(feature = "images-crisplens")]
    let want_cl    = enabled.contains("crisplens");

    // Snapshot config under a single lock.
    let (cb_url_val, data_dir_opt) = {
        let idx = state.index.lock().await;
        let url = idx.config.cloud_backup_url.clone().unwrap_or_default();
        let dd  = state.data_dir.lock().await.clone();
        (url, dd)
    };
    let data_dir = data_dir_opt.ok_or("data_dir not initialised")?;

    // Build filters from optional params.
    let filters = {
        let mut f = crate::index::schema::SearchFilters::default();
        if let Some(ref exts) = ext {
            f.ext = exts.clone();
        }
        if let Some(ref l) = lang {
            f.language = Some(l.clone());
        }
        f.year_min = year_min;
        f.year_max = year_max;
        if let Some(ref fp) = folder_prefix {
            f.parent_dir_prefix = Some(fp.clone());
        }
        if let Some(ref ud) = url_domain {
            f.url_domain = Some(ud.clone());
        }
        if let Some(ref t) = tag {
            f.tag = Some(t.clone());
        }
        f.audio_duration_min_seconds = audio_duration_min;
        f.audio_duration_max_seconds = audio_duration_max;
        if let Some(ref m) = image_camera_make {
            f.image_camera_make = Some(m.clone());
        }
        if let Some(ref m) = image_camera_model {
            f.image_camera_model = Some(m.clone());
        }
        if let Some(c) = colbert_rerank {
            f.colbert_rerank = c;
        }
        if let Some(o) = omni_search {
            f.omni_search = o;
        }
        f
    };

    // ── Local backend ───────────────────────────────────────────────────────
    let local_fut = async {
        if !want_local { return (Vec::new(), None); }
        let lock = state.index.lock().await;
        if !lock.config.enabled { return (Vec::new(), None); }
        let engine = lock.engine.clone();
        drop(lock);
        let Some(engine) = engine else {
            return (Vec::new(), Some("local index not initialised".to_owned()));
        };
        match engine.search_hybrid(&q, &filters, limit).await {
            Err(e) => (Vec::new(), Some(e.to_string())),
            Ok(hits) => {
                let fed: Vec<FederatedHit> = hits.into_iter().enumerate().map(|(i, r)| {
                    FederatedHit {
                        id: format!("local:{}", r.doc_id),
                        source: "local".into(),
                        score: r.score,
                        rrf_rank: i + 1,
                        filename: r.filename,
                        path: Some(r.location_uri.clone()),
                        ext: r.ext,
                        title: r.title,
                        author: r.author,
                        year: r.year,
                        language: r.language,
                        sha256: if r.source_hash.is_empty() { None } else { Some(r.source_hash) },
                        size_bytes: None,
                        snippet: if r.snippet.is_empty() { None } else { Some(r.snippet) },
                        location_uri: Some(r.location_uri),
                        url: r.url,
                        tags: if r.tags.is_empty() { None } else { Some(r.tags) },
                    }
                }).collect();
                (fed, None)
            }
        }
    };

    // ── Cloud-backup backend ────────────────────────────────────────────────
    let cb_fut = async {
        if !want_cb || cb_url_val.is_empty() { return (Vec::new(), None); }
        let token = match super::secret::get_token_for_url(&cb_url_val) {
            Ok(Some(t)) => t,
            Ok(None)    => return (Vec::new(), Some("cloud_backup: no token configured".to_owned())),
            Err(e)      => return (Vec::new(), Some(format!("cloud_backup: keychain error: {e}"))),
        };
        let cli = match CloudBackupClient::new_with_proxy(&cb_url_val, &token, &proxy) {
            Ok(c)  => c,
            Err(e) => return (Vec::new(), Some(format!("cloud_backup: client error: {e}"))),
        };
        // Federated search is display-only — request the lean payload and
        // render the server-computed snippet (fall back to a client-side
        // truncation of full_text for older servers that don't send one).
        match cli.search(&q, limit, false).await {
            Err(e) => (Vec::new(), Some(format!("cloud_backup: {e}"))),
            Ok(resp) => {
                let fed: Vec<FederatedHit> = resp.rows.into_iter().enumerate().map(|(i, h)| {
                    let snippet = h.snippet.clone().or_else(|| {
                        h.full_text.as_ref().map(|t| crate::index::snippet::truncate_str(t, 300).to_owned())
                    });
                    FederatedHit {
                        id: format!("cloud_backup:{}", h.sha256),
                        source: "cloud_backup".into(),
                        score: h.score,
                        rrf_rank: i + 1,
                        filename: Some(h.filename),
                        path: Some(h.path.clone()),
                        ext: Some(h.ext),
                        title: h.title,
                        author: h.author,
                        year: h.year,
                        language: h.language,
                        sha256: Some(h.sha256),
                        size_bytes: Some(h.size_bytes),
                        snippet,
                        location_uri: None,
                        url: h.url,
                        tags: if h.tags.is_empty() { None } else { Some(h.tags) },
                    }
                }).collect();
                (fed, None)
            }
        }
    };

    // ── CrispLens backend ───────────────────────────────────────────────────
    // Behind `images-crisplens`: with the feature off there is no client, and the
    // leg contributes nothing rather than erroring — the same shape `want_cl ==
    // false` already produces for an unconfigured install, so callers need no
    // second code path. See PLAN.md P35.4.
    #[cfg(not(feature = "images-crisplens"))]
    // Typed `None` rather than a bare one: the feature-on arm infers the error
    // type from its own `Err(e) => (…, Some(e.to_string()))`, but this arm has
    // nothing to infer from, and the only consumer is `e.into()` into a
    // `serde_json::Value`.
    let cl_fut = async { (Vec::<FederatedHit>::new(), Option::<String>::None) };
    #[cfg(feature = "images-crisplens")]
    let cl_fut = async {
        if !want_cl { return (Vec::new(), None); }
        let dd = data_dir.clone();
        let q2 = q.clone();
        let lim = limit as i64;
        match tauri::async_runtime::spawn_blocking(move || {
            use crate::images::crisplens::tauri_commands::get_json;
            let encoded: String = q2.chars().flat_map(|c| {
                if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '~') {
                    vec![c]
                } else {
                    format!("%{:02X}", c as u32).chars().collect::<Vec<_>>()
                }
            }).collect();
            let path = format!("/api/search/semantic?q={encoded}&limit={lim}");
            get_json::<Vec<crisplens_protocol::SearchHit>>(&dd, &path)
        }).await {
            Err(e) => (Vec::new(), Some(format!("crisplens: join error: {e}"))),
            Ok(Err(e)) => (Vec::new(), Some(format!("crisplens: {e}"))),
            Ok(Ok(hits)) => {
                let fed: Vec<FederatedHit> = hits.into_iter().enumerate().map(|(i, h)| {
                    let filename = h.filename.clone();
                    FederatedHit {
                        id: format!("crisplens:{}", h.id),
                        source: "crisplens".into(),
                        score: h.score.unwrap_or(0.0),
                        rrf_rank: i + 1,
                        filename: Some(filename),
                        path: Some(h.filepath.clone()),
                        ext: h.filepath.rsplit('.').next().map(|e| e.to_lowercase()),
                        title: h.description.clone(),
                        author: None,
                        year: None,
                        language: None,
                        sha256: None,
                        size_bytes: None,
                        snippet: h.description,
                        location_uri: None,
                        url: None,
                        tags: None,
                    }
                }).collect();
                (fed, None)
            }
        }
    };

    let ((local_hits, local_err), (cb_hits, cb_err), (cl_hits, cl_err)) =
        join!(local_fut, cb_fut, cl_fut);

    let mut lists: Vec<Vec<FederatedHit>> = Vec::new();
    if !local_hits.is_empty() { lists.push(local_hits); }
    if !cb_hits.is_empty()    { lists.push(cb_hits); }
    if !cl_hits.is_empty()    { lists.push(cl_hits); }

    let merged = rrf_merge(lists, limit);

    let mut errors = serde_json::Map::new();
    if let Some(e) = local_err { errors.insert("local".into(), e.into()); }
    if let Some(e) = cb_err    { errors.insert("cloud_backup".into(), e.into()); }
    if let Some(e) = cl_err    { errors.insert("crisplens".into(), e.into()); }

    Ok(serde_json::json!({
        "hits":   merged,
        "errors": errors,
    }))
}

// ── Stage U — thin-client extract-status ───────────────────────────────────

/// Stage U — poll the VPS extraction-worker queue depths.
/// Returns `{pending, in_progress, done, failed, worker_db_found}`.
#[tauri::command]
pub async fn sync_cb_extract_status(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let cli = make_cb_client(&state).await?;
    let r = cli.extract_status().await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "pending":         r.pending,
        "in_progress":     r.in_progress,
        "done":            r.done,
        "failed":          r.failed,
        "worker_db_found": r.worker_db_found,
    }))
}

// ── Stage W — skeleton index search ────────────────────────────────────────

/// Stage W — search the local skeleton index for quick author / dir hints.
///
/// Returns `{authors: [{name, doc_count}], parent_dirs: [{name, doc_count}]}`.
/// Safe to call even when `local_skeleton_only=false` — returns empty lists if
/// no skeleton DB exists yet.
#[tauri::command]
pub async fn sync_skeleton_search(
    state: State<'_, AppState>,
    query: String,
) -> Result<serde_json::Value, String> {
    let data_dir = state
        .data_dir
        .lock()
        .await
        .clone()
        .ok_or("data_dir not set")?;

    let sk = match crate::index::skeleton::SkeletonIndex::open_or_create(&data_dir) {
        Ok(s) => s,
        Err(_) => {
            return Ok(serde_json::json!({ "authors": [], "parent_dirs": [] }));
        }
    };

    let authors = sk
        .search_authors(&query, 10)
        .unwrap_or_default()
        .into_iter()
        .map(|h| serde_json::json!({ "name": h.name, "doc_count": h.doc_count }))
        .collect::<Vec<_>>();

    let parent_dirs = sk
        .search_parent_dirs(&query, 10)
        .unwrap_or_default()
        .into_iter()
        .map(|h| serde_json::json!({ "name": h.name, "doc_count": h.doc_count }))
        .collect::<Vec<_>>();

    Ok(serde_json::json!({ "authors": authors, "parent_dirs": parent_dirs }))
}

// ── Stage T — admin key management ─────────────────────────────────────────

fn cb_client_for_admin(state: &AppState) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(CloudBackupClient, String), String>> + Send + '_>> {
    Box::pin(async move {
        let (url, _) = {
            let idx = state.index.lock().await;
            let url = idx.config.cloud_backup_url.clone().unwrap_or_default();
            (url, ())
        };
        if url.is_empty() {
            return Err("cloud_backup_url not configured".into());
        }
        let token = super::secret::get_token_for_url(&url)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "no API token stored — run sync cloud-backup login first".to_owned())?;
        let proxy = proxy_config_for_app_state(state).await?;
        let cli = CloudBackupClient::new_with_proxy(&url, &token, &proxy)
            .map_err(|e| e.to_string())?;
        Ok((cli, url))
    })
}

/// Stage T — mint a new API key on the VPS admin surface.
/// `admin_token` is the `CB_API_ADMIN_TOKEN` value from the VPS env file.
#[tauri::command]
pub async fn sync_cb_admin_mint(
    state: State<'_, AppState>,
    admin_token: String,
    name: String,
    owner_id: Option<String>,
) -> Result<serde_json::Value, String> {
    let (cli, _url) = cb_client_for_admin(&state).await?;
    let resp: AdminMintResponse = cli
        .admin_mint(&admin_token, &name, owner_id.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "raw_key":  resp.raw_key,
        "name":     resp.name,
        "owner_id": resp.owner_id,
    }))
}

/// Stage T — revoke an existing API key by name.
#[tauri::command]
pub async fn sync_cb_admin_revoke(
    state: State<'_, AppState>,
    admin_token: String,
    name: String,
) -> Result<serde_json::Value, String> {
    let (cli, _url) = cb_client_for_admin(&state).await?;
    let resp: AdminRevokeResponse = cli
        .admin_revoke(&admin_token, &name)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "revoked": resp.revoked,
        "name":    resp.name,
    }))
}

/// Stage T — list all API keys on the VPS (metadata only, no hashes).
#[tauri::command]
pub async fn sync_cb_admin_list_keys(
    state: State<'_, AppState>,
    admin_token: String,
) -> Result<serde_json::Value, String> {
    let (cli, _url) = cb_client_for_admin(&state).await?;
    let rows: Vec<AdminKeyInfo> = cli
        .admin_list_keys(&admin_token)
        .await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "keys": rows }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::cloud_backup::FederatedHit;

    fn make_hit(source: &str, id: &str, score: f32) -> FederatedHit {
        make_hit_with_sha(source, id, score, None)
    }

    fn make_hit_with_sha(source: &str, id: &str, score: f32, sha: Option<&str>) -> FederatedHit {
        FederatedHit {
            id: format!("{source}:{id}"),
            source: source.into(),
            score,
            rrf_rank: 0,
            filename: Some(format!("{id}.pdf")),
            path: None, ext: None, title: None, author: None, year: None,
            language: None,
            sha256: sha.map(|s| s.to_owned()),
            size_bytes: None, snippet: None,
            location_uri: None,
            url: None, tags: None,
        }
    }

    #[test]
    fn rrf_merge_deduplicates_by_sha256_and_ranks() {
        // Two backends each return hits; one sha256 appears in both.
        // The shared sha256 should accumulate RRF from both backends →
        // rank above a hit that only appeared in one backend at rank 1.
        let shared_sha = "aa".repeat(32); // 64-char hex
        let local = vec![
            make_hit("local", "a", 0.9),
            make_hit_with_sha("local", "shared", 0.8, Some(&shared_sha)),
            make_hit("local", "b", 0.7),
        ];
        let cb = vec![
            // cloud_backup rank-1 hit has same sha256 → merges with local:shared
            make_hit_with_sha("cloud_backup", "shared-cb", 0.95, Some(&shared_sha)),
            make_hit("cloud_backup", "c", 0.85),
        ];
        let merged = rrf_merge(vec![local, cb], 10);

        // "shared" accumulated RRF(rank-2-in-local) + RRF(rank-1-in-cb).
        // "local:a" only has RRF(rank-1-in-local).
        // RRF(1) = 1/61 ≈ 0.01639
        // RRF(2) = 1/62 ≈ 0.01613
        // shared_rrf = 1/61 + 1/62 ≈ 0.03252  >  local:a_rrf = 1/61 ≈ 0.01639
        let winner_sha = merged[0].sha256.as_deref().unwrap_or("");
        assert_eq!(winner_sha, shared_sha,
            "sha-merged hit should win; top hit sha={winner_sha:?}, \
             all: {:?}", merged.iter().map(|h| (&h.id, h.score)).collect::<Vec<_>>());

        // 4 unique sha/id keys: shared, a, b, c (not 5 — shared is merged).
        assert_eq!(merged.len(), 4,
            "expected 4 hits after dedup; got {}: {:?}",
            merged.len(), merged.iter().map(|h| &h.id).collect::<Vec<_>>());

        // All non-shared unique items present by id substring.
        let ids: Vec<&str> = merged.iter().map(|h| h.id.as_str()).collect();
        assert!(ids.iter().any(|id| id.contains("local:a")));
        assert!(ids.iter().any(|id| id.contains("cloud_backup:c")));
        assert!(ids.iter().any(|id| id.contains("local:b")));

        // rrf_rank is 1-based and strictly increasing.
        for (i, h) in merged.iter().enumerate() {
            assert_eq!(h.rrf_rank, i + 1);
        }
    }

    #[test]
    fn rrf_merge_no_dedup_without_sha256() {
        // Hits without sha256 are not deduplicated even if they look related.
        let local = vec![make_hit("local", "x", 0.9)];
        let cb    = vec![make_hit("cloud_backup", "x", 0.9)]; // same id stem, no sha
        let merged = rrf_merge(vec![local, cb], 10);
        assert_eq!(merged.len(), 2, "no sha → no dedup");
    }

    #[test]
    fn rrf_merge_respects_limit() {
        let list: Vec<FederatedHit> = (0..10u32)
            .map(|i| make_hit("local", &i.to_string(), 1.0 - i as f32 * 0.1))
            .collect();
        let merged = rrf_merge(vec![list], 3);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn rrf_merge_empty_lists() {
        let merged = rrf_merge(vec![], 10);
        assert!(merged.is_empty());
    }

    #[test]
    fn remote_inventory_recurses_and_applies_pair_filters() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/report.pdf"), b"pdf").unwrap();
        std::fs::write(dir.path().join("nested/skip.txt"), b"txt").unwrap();
        let drive = crate::drives::LocalDrive::new("inventory", dir.path().to_owned());
        let mut rows = Vec::new();
        // Start from the drive's own root path, the way a configured sync pair
        // does. NOT `Path::new("/")`: `LocalDrive::full` passes absolute paths
        // through by design (it backs local disks and SFTP mounts, where
        // absolute remote paths are the normal case), so `/` means the real
        // filesystem root and this walked the whole machine until it hit an
        // unreadable system directory.
        inventory_remote(
            &drive,
            dir.path(),
            "",
            &["**/*.pdf".into()],
            &[],
            &mut rows,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].relative_path, "nested/report.pdf");
        assert_eq!(rows[0].size, 3);
        assert!(rows[0].mtime_unix.is_some());
    }
}
