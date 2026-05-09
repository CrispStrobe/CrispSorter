//! P11 Pillar 6 — pull-delta endpoint.
//!
//! `GET /v1/sync/since?ts=<unix_ms>&limit=<N>` returns up to `limit` rows
//! from the documents table whose `indexed_at` is greater than or equal
//! to `ts`, ordered ascending by `indexed_at`. The client uses this to
//! reconcile its local cache after offline periods.
//!
//! Only `chunk_index = 0` rows are returned — one row per document.
//! For full text re-fetch the client can issue follow-up `/v1/search`
//! requests using each row's `doc_id`.

use axum::{
    extract::{Query, State},
    Json,
    http::StatusCode,
};
use crisp_index_protocol::{IngestChunk, SearchHit};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::state::SharedState;

#[derive(Debug, Deserialize)]
pub struct SinceQuery {
    /// Unix milliseconds. Returns rows with `indexed_at >= ts`.
    /// Default: 0 (full snapshot).
    #[serde(default)]
    pub ts:    Option<i64>,
    /// Maximum rows to return. Default: 200, capped at 1000.
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct SinceResponse {
    /// Rows whose `indexed_at` ≥ the requested `ts`.
    pub rows:           Vec<SearchHit>,
    /// Largest `indexed_at` in the returned rows (or the request `ts` when
    /// the response is empty). Clients persist this as their `last_pull_ts`.
    pub max_indexed_at: i64,
    /// True when `rows.len() == limit`, signalling more pages may exist.
    /// The client should call again with `ts = max_indexed_at + 1`.
    pub has_more:       bool,
}

pub async fn since(
    State(state): State<Arc<SharedState>>,
    Query(q):     Query<SinceQuery>,
) -> Result<Json<SinceResponse>, (StatusCode, String)> {
    let ts    = q.ts.unwrap_or(0);
    let limit = q.limit.unwrap_or(200).min(1000);

    let batches = state
        .index
        .vector
        .rows_since(ts, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Reuse the existing batches-to-SearchHit converter via a tiny helper;
    // we don't have direct access to `batches_to_results` from the route
    // module, so we inline the small projection we need.
    let mut rows: Vec<SearchHit> = Vec::new();
    let mut max_indexed_at = ts;
    for batch in &batches {
        use arrow_array::{Array, Int32Array, StringArray, TimestampMillisecondArray};
        let n = batch.num_rows();
        let doc_id_col = batch.column_by_name("doc_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let uri_col    = batch.column_by_name("location_uri")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let owner_col  = batch.column_by_name("owner_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let title_col  = batch.column_by_name("title")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let author_col = batch.column_by_name("author")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let year_col   = batch.column_by_name("year")
            .and_then(|c| c.as_any().downcast_ref::<Int32Array>());
        let fname_col  = batch.column_by_name("filename")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let ext_col    = batch.column_by_name("ext")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let indexed_col = batch.column_by_name("indexed_at")
            .and_then(|c| c.as_any().downcast_ref::<TimestampMillisecondArray>());

        let (Some(doc_id_col), Some(uri_col), Some(owner_col)) = (doc_id_col, uri_col, owner_col)
        else { continue };

        for i in 0..n {
            if doc_id_col.is_null(i) { continue; }
            let mtime_ms = indexed_col.as_ref()
                .filter(|c| !c.is_null(i))
                .map(|c| c.value(i))
                .unwrap_or(0);
            if mtime_ms > max_indexed_at { max_indexed_at = mtime_ms; }

            rows.push(SearchHit {
                doc_id:       doc_id_col.value(i).to_owned(),
                location_uri: uri_col.value(i).to_owned(),
                owner_id:     owner_col.value(i).to_owned(),
                score:        0.0,                     // sync isn't a search
                title:        title_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i).to_owned()),
                author:       author_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i).to_owned()),
                year:         year_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i)),
                filename:     fname_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i).to_owned()),
                ext:          ext_col.as_ref().filter(|c| !c.is_null(i)).map(|c| c.value(i).to_owned()),
                language:     None,
                snippet:      String::new(),
                chunk_index:  0,
            });
        }
    }

    let has_more = rows.len() >= limit;
    Ok(Json(SinceResponse { rows, max_indexed_at, has_more }))
}

// Allow the unused import warning when the route module is included but
// the IngestChunk type isn't used here.
#[allow(dead_code)]
fn _no_warn(_: IngestChunk) {}
