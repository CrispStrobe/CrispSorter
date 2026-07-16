//! Tauri commands for the audit trail.

use super::{AuditEntry, AuditLog};
use tauri::State;

use crate::AppState;

/// Retrieve the audit log, lazily initialised.
async fn get_audit(state: &State<'_, AppState>) -> Result<AuditLog, String> {
    let data_dir = state.data_dir.lock().await;
    let dir = data_dir.as_ref().ok_or("App data dir not set")?;
    AuditLog::open_or_create(dir).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn audit_log_event(
    state: State<'_, AppState>,
    action: String,
    doc_id: Option<String>,
    detail: String,
) -> Result<(), String> {
    let log = get_audit(&state).await?;
    log.log(&action, doc_id.as_deref(), &detail, "gui")
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn audit_query(
    state: State<'_, AppState>,
    since: Option<i64>,
    action: Option<String>,
    doc_id: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<AuditEntry>, String> {
    let log = get_audit(&state).await?;
    log.query(
        since,
        action.as_deref(),
        doc_id.as_deref(),
        limit.unwrap_or(100),
        offset.unwrap_or(0),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn audit_count(
    state: State<'_, AppState>,
    action: Option<String>,
) -> Result<usize, String> {
    let log = get_audit(&state).await?;
    log.count(action.as_deref()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn audit_summary(
    state: State<'_, AppState>,
) -> Result<Vec<(String, usize)>, String> {
    let log = get_audit(&state).await?;
    log.action_summary().map_err(|e| e.to_string())
}
