//! Authenticated exact-owner lookup of actual stored member provenance.
use std::sync::Arc;
use axum::{extract::State,http::{HeaderMap,HeaderValue,StatusCode},response::Json};
use crisp_index_protocol::{ContainerMemberRequest,CONTAINER_MEMBERS_VERSION_HEADER,MAX_CONTAINER_MEMBER_RESPONSE_BYTES};
use serde_json::{json,Value};
use crate::state::SharedState;

pub async fn read_member(State(state):State<Arc<SharedState>>,Json(request):Json<ContainerMemberRequest>)
    ->(StatusCode,HeaderMap,Json<Value>) {
    let mut headers=HeaderMap::new();
    match state.index.read_container_member(&request).await {
        Ok(response)=>match serde_json::to_vec(&response) {
            Ok(encoded) if encoded.len()<=MAX_CONTAINER_MEMBER_RESPONSE_BYTES=>{
                headers.insert(CONTAINER_MEMBERS_VERSION_HEADER,HeaderValue::from_static("1"));
                (StatusCode::OK,headers,Json(serde_json::to_value(response).expect("serialized metadata")))
            }
            _=>(StatusCode::UNPROCESSABLE_ENTITY,headers,Json(json!({"error":"Stored member metadata exceeds its response limit"}))),
        },
        Err(error)=>(StatusCode::UNPROCESSABLE_ENTITY,headers,Json(json!({"error":error.to_string()}))),
    }
}
