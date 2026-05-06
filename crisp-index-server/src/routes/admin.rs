use axum::{extract::State, http::StatusCode, response::Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

/// POST /v1/admin/build-ivf-pq
/// Trigger IVF-PQ vector index build.  Run after bulk ingest (≥10 000 rows).
pub async fn build_ivf_pq(
    State(state): State<Arc<SharedState>>,
) -> (StatusCode, Json<Value>) {
    tracing::info!("IVF-PQ build requested");
    if let Err(e) = state.index.vector.build_ivf_pq().await {
        tracing::error!("IVF-PQ build error: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
    }
    (StatusCode::OK, Json(json!({ "built": true })))
}
