/// Bearer token authentication middleware for crisp-index-server.
///
/// Validates the `Authorization: Bearer <token>` header against the
/// `CRISP_API_KEY` environment variable (loaded via dotenvy at startup).
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};

use crate::state::SharedState;

pub async fn require_bearer(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<SharedState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match token {
        Some(t) if t == state.config.api_key => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}
