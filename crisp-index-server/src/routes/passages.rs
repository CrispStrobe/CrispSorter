//! Authenticated, versioned access to exact indexed source passages.
use axum::{extract::State, http::StatusCode, response::{IntoResponse, Response}, Json};
use crisp_index_protocol::{StoredPassageRequest, STORED_PASSAGE_VERSION, STORED_PASSAGE_VERSION_HEADER};
use serde_json::json;
use std::sync::Arc;
use crate::state::SharedState;

/// `POST /v1/passages/read`; the enclosing `/v1` router requires its bearer token.
pub async fn read_passage(State(state): State<Arc<SharedState>>, Json(request): Json<StoredPassageRequest>) -> Response {
    if request.passage_version != STORED_PASSAGE_VERSION {
        return (StatusCode::CONFLICT, Json(json!({"error": "Unsupported stored passage version; negotiate /v1/capabilities"}))).into_response();
    }
    if let Err(error) = request.validate() {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": error}))).into_response();
    }
    match state.index.read_passage(&request).await {
        Ok(passage) => {
            let mut response = Json(passage).into_response();
            response.headers_mut().insert(STORED_PASSAGE_VERSION_HEADER,
                STORED_PASSAGE_VERSION.to_string().parse().expect("integer header"));
            response
        }
        Err(error) => {
            let status = match error.downcast_ref::<crate::index::PassageReadError>() {
                Some(crate::index::PassageReadError::Unavailable) => StatusCode::NOT_FOUND,
                Some(crate::index::PassageReadError::Ambiguous) => StatusCode::CONFLICT,
                None => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error": error.to_string()}))).into_response()
        }
    }
}
