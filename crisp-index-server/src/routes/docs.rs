use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};
use crisp_index_protocol::{DeleteResponse, UpdateLocationBody, UpdateLocationResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

/// DELETE /v1/docs/:doc_id
/// Delete all LanceDB chunks + the Tantivy entry for this document.
pub async fn delete_doc(
    State(state): State<Arc<SharedState>>,
    Path(doc_id): Path<String>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = state.index.vector.delete_doc(&doc_id).await {
        tracing::error!("delete_doc LanceDB error: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
    }
    if let Err(e) = state.index.fts.delete_document(&doc_id) {
        tracing::error!("delete_doc Tantivy error: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
    }
    let resp = DeleteResponse { deleted: true };
    (StatusCode::OK, Json(serde_json::to_value(resp).unwrap_or(json!({}))))
}

/// POST /v1/docs/:doc_id/location
/// Update the stored `location_uri` for all chunks of a document.
pub async fn update_location(
    State(state): State<Arc<SharedState>>,
    Path(doc_id): Path<String>,
    Json(body): Json<UpdateLocationBody>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = state.index.vector.update_location(&doc_id, &body.new_uri).await {
        tracing::error!("update_location error: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
    }
    let resp = UpdateLocationResponse { updated: true };
    (StatusCode::OK, Json(serde_json::to_value(resp).unwrap_or(json!({}))))
}
