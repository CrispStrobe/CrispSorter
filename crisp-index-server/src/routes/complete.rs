//! Authenticated complete search endpoints. The parent /v1 router supplies the
//! existing global bearer policy; exact owner/filter scope stays in receipts.
use axum::{extract::{Path,State}, http::StatusCode, response::{IntoResponse,Response}, Json};
use crisp_index_protocol::*;
use std::sync::Arc;
use crate::{index::CompleteError,state::SharedState};

fn response<T:serde::Serialize>(result:anyhow::Result<T>)->Response {
    match result {
        Ok(value)=> {
            let encoded=match serde_json::to_vec(&value) {
                Ok(encoded) if encoded.len()<=MAX_COMPLETE_RESPONSE_BYTES=>encoded,
                _=>return (StatusCode::UNPROCESSABLE_ENTITY,Json(serde_json::json!({"error":"Complete search response exceeds its encoded byte limit"}))).into_response(),
            };
            let mut response=([(axum::http::header::CONTENT_TYPE,"application/json")],encoded).into_response();
            response.headers_mut().insert(COMPLETE_SEARCH_VERSION_HEADER, "1".parse().unwrap());
            response
        },
        Err(error)=> {
            let status=match error.downcast_ref::<CompleteError>() {
                Some(CompleteError::Changed|CompleteError::Cancelled)=>StatusCode::CONFLICT,
                Some(CompleteError::Expired)=>StatusCode::GONE,
                Some(CompleteError::Busy)=>StatusCode::TOO_MANY_REQUESTS,
                Some(CompleteError::TimedOut)=>StatusCode::REQUEST_TIMEOUT,
                None=>StatusCode::UNPROCESSABLE_ENTITY,
            };
            (status,Json(serde_json::json!({"error":error.to_string()}))).into_response()
        }
    }
}
pub async fn generation(State(state):State<Arc<SharedState>>,Json(request):Json<CompleteSearchScope>)->Response {
    response(state.index.complete_generation(&request).await)
}
pub async fn summary(State(state):State<Arc<SharedState>>,Json(request):Json<CompleteSearchRequest>)->Response {
    response(state.index.complete_summary(request).await)
}
pub async fn page(State(state):State<Arc<SharedState>>,Json(request):Json<CompletePageRequest>)->Response {
    response(state.index.complete_page(request).await)
}
pub async fn cancel(State(state):State<Arc<SharedState>>,Path(request_id):Path<String>)->Response {
    response(state.index.cancel_complete(&request_id))
}
pub async fn destination(State(state):State<Arc<SharedState>>,Json(request):Json<CompleteDestinationRequest>)->Response {
    response(state.index.complete_destination(request).await)
}
