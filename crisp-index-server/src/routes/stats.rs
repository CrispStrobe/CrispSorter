use axum::{extract::State, http::StatusCode, response::Json};
use crisp_index_protocol::StatsResponse;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

/// GET /v1/stats
/// Returns total chunk count and approximate distinct-document count.
pub async fn stats(
    State(state): State<Arc<SharedState>>,
) -> (StatusCode, Json<Value>) {
    let row_count = match state.index.vector.count().await {
        Ok(n) => n,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))),
    };
    let doc_count = state.index.vector.doc_count().await.unwrap_or(0);
    let resp = StatsResponse {
        row_count,
        doc_count,
        embed_dims: state.config.embed_dims,
    };
    (StatusCode::OK, Json(serde_json::to_value(resp).unwrap_or(json!({}))))
}
