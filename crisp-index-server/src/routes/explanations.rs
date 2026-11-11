//! Authenticated query evidence with separately negotiated receipts.
use crate::state::SharedState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use crisp_index_protocol::{
    QueryExplanationRequest, QUERY_EXPLANATION_VERSION, QUERY_EXPLANATION_VERSION_HEADER,
};
use serde_json::json;
use std::sync::Arc;

pub async fn explain(
    State(state): State<Arc<SharedState>>,
    Json(request): Json<QueryExplanationRequest>,
) -> Response {
    if request.explanation_version != QUERY_EXPLANATION_VERSION {
        return (StatusCode::CONFLICT,Json(json!({"error":"Unsupported query explanation version; negotiate /v1/capabilities"}))).into_response();
    }
    if let Err(error) = request.validate() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":error})),
        )
            .into_response();
    }
    match state.index.explain_passage(&request).await {
        Ok(explanation) => {
            let mut response = Json(explanation).into_response();
            response.headers_mut().insert(
                QUERY_EXPLANATION_VERSION_HEADER,
                QUERY_EXPLANATION_VERSION
                    .to_string()
                    .parse()
                    .expect("integer version"),
            );
            response
        }
        Err(error) => {
            let status = match error.downcast_ref::<crate::index::PassageReadError>() {
                Some(crate::index::PassageReadError::Unavailable) => StatusCode::NOT_FOUND,
                Some(crate::index::PassageReadError::Ambiguous) => StatusCode::CONFLICT,
                None => StatusCode::UNPROCESSABLE_ENTITY,
            };
            (status, Json(json!({"error":error.to_string()}))).into_response()
        }
    }
}
