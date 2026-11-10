use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use crisp_index_protocol::SearchRequest;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::state::SharedState;

/// `POST /v1/search` — text / vector / hybrid search.
///
/// Wire request type (`SearchRequest`) and response item (`SearchHit`)
/// come from `crisp-index-protocol`. The `mode` field selects the path:
/// `text` (BM25 only), `vector` (ANN only), or `hybrid` (RRF k=60 merge).
/// Defaults to `hybrid` when absent.
pub async fn search(state: State<Arc<SharedState>>, request: Json<SearchRequest>) -> Response {
    let metadata_version = request.metadata_filter_version.unwrap_or(crisp_index_protocol::METADATA_FILTER_VERSION);
    let embedding_version = request.embedding_query_version;
    let (status, body) = search_inner(state, request).await;
    let mut response = (status, body).into_response();
    if status.is_success() {
        if let Some(version) = embedding_version {
            response.headers_mut().insert(crisp_index_protocol::EMBEDDING_QUERY_VERSION_HEADER,version.to_string().parse().expect("integer header"));
        }
        response.headers_mut().insert(
            crisp_index_protocol::METADATA_FILTER_VERSION_HEADER,
            metadata_version.to_string().parse().expect("integer header"),
        );
        response.headers_mut().insert(
            crisp_index_protocol::QUERY_VERSION_HEADER,
            crisp_index_protocol::QUERY_LANGUAGE_VERSION
                .to_string()
                .parse()
                .expect("integer header"),
        );
    }
    response
}

async fn search_inner(
    State(state): State<Arc<SharedState>>,
    Json(req): Json<SearchRequest>,
) -> (StatusCode, Json<Value>) {
    let mode = req.mode.as_deref().unwrap_or("hybrid");
    let limit = req.limit.unwrap_or(20);
    if !matches!(mode, "text" | "vector" | "hybrid") {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":"mode must be text, vector, or hybrid"})),
        );
    }
    if limit == 0 || limit > 200 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":"limit must be between 1 and 200"})),
        );
    }
    if req
        .query_language_version
        .is_some_and(|version| version != crisp_index_protocol::QUERY_LANGUAGE_VERSION)
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"Unsupported query language version; negotiate /v1/capabilities"})),
        );
    }
    if let Some(query) = &req.query {
        if query.trim().is_empty() || query.len() > 8192 {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"query must contain between 1 and 8192 bytes"})),
            );
        }
    }
    if let Some(embedding) = &req.embedding {
        if let Err(error) = crate::index::validate_embedding(embedding, state.config.embed_dims) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":error.to_string()})),
            );
        }
    }

    if req.embedding_query_version.is_some_and(|version|version != 1)
        || (req.embedding_query.is_some() && req.embedding_query_version != Some(1))
        || req.embedding_query.as_ref().is_some_and(|q|q.trim().is_empty() || q.len() > 8192) {
        return (StatusCode::UNPROCESSABLE_ENTITY,Json(json!({"error":"A separate embedding query requires negotiated version 1 and 1 to 8192 bytes"})));
    }
    let filters = req.filters();
    if let Err(error) = filters.validate() {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": error})));
    }
    if req.metadata_filter_version.is_some_and(|version| version > crisp_index_protocol::METADATA_FILTER_VERSION || version < filters.required_metadata_filter_version()) {
        return (StatusCode::CONFLICT, Json(json!({"error": "Unsupported metadata filter version; negotiate /v1/capabilities"})));
    }
    if filters.needs_metadata_filter_version() && req.metadata_filter_version.is_none() {
        return (StatusCode::CONFLICT, Json(json!({"error": "Extended metadata filters require a negotiated metadata_filter_version"})));
    }

    if mode != "vector" {
        let Some(query) = req.query.as_deref() else {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":"query is required for lexical search"})),
            );
        };
        if let Err(error) = state.index.validate_query(query, &filters).await {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":error.to_string()})),
            );
        }
    }
    let results = match mode {
        "text" => {
            let query = match &req.query {
                Some(q) => q.as_str(),
                None => {
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({ "error": "query is required for text search" })),
                    )
                }
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
                            Json(
                                json!({ "error": "query is required for vector search when SERVER_EMBED is on" }),
                            ),
                        )
                    }
                };
                let Some(ref embedder) = state.embedder else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(
                            json!({ "error": "SERVER_EMBED is on but no server embedder is loaded" }),
                        ),
                    );
                };
                match embedder.embed_query(req.embedding_query.as_deref().unwrap_or(query)).await {
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
        _ => {
            // "hybrid" (default)
            let query = match &req.query {
                Some(q) => q.as_str(),
                None => {
                    return (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(json!({ "error": "query is required for hybrid search" })),
                    )
                }
            };
            let server_embedded = if let Some(e) = &req.embedding {
                Some(e.clone())
            } else if state.config.server_embed_enabled {
                let Some(ref embedder) = state.embedder else {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(
                            json!({ "error": "SERVER_EMBED is on but no server embedder is loaded" }),
                        ),
                    );
                };
                match embedder.embed_query(req.embedding_query.as_deref().unwrap_or(query)).await {
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
        Ok(r) => (
            StatusCode::OK,
            Json(serde_json::to_value(r).unwrap_or(json!([]))),
        ),
        Err(e) => {
            tracing::error!("search error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        }
    }
}
