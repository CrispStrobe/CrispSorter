use std::time::Instant;

use axum::{extract::State, http::StatusCode, response::Json};
use crisp_index_protocol::{ErrorResponse, IngestChunk, IngestResponse};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

/// `POST /v1/ingest` — store a single pre-embedded chunk.
///
/// Wire types (`IngestChunk`, `IngestResponse`, `ErrorResponse`) come
/// from `crisp-index-protocol`. Validates `embedding.len() ==
/// EMBED_DIMS`; rejects mismatches with HTTP 422.
pub async fn ingest(
    State(state): State<Arc<SharedState>>,
    Json(chunk): Json<IngestChunk>,
) -> (StatusCode, Json<Value>) {
    // Validate embedding dimension.
    if chunk.embedding.len() != state.config.embed_dims {
        let err = ErrorResponse {
            error: format!(
                "embedding length {} != expected {}",
                chunk.embedding.len(),
                state.config.embed_dims,
            ),
        };
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::to_value(err).unwrap_or(json!({}))),
        );
    }

    let owner_id  = chunk.owner_id.clone();
    let language  = chunk.language.clone();
    let doc_id    = chunk.doc_id.clone();
    let headings  = chunk.headings.as_ref()
        .map(|h| h.join(" "))
        .unwrap_or_default();
    let full_text = chunk.full_text.clone();
    let chunk_idx = chunk.chunk_index;

    let start = Instant::now();

    // Write to LanceDB.
    if let Err(e) = state.index.vector.ingest(&chunk).await {
        tracing::error!("LanceDB ingest error: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
    }

    // Write to Tantivy (only for the first chunk per document to avoid duplicates).
    if chunk_idx == 0 {
        if let Err(e) = state.index.fts.add_document(&doc_id, &owner_id, &language, &headings, &full_text) {
            tracing::error!("Tantivy ingest error: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })));
        }
    }

    let resp = IngestResponse {
        chunk_count: 1,
        write_time_ms: start.elapsed().as_millis() as u64,
    };

    (StatusCode::OK, Json(serde_json::to_value(resp).unwrap_or(json!({}))))
}
