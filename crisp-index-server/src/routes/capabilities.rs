use crate::state::SharedState;
use axum::{extract::State, http::StatusCode, Json};
use crisp_index_protocol::SearchCapabilities;
use std::sync::Arc;

/// Active lexical capabilities, after repairing any pending dual-store writes.
pub async fn capabilities(
    State(state): State<Arc<SharedState>>,
) -> Result<Json<SearchCapabilities>, (StatusCode, String)> {
    state
        .index
        .capabilities()
        .await
        .map(Json)
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))
}
