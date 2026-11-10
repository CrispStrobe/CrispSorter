use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use crisp_index_protocol::{DeleteResponse, UpdateLocationBody, UpdateLocationResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

#[derive(serde::Deserialize)]
pub struct DocumentScope {
    pub owner_id: Option<String>,
}

fn mutation_error(error:anyhow::Error)->(StatusCode,Json<Value>) {
    let status=if error.downcast_ref::<crate::index::AmbiguousDocument>().is_some() {StatusCode::CONFLICT} else {StatusCode::INTERNAL_SERVER_ERROR};
    (status,Json(json!({"error":error.to_string()})))
}

/// DELETE /v1/docs/:doc_id
/// Delete all LanceDB chunks + the Tantivy entry for this document.
pub async fn delete_doc(
    State(state): State<Arc<SharedState>>,
    Path(doc_id): Path<String>,
    Query(scope): Query<DocumentScope>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = state.index.delete_scoped_doc(&doc_id,scope.owner_id.as_deref()).await {
        return mutation_error(e);
    }
    let resp = DeleteResponse { deleted: true };
    (
        StatusCode::OK,
        Json(serde_json::to_value(resp).unwrap_or(json!({}))),
    )
}

/// POST /v1/docs/:doc_id/location
/// Update the stored `location_uri` for all chunks of a document.
pub async fn update_location(
    State(state): State<Arc<SharedState>>,
    Path(doc_id): Path<String>,
    Query(scope): Query<DocumentScope>,
    Json(body): Json<UpdateLocationBody>,
) -> (StatusCode, Json<Value>) {
    if let Err(e) = state.index.update_scoped_location(&doc_id, &body.new_uri,scope.owner_id.as_deref()).await {
        return mutation_error(e);
    }
    let resp = UpdateLocationResponse { updated: true };
    (
        StatusCode::OK,
        Json(serde_json::to_value(resp).unwrap_or(json!({}))),
    )
}
