use axum::{extract::State, http::StatusCode, response::Json};
use crisp_index_protocol::StatsResponse;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

/// GET /v1/stats
/// Returns total chunk count and approximate distinct-document count.
pub async fn stats(State(state): State<Arc<SharedState>>) -> (StatusCode, Json<Value>) {
    let (row_count,doc_count)=match state.index.stats().await {
        Ok(counts)=>counts,
        Err(error)=>return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":error.to_string()}))),
    };
    let resp = StatsResponse {
        row_count,
        doc_count,
        embed_dims: state.config.embed_dims,
    };
    (
        StatusCode::OK,
        Json(serde_json::to_value(resp).unwrap_or(json!({}))),
    )
}
