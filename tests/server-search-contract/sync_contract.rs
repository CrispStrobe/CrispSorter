#[path = "../../src-tauri/src/sync/pull.rs"]
mod client;

use axum::{extract::Query, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use client::{PendingPullPage, PullCheckpoint};
use crisp_index_protocol::{SearchHit, SyncCursor, SyncPage, SYNC_CURSOR_VERSION};
use rusqlite::OptionalExtension;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn row(id: &str) -> SearchHit {
    SearchHit {
        doc_id: id.into(),
        owner_id: "owner'one".into(),
        location_uri: format!("test://{id}"),
        title: None,
        author: None,
        year: None,
        filename: None,
        ext: None,
        language: Some("en".into()),
        snippet: String::new(),
        score: 0.0,
        chunk_index: 0,
    }
}
fn cursor(id: &str) -> String {
    serde_json::to_string(&SyncCursor {
        version: SYNC_CURSOR_VERSION,
        snapshot_version: 3,
        since_ms: 0,
        indexed_at: 42,
        owner_id: "owner'one".into(),
        doc_id: id.into(),
    })
    .unwrap()
}
fn page(ids: &[&str], has_more: bool) -> SyncPage {
    SyncPage {
        rows: ids.iter().map(|id| row(id)).collect(),
        max_indexed_at: 42,
        has_more,
        next_cursor: has_more.then(|| cursor(ids.last().unwrap())),
        snapshot_version: Some(3),
    }
}
async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(socket, router).await.unwrap();
    });
    (format!("http://{address}"), task)
}
fn state(connection: &rusqlite::Connection, key: &str) -> Option<String> {
    connection
        .query_row("SELECT value FROM sync_state WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .unwrap()
}

#[test]
fn pull_checkpoint_ack_is_atomic_resumable_and_rejects_racing_regression() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let mut connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE sync_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);").unwrap();
    let initial = PullCheckpoint::for_remote(None, 0, "http://server/").unwrap();
    let first = page(&["a", "b"], true);
    let pending = PendingPullPage {
        checkpoint_after_apply: initial.after_page(&first, 2).unwrap(),
        page: first,
        expected_checkpoint: None,
    };
    assert!(state(&connection, client::CHECKPOINT_KEY).is_none());

    connection.execute_batch("CREATE TRIGGER fail_watermark BEFORE INSERT ON sync_state WHEN NEW.key='last_pull_ts' BEGIN SELECT RAISE(ABORT,'simulated storage failure'); END;").unwrap();
    assert!(client::acknowledge_page(&mut connection, &pending).is_err());
    assert!(state(&connection, client::CHECKPOINT_KEY).is_none());
    connection
        .execute_batch("DROP TRIGGER fail_watermark;")
        .unwrap();
    client::acknowledge_page(&mut connection, &pending).unwrap();
    assert_eq!(state(&connection, "last_pull_ts").as_deref(), Some("0"));
    drop(connection);

    let mut reopened = rusqlite::Connection::open(&path).unwrap();
    let encoded = state(&reopened, client::CHECKPOINT_KEY).unwrap();
    let resumed = PullCheckpoint::for_remote(Some(&encoded), 0, "http://server").unwrap();
    assert_eq!(resumed.cursor, Some(cursor("b")));
    let final_page = page(&["c"], false);
    let completed = PendingPullPage {
        checkpoint_after_apply: resumed.after_page(&final_page, 2).unwrap(),
        page: final_page,
        expected_checkpoint: Some(encoded),
    };
    client::acknowledge_page(&mut reopened, &completed).unwrap();
    assert_eq!(state(&reopened, "last_pull_ts").as_deref(), Some("42"));
    let final_state = state(&reopened, client::CHECKPOINT_KEY).unwrap();
    assert!(
        PullCheckpoint::for_remote(Some(&final_state), 0, "http://server")
            .unwrap()
            .cursor
            .is_none()
    );
    assert!(client::acknowledge_page(&mut reopened, &pending).is_err());
    assert_eq!(
        state(&reopened, client::CHECKPOINT_KEY).unwrap(),
        final_state
    );
    assert_eq!(
        PullCheckpoint::for_remote(Some(&final_state), 42, "http://different-server")
            .unwrap()
            .since_ms,
        0
    );
}

#[tokio::test]
async fn pull_transport_encodes_cursor_and_validates_bounded_progress() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let router = Router::new().route(
        "/v1/sync/since",
        get(move |Query(query): Query<HashMap<String, String>>| {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                assert_eq!(query.get("cursor_version").map(String::as_str), Some("1"));
                assert_eq!(query.get("ts").map(String::as_str), Some("0"));
                if let Some(encoded) = query.get("cursor") {
                    assert_eq!(encoded, &cursor("b"));
                    Json(page(&["c"], false))
                } else {
                    Json(page(&["a", "b"], true))
                }
            }
        }),
    );
    let (url, task) = serve(router).await;
    let client_http = reqwest::Client::builder().no_proxy().build().unwrap();
    let initial = PullCheckpoint::for_remote(None, 0, &url).unwrap();
    let (first, next) = client::fetch_page(&client_http, &initial, "", 2)
        .await
        .unwrap();
    assert_eq!(first.rows.len(), 2);
    assert_eq!(next.since_ms, 0);
    let (last, completed) = client::fetch_page(&client_http, &next, "", 2)
        .await
        .unwrap();
    assert_eq!(last.rows[0].doc_id, "c");
    assert_eq!(completed.since_ms, 42);
    assert!(completed.cursor.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(next.after_page(&page(&["a", "b"], true), 2).is_err());
    task.abort();
}

