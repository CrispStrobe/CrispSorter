/// Tauri commands exposing the durable job queue to the frontend.
///
/// All SQLite work runs in `spawn_blocking` so the Tokio executor thread
/// is never blocked by a synchronous lock.
use tauri::State;

use super::{FileEntry, JobQueue, JobSummary, ListedFile, QueuedFile};
use crate::AppState;

fn get_queue(state: &AppState) -> Result<JobQueue, String> {
    state
        .job_queue
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Job queue not yet initialised".to_owned())
}

// ── Job lifecycle ──────────────────────────────────────────────────────────────

/// Create a new ingest job. Returns the new job UUID.
///
/// `job_type`     — caller-defined label (e.g. `"hinzufuegen"`, `"batch_import"`).
/// `source_paths` — root directories or file lists that the caller will scan.
/// `target_level` — 1 (fs metadata only), 2 (extracted metadata), 3 (full text + embeddings).
/// `config_json`  — optional JSON blob (batch size, model, etc.) for the worker.
#[tauri::command]
pub async fn jobs_create(
    state: State<'_, AppState>,
    job_type: Option<String>,
    source_paths: Vec<String>,
    target_level: u8,
    config_json: Option<String>,
) -> Result<String, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    let jt = job_type.unwrap_or_else(|| "ingest".to_owned());
    tokio::task::spawn_blocking(move || {
        queue.create_job(&jt, &source_paths, target_level, config_json.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// List all jobs (newest first).
#[tauri::command]
pub async fn jobs_list(state: State<'_, AppState>) -> Result<Vec<JobSummary>, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.list_jobs())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Get a single job by ID.
#[tauri::command]
pub async fn jobs_get(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<Option<JobSummary>, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.get_job(&job_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Set a job's status string (`pending` | `running` | `paused` | `done` | `error`).
#[tauri::command]
pub async fn jobs_set_status(
    state: State<'_, AppState>,
    job_id: String,
    status: String,
) -> Result<(), String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.set_job_status(&job_id, &status))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Delete a job and all its file_queue rows.
#[tauri::command]
pub async fn jobs_delete(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<(), String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.delete_job(&job_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

// ── File queue ────────────────────────────────────────────────────────────────

/// Bulk-add files to a job's queue.  Files already present are silently
/// ignored so this is safe to call multiple times (idempotent).
/// Returns the number of newly inserted rows.
#[tauri::command]
pub async fn jobs_add_files(
    state: State<'_, AppState>,
    job_id: String,
    files: Vec<FileEntry>,
) -> Result<usize, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.add_files(&job_id, &files))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Claim up to `batch_size` pending files and mark them `in_progress`.
/// Returns the claimed entries.  Empty result = job queue is drained.
#[tauri::command]
pub async fn jobs_claim_batch(
    state: State<'_, AppState>,
    job_id: String,
    batch_size: usize,
) -> Result<Vec<QueuedFile>, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.claim_batch(&job_id, batch_size))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Mark a batch of files as successfully processed.
/// `row_ids` are the `QueuedFile.row_id` values returned by `jobs_claim_batch`.
#[tauri::command]
pub async fn jobs_mark_done(
    state: State<'_, AppState>,
    job_id: String,
    row_ids: Vec<i64>,
) -> Result<(), String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.mark_done(&job_id, &row_ids))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Mark one file as errored.  The file is re-queued up to `max_retries`
/// times; after that it stays `error`.
#[tauri::command]
pub async fn jobs_mark_error(
    state: State<'_, AppState>,
    job_id: String,
    row_id: i64,
    error: String,
    max_retries: i32,
) -> Result<(), String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.mark_error(&job_id, row_id, &error, max_retries))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Mark one file as skipped (DRM, unsupported, etc.).
#[tauri::command]
pub async fn jobs_mark_skipped(
    state: State<'_, AppState>,
    job_id: String,
    row_id: i64,
) -> Result<(), String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.mark_skipped(&job_id, row_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Update the `doc_id` for a file after SHA-256 hashing completes.
#[tauri::command]
pub async fn jobs_set_doc_id(
    state: State<'_, AppState>,
    row_id: i64,
    doc_id: String,
) -> Result<(), String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.set_doc_id(row_id, &doc_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Reclaim all `in_progress` files back to `pending` for a job.
/// Call this at startup for any job in `running` state — it resets
/// work that was in-flight when the app was previously closed.
/// Returns the number of files reclaimed.
#[tauri::command]
pub async fn jobs_reclaim(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<usize, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.reclaim_in_progress(&job_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Number of files still pending or in_progress for a job.
#[tauri::command]
pub async fn jobs_pending_count(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<i64, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.pending_count(&job_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// List file-queue rows for a job (for UI display).
/// `status_filter` limits to a specific status; `null` returns all.
/// Returns at most `limit` rows starting at `offset`.
#[tauri::command]
pub async fn jobs_list_files(
    state: State<'_, AppState>,
    job_id: String,
    status_filter: Option<String>,
    limit: i64,
    offset: i64,
) -> Result<Vec<ListedFile>, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || {
        queue.list_files(&job_id, status_filter.as_deref(), limit, offset)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Delete a single file-queue row by `row_id`.
/// Returns `true` if a row was deleted.
#[tauri::command]
pub async fn jobs_remove_file(
    state: State<'_, AppState>,
    row_id: i64,
) -> Result<bool, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.remove_file(row_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Delete all file-queue rows for a job with the given status (e.g. `"done"`).
/// Returns the number of rows deleted.
#[tauri::command]
pub async fn jobs_remove_files_by_status(
    state: State<'_, AppState>,
    job_id: String,
    status: String,
) -> Result<usize, String> {
    let queue = get_queue(&state).map_err(|e| e)?;
    tokio::task::spawn_blocking(move || queue.remove_files_by_status(&job_id, &status))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
