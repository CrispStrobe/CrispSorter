use axum::{extract::State, http::StatusCode, response::Json};
use crisp_index_protocol::{SearchFilters, SearchRequest};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

/// `POST /v1/search` — text / vector / hybrid search.
///
/// Wire request type (`SearchRequest`) and response item (`SearchHit`)
/// come from `crisp-index-protocol`. The `mode` field selects the path:
/// `text` (BM25 only), `vector` (ANN only), or `hybrid` (RRF k=60 merge).
/// Defaults to `hybrid` when absent.
pub async fn search(
    State(state): State<Arc<SharedState>>,
    Json(req): Json<SearchRequest>,
) -> (StatusCode, Json<Value>) {
    let mode  = req.mode.as_deref().unwrap_or("hybrid");
    let limit = req.limit.unwrap_or(20).min(200);

    // Flatten the wire request's filter fields into the `SearchFilters`
    // struct used by the index layer (same fields, different shape).
    let filters = SearchFilters {
        owner_id: req.owner_id,
        language: req.language,
        year_min: req.year_min,
        year_max: req.year_max,
    };

    let results = match mode {
        "text" => {
            let query = match &req.query {
                Some(q) => q.as_str(),
                None => return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "query is required for text search" }))),
            };
            state.index.search_text(query, &filters, limit).await
        }
        "vector" => {
            let server_embedded = if let Some(e) = &req.embedding {
                Some(e.clone())
            } else if state.config.server_embed_enabled {
                let query = match &req.query {
                    Some(q) => q.as_str(),
                    None => {
                        return (
                            StatusCode::UNPROCESSABLE_ENTITY,
                            Json(json!({ "error": "query is required for vector search when SERVER_EMBED is on" })),
                        )
                    }
                };
                let Some(ref embedder) = state.embedder else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "SERVER_EMBED is on but no server embedder is loaded" })),
                    );
                };
                match embedder.embed_query(query).await {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::error!("server-side query embedding failed: {e:#}");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": e.to_string() })),
                        );
                    }
                }
            } else {
                None
            };
            let emb = match server_embedded.as_ref() {
                Some(e) => e.as_slice(),
                None => {
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({ "error": "embedding is required for vector search" })),
                    )
                }
            };
            state.index.search_vector(emb, &filters, limit).await
        }
        _ => {  // "hybrid" (default)
            let query = match &req.query {
                Some(q) => q.as_str(),
                None => return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": "query is required for hybrid search" }))),
            };
            let server_embedded = if let Some(e) = &req.embedding {
                Some(e.clone())
            } else if state.config.server_embed_enabled {
                let Some(ref embedder) = state.embedder else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "SERVER_EMBED is on but no server embedder is loaded" })),
                    );
                };
                match embedder.embed_query(query).await {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::error!("server-side hybrid query embedding failed: {e:#}");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": e.to_string() })),
                        );
                    }
                }
            } else {
                None
            };
            let emb = match server_embedded.as_ref() {
                Some(e) => e.as_slice(),
                None => {
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({ "error": "embedding is required for hybrid search" })),
                    )
                }
            };
            state.index.search_hybrid(query, emb, &filters, limit).await
        }
    };

    match results {
        Ok(r)  => (StatusCode::OK, Json(serde_json::to_value(r).unwrap_or(json!([])))),
        Err(e) => {
            tracing::error!("search error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })))
        }
    }
}