#[tokio::test]
async fn expired_snapshot_restarts_at_completed_boundary_without_skipping() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let router = Router::new().route(
        "/v1/sync/since",
        get(move |Query(query): Query<HashMap<String, String>>| {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                assert_eq!(query["ts"], "0");
                if query.contains_key("cursor") {
                    (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({"code":"sync_snapshot_expired"})),
                    )
                        .into_response()
                } else {
                    Json(page(&["a", "b", "c"], false)).into_response()
                }
            }
        }),
    );
    let (url, task) = serve(router).await;
    let initial = PullCheckpoint::for_remote(None, 0, &url).unwrap();
    let resume = initial.after_page(&page(&["a", "b"], true), 3).unwrap();
    let (result, completed) = client::fetch_page(
        &reqwest::Client::builder().no_proxy().build().unwrap(),
        &resume,
        "",
        3,
    )
    .await
    .unwrap();
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row.doc_id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b", "c"]
    );
    assert!(completed.cursor.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn malformed_or_unpageable_remote_payload_never_becomes_an_empty_success() {
    for body in [
        serde_json::json!({"rows":"bad","max_indexed_at":42,"has_more":false}),
        serde_json::json!({"rows":[row("a")],"max_indexed_at":42,"has_more":true}),
    ] {
        let router = Router::new().route(
            "/v1/sync/since",
            get(move || {
                let body = body.clone();
                async move { Json(body) }
            }),
        );
        let (url, task) = serve(router).await;
        let initial = PullCheckpoint::for_remote(None, 0, &url).unwrap();
        assert!(client::fetch_page(
            &reqwest::Client::builder().no_proxy().build().unwrap(),
            &initial,
            "",
            2
        )
        .await
        .is_err());
        task.abort();
    }
}

#[tokio::test]
async fn actual_sync_route_supports_safe_legacy_pages_and_cursor_clients() {
    let dir = tempfile::tempdir().unwrap();
    let vector = Arc::new(
        crate::index::VectorStore::open_or_create(dir.path(), 2)
            .await
            .unwrap(),
    );
    let fts = Arc::new(crate::fts::FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index = Arc::new(crate::index::SearchIndex::new(vector, fts));
    index.ensure_ready().await.unwrap();
    let chunks = ["b", "a"]
        .iter()
        .map(|id| {
            serde_json::from_value(serde_json::json!({
        "doc_id":id,"owner_id":"owner","chunk_index":0,"chunk_total":1,
        "full_text":"body","embedding":[1.0,0.0],"filename":format!("{id}.txt"),
        "ext":"txt","language":"en","location_uri":format!("test://{id}"),"source_hash":id,"tags":[]
    })).unwrap()
        })
        .collect();
    index
        .ingest_batch(&crisp_index_protocol::IngestBatch { chunks })
        .await
        .unwrap();
    let state = Arc::new(crate::state::SharedState {
        index,
        queue: crate::queue::TaskQueue::open_or_create(dir.path(), Default::default()).unwrap(),
        embedder: None,
        config: crate::state::ServerConfig {
            api_key: String::new(),
            port: 0,
            data_dir: dir.path().to_owned(),
            embed_dims: 2,
            server_embed_enabled: false,
            queue_lease_secs: 30,
            queue_retry_base_ms: 1000,
            queue_max_attempts: 3,
        },
    });
    let (url, task) = serve(
        Router::new()
            .route("/v1/sync/since", get(crate::routes::sync::since))
            .with_state(state),
    )
    .await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        http.get(format!("{url}/v1/sync/since?ts=0&limit=1"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        http.get(format!("{url}/v1/sync/since?ts=0&limit=2"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    for query in ["limit=0", "limit=1001", "cursor_version=2", "cursor=bad"] {
        assert_eq!(
            http.get(format!("{url}/v1/sync/since?{query}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    let initial = PullCheckpoint::for_remote(None, 0, &url).unwrap();
    let (first, next) = client::fetch_page(&http, &initial, "", 1).await.unwrap();
    let (last, completed) = client::fetch_page(&http, &next, "", 1).await.unwrap();
    assert_eq!(first.rows.len(), 1);
    assert_eq!(last.rows.len(), 1);
    assert_ne!(first.rows[0].doc_id, last.rows[0].doc_id);
    assert_eq!(first.snapshot_version, last.snapshot_version);
    assert!(completed.cursor.is_none());
    task.abort();
}
