/// Tauri commands exposing the batch session SQLite store to the frontend.
///
/// All SQLite work runs in `spawn_blocking` so the Tokio executor is never
/// blocked by a synchronous Mutex acquisition.
use tauri::State;

use super::{BatchItemRow, BatchSessionStore};
use crate::AppState;

fn get_store(state: &AppState) -> Result<BatchSessionStore, String> {
    state
        .batch_session_store
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "Batch session store not yet initialised".to_owned())
}

/// Load all items for the current session.
/// Returns them in insertion order.
#[tauri::command]
pub async fn batch_session_load(
    state: State<'_, AppState>,
) -> Result<Vec<BatchItemRow>, String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.load())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
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
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.upsert_items_bulk(&items))
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
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.delete_items(&ids))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

/// Delete all items for the current session.
#[tauri::command]
pub async fn batch_session_clear(
    state: State<'_, AppState>,
) -> Result<(), String> {
    let store = get_store(&state)?;
    tokio::task::spawn_blocking(move || store.clear())
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
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
