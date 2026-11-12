/// Tauri commands exposing the batch session SQLite store to the frontend.
///
/// All SQLite work runs in `spawn_blocking` so the Tokio executor is never
/// blocked by a synchronous Mutex acquisition.
use tauri::State;

use serde_json::{json, Value};

use super::{BatchItemRow, BatchReplacementItem, BatchSessionStore, ProcessedHistoryRow};
use crate::operation_log::OperationLog;
use crate::AppState;

fn get_store(state: &AppState) -> Result<BatchSessionStore, String> {
    state
        .batch_session_store
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Batch session store not yet initialised".to_owned())
}

/// Run one state-changing session-store call under exactly one receipt.
/// Item ids and SQLite error text stay in the Result; errors propagate unchanged.
async fn logged<T, W, R>(state: &AppState, log: OperationLog, work: W, receipt: R) -> Result<T, String>
where
    T: Send + 'static,
    W: FnOnce(BatchSessionStore) -> anyhow::Result<T> + Send + 'static,
    R: FnOnce(&T) -> Value,
{
    let store = match get_store(state) {
        Ok(store) => store,
        Err(error) => { log.fail("store_unavailable"); return Err(error); }
    };
    match tokio::task::spawn_blocking(move || work(store)).await {
        Err(error) => { log.fail("worker_failed"); Err(error.to_string()) }
        Ok(Err(error)) => { log.fail("store_error"); Err(error.to_string()) }
        Ok(Ok(value)) => { log.succeed(receipt(&value)); Ok(value) }
    }
}

/// Load all items for the current session.
/// Returns them in insertion order.
#[tauri::command]
pub async fn batch_session_load(
    state: State<'_, AppState>,
) -> Result<Vec<BatchItemRow>, String> {
    let log = OperationLog::start("batch.session.load", Value::Null);
    logged(&state, log, |store| store.load(), |items| json!({"items": items.len()})).await
}

/// Upsert a single item (insert or update all mutable columns).
#[tauri::command]
pub async fn batch_session_upsert_item(
    state: State<'_, AppState>,
    item: BatchItemRow,
) -> Result<(), String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.upsert_item(&item))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Upsert many items in a single transaction.
/// Use this for bulk status updates (resetToQueued, setAcceptedItems, etc.)
/// to avoid N separate round-trips.
#[tauri::command]
pub async fn batch_session_upsert_items_bulk(
    state: State<'_, AppState>,
    items: Vec<BatchItemRow>,
) -> Result<(), String> {
    // Small status updates are frequent; only large writes get a receipt.
    if items.len() > 100 {
        let log = OperationLog::start("batch.session.save", json!({"items": items.len()}));
        return logged(&state, log, move |store| store.upsert_items_bulk(&items), |_| json!({"saved": true})).await;
    }
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.upsert_items_bulk(&items))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Commit a validated batch replacement and its full text in one transaction.
#[tauri::command]
pub async fn batch_session_replace(
    state: State<'_, AppState>,
    items: Vec<BatchReplacementItem>,
) -> Result<(), String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.replace(&items))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Delete items by their ids.
#[tauri::command]
pub async fn batch_session_delete_items(
    state: State<'_, AppState>,
    ids: Vec<String>,
) -> Result<(), String> {
    let log = OperationLog::start("batch.session.delete", json!({"items": ids.len()}));
    logged(&state, log, move |store| store.delete_items(&ids), |_| Value::Null).await
}

/// Delete all items for the current session.
#[tauri::command]
pub async fn batch_session_clear(
    state: State<'_, AppState>,
) -> Result<(), String> {
    let log = OperationLog::start("batch.session.clear", Value::Null);
    logged(&state, log, |store| store.clear(), |_| Value::Null).await
}

/// Persist the full extracted text for an item.
/// Kept separate from the item row so the hot path (status updates, list
/// rendering) doesn't carry MB of text across the IPC boundary.
#[tauri::command]
pub async fn batch_session_set_extracted_text(
    state: State<'_, AppState>,
    item_id: String,
    text: String,
) -> Result<(), String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.set_extracted_text(&item_id, &text))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Retrieve the full extracted text for an item, or `null` if not stored.
#[tauri::command]
pub async fn batch_session_get_extracted_text(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<Option<String>, String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.get_extracted_text(&item_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Return `true` if the one-shot JSON→SQLite migration has already run.
#[tauri::command]
pub async fn batch_session_is_migrated(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.is_migrated())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Record that the one-shot JSON→SQLite migration is done.
/// Idempotent — safe to call on every startup after checking `is_migrated`.
#[tauri::command]
pub async fn batch_session_mark_migrated(
    state: State<'_, AppState>,
) -> Result<(), String> {
    let log = OperationLog::start("batch.session.migrate", Value::Null);
    logged(&state, log, |store| store.mark_migrated(), |_| Value::Null).await
}

/// Record a successfully-processed item in the persistent history table.
/// Call after a file reaches 'done' status so future batches can skip it.
#[tauri::command]
pub async fn batch_session_record_processed(
    state: State<'_, AppState>,
    row: ProcessedHistoryRow,
) -> Result<(), String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.record_processed(&row))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Look up a previously-processed item by SHA-256.
/// Returns `null` when the hash has never been seen before.
#[tauri::command]
pub async fn batch_session_lookup_history(
    state: State<'_, AppState>,
    sha256: String,
) -> Result<Option<ProcessedHistoryRow>, String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.lookup_history_by_sha256(&sha256))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Total number of distinct files in the processed history.
#[tauri::command]
pub async fn batch_session_history_count(
    state: State<'_, AppState>,
) -> Result<i64, String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.processed_history_count())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
