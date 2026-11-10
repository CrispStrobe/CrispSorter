//! Bounded metadata synchronization with snapshot and timestamp tie cursors.
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use crisp_index_protocol::{SyncCursor, SyncPage, SYNC_CURSOR_VERSION};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::index::sync_cursor::SyncSnapshotUnavailable;
use crate::state::SharedState;

#[derive(Debug, Deserialize)]
pub struct SinceQuery {
    #[serde(default)]
    pub ts: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// New clients explicitly opt into continuation support on the first page.
    #[serde(default)]
    pub cursor_version: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

pub async fn since(
    State(state): State<Arc<SharedState>>,
    Query(query): Query<SinceQuery>,
) -> Result<Json<SyncPage>, (StatusCode, Json<Value>)> {
    let limit = query.limit.unwrap_or(200);
    if !(1..=1000).contains(&limit)
        || query.ts.is_some_and(|timestamp| timestamp < 0)
        || query
            .cursor_version
            .is_some_and(|version| version != SYNC_CURSOR_VERSION)
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":"Invalid sync timestamp, page limit, or cursor version"})),
        ));
    }
    let cursor = query
        .cursor
        .as_deref()
        .map(|encoded| {
            if encoded.len() > 8192 {
                return Err("Sync cursor is too large".to_owned());
            }
            let cursor: SyncCursor =
                serde_json::from_str(encoded).map_err(|_| "Malformed sync cursor".to_owned())?;
            if !cursor.is_valid()
                || query
                    .ts
                    .is_some_and(|timestamp| timestamp != cursor.since_ms)
            {
                return Err("Invalid or mismatched sync cursor".to_owned());
            }
            Ok(cursor)
        })
        .transpose()
        .map_err(|error| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error":error})),
            )
        })?;
    let since_ms = cursor
        .as_ref()
        .map_or(query.ts.unwrap_or(0), |cursor| cursor.since_ms);
    let _guard = state.index.read_guard().await.map_err(|error| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":error.to_string()})),
        )
    })?;
    let page = state
        .index
        .vector
        .rows_since(&state.index.fts, since_ms, cursor.as_ref(), limit)
        .await
        .map_err(|error| {
            if error.downcast_ref::<SyncSnapshotUnavailable>().is_some() {
                (
                    StatusCode::CONFLICT,
                    Json(json!({"error":error.to_string(),"code":"sync_snapshot_expired"})),
                )
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":error.to_string()})),
                )
            }
        })?;
    if (page.has_more || !page.deletions.is_empty())
        && query.cursor_version != Some(SYNC_CURSOR_VERSION)
    {
        // Older clients that can consume the entire delta still work. Refuse a
        // truncated timestamp-only page, which could silently skip tied rows.
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error":"This sync requires cursor pagination; upgrade the client before continuing",
                "code":"sync_cursor_required"
            })),
        ));
    }
    Ok(Json(page))
}
