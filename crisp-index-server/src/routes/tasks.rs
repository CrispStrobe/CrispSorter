use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

/// GET /v1/tasks/:task_id
/// Return the current durable queue state for one bulk-ingest task.
pub async fn task_status(
    State(state): State<Arc<SharedState>>,
    Path(task_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let queue = state.queue.clone();
    match tokio::task::spawn_blocking(move || queue.get_task_status(&task_id)).await {
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("spawn_blocking panic: {e}") })),
        ),
        Ok(Ok(Some(status))) => (StatusCode::OK, Json(serde_json::to_value(status).unwrap_or(json!({})))),
        Ok(Ok(None)) => (StatusCode::NOT_FOUND, Json(json!({ "error": "task not found" }))),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    }
}
