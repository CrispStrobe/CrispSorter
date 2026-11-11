use crate::desktop::remote_client::RemoteClient;
use axum::{middleware, routing::{get, post}, response::IntoResponse, Json, Router};
use crisp_index_protocol::{IngestBatch, IngestChunk, SearchCapabilities, StoredPassage,
    StoredPassageRequest, MAX_STORED_PASSAGE_BYTES, MAX_STORED_PASSAGE_RESPONSE_BYTES,
    STORED_PASSAGE_VERSION, STORED_PASSAGE_VERSION_HEADER};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

fn capabilities() -> SearchCapabilities {
    SearchCapabilities { query_explanation_version: 1, query_plan_version: 1, api_version: 1, metadata_filter_version: 1, stored_passage_version: STORED_PASSAGE_VERSION,
        query_language_version: 1, fts_schema_version: 2, fts_ready: true, strict_hybrid_constraints: true,
        searchable_fields: vec!["body".into()], chunk_boundary_policy: "verified byte offsets".into(), ..Default::default() }
}

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(socket, router).await.unwrap(); });
    (format!("http://{address}"), server)
}

fn chunk(owner: &str, doc: &str, index: i32, text: &str) -> IngestChunk {
    serde_json::from_value(serde_json::json!({
        "doc_id":doc, "owner_id":owner, "chunk_index":index, "full_text":text, "embedding":[1.0,0.0],
        "chunk_start_char":index * 1000, "chunk_end_char":index * 1000 + text.len() as i32,
        "filename":"book.txt", "title":"Book", "ext":"txt", "language":"en",
        "location_uri":format!("test://{owner}/{doc}"), "source_hash":"stored-fingerprint-v2", "tags":[]
    })).unwrap()
}

#[tokio::test]
async fn authenticated_passage_route_reads_exact_owner_and_real_stored_navigation() {
    let dir = tempfile::tempdir().unwrap();
    let vector = Arc::new(crate::index::VectorStore::open_or_create(dir.path(), 2).await.unwrap());
    let fts = Arc::new(crate::fts::FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index = Arc::new(crate::index::SearchIndex::new(vector, fts));
    index.ensure_ready().await.unwrap();
    let id = "book' /?identifier";
    let owner = "reader'a";
    let unicode = "東".repeat(MAX_STORED_PASSAGE_BYTES / 3 + 10);
    index.ingest_batch(&IngestBatch { container_group_version: None, chunks: vec![
        chunk(owner, id, 2, "first passage"), chunk(owner, id, 5, "café middle"), chunk(owner, id, 9, "last passage"),
        chunk("other-owner", id, 5, "other owner's private text"), chunk(owner, "different", 5, "unrelated text"),
        chunk(owner, "unicode", 0, &unicode),
    ] }).await.unwrap();
    let state = Arc::new(crate::state::SharedState {
        index: index.clone(), queue: crate::queue::TaskQueue::open_or_create(dir.path(), Default::default()).unwrap(),
        embedder: None, config: crate::state::ServerConfig { api_key: "passage-secret".into(), port: 0,
            data_dir: dir.path().to_owned(), embed_dims: 2, server_embed_enabled: false,
            queue_lease_secs: 30, queue_retry_base_ms: 1000, queue_max_attempts: 3 },
    });
    let router = Router::new()
        .route("/v1/capabilities", get(crate::routes::capabilities::capabilities))
        .route("/v1/passages/read", post(crate::routes::passages::read_passage))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::require_bearer))
        .with_state(state);
    let (url, server) = serve(router).await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let request = StoredPassageRequest { passage_version: STORED_PASSAGE_VERSION,
        doc_id: id.into(), owner_id: owner.into(), chunk_index: 5 };
    for token in [None, Some("wrong-token")] {
        let mut call = http.post(format!("{url}/v1/passages/read")).json(&request);
        if let Some(token) = token { call = call.bearer_auth(token); }
        assert_eq!(call.send().await.unwrap().status(), 401);
    }
    let client = RemoteClient::new(&url, "passage-secret");
    let middle = client.read_passage(id, owner, 5).await.unwrap();
    assert_eq!(middle.text, "café middle");
    assert_eq!((middle.passage_number, middle.chunk_count), (2, 3));
    assert_eq!((middle.previous_chunk_index, middle.next_chunk_index), (Some(2), Some(9)));
    assert_eq!((middle.start_char, middle.end_char), (Some(5000), Some(5012)));
    assert_eq!(middle.source_hash.as_deref(), Some("stored-fingerprint-v2"));
    assert!(middle.indexed_at.unwrap() > 0);
    assert_eq!(client.read_passage(id, owner, 2).await.unwrap().previous_chunk_index, None);
    assert_eq!(client.read_passage(id, owner, 9).await.unwrap().next_chunk_index, None);
    assert_eq!(client.read_passage(id, "other-owner", 5).await.unwrap().text, "other owner's private text");
    let bounded = client.read_passage("unicode", owner, 0).await.unwrap();
    assert!(bounded.truncated);
    assert!(bounded.text.len() <= MAX_STORED_PASSAGE_BYTES && bounded.text.len() > MAX_STORED_PASSAGE_BYTES - 3);
    assert_eq!(bounded.end_char, Some(unicode.len() as i32));

    for (body, status) in [
        (serde_json::json!({"passage_version":1,"doc_id":id,"chunk_index":5}), 422),
        (serde_json::json!({"passage_version":999,"doc_id":id,"owner_id":owner,"chunk_index":5}), 409),
        (serde_json::json!({"passage_version":1,"doc_id":id,"owner_id":owner,"chunk_index":-1}), 422),
        (serde_json::json!({"passage_version":1,"doc_id":id,"owner_id":"missing-owner","chunk_index":5}), 404),
        (serde_json::json!({"passage_version":1,"doc_id":id,"owner_id":owner,"chunk_index":4}), 404),
        (serde_json::json!({"passage_version":1,"doc_id":"' OR 1=1 --","owner_id":owner,"chunk_index":5}), 404),
    ] {
        let response = http.post(format!("{url}/v1/passages/read")).bearer_auth("passage-secret")
            .json(&body).send().await.unwrap();
        assert_eq!(response.status(), status);
        assert!(!response.text().await.unwrap().contains("private text"));
    }
    let mut changed = chunk(owner, id, 5, "updated current text");
    changed.source_hash = "new-current-fingerprint".into();
    index.ingest_batch(&IngestBatch { container_group_version: None, chunks: vec![changed] }).await.unwrap();
    let current = client.read_passage(id, owner, 5).await.unwrap();
    assert_eq!(current.text, "updated current text");
    assert_eq!(current.source_hash.as_deref(), Some("new-current-fingerprint"));
    assert_eq!((current.passage_number, current.chunk_count), (2, 3));
    server.abort();
}

#[tokio::test]
async fn legacy_and_incompatible_servers_never_receive_passage_requests() {
    for mode in 0..4 {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut router = Router::new().route("/v1/passages/read", post(move || {
            let seen = seen.clone(); async move { seen.fetch_add(1, Ordering::SeqCst); Json(serde_json::json!({})) }
        }));
        if mode != 0 {
            router = router.route("/v1/capabilities", get(move || async move {
                let mut value = serde_json::to_value(capabilities()).unwrap();
                match mode {
                    1 => { value.as_object_mut().unwrap().remove("stored_passage_version"); }
                    2 => value["stored_passage_version"] = 999.into(),
                    _ => value["fts_ready"] = false.into(),
                }
                Json(value)
            }));
        }
        let (url, server) = serve(router).await;
        let client = RemoteClient::new(url, "test");
        let error = client.read_passage("book", "reader", 0).await.unwrap_err().to_string();
        assert!(error.contains("stored passage inspection"), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }
}

fn response_fixture() -> StoredPassage {
    StoredPassage { container_member:None, container_source:None, container_coverage:None, passage_version: STORED_PASSAGE_VERSION, doc_id: "book".into(), owner_id: "reader".into(),
        text: "stored text".into(), start_char: None, end_char: None, offset_unit: "utf8_bytes".into(), truncated: false,
        chunk_index: 2, passage_number: 1, chunk_count: 1, previous_chunk_index: None, next_chunk_index: None,
        source_hash: Some("fingerprint".into()), indexed_at: Some(1234) }
}

#[tokio::test]
async fn passage_client_rejects_unconfirmed_mismatched_and_unbounded_responses() {
    for mode in 0..9 {
        let router = Router::new().route("/v1/capabilities", get(|| async { Json(capabilities()) }))
            .route("/v1/passages/read", post(move || async move {
                let mut value = response_fixture();
                match mode {
                    1 => value.owner_id = "another-owner".into(),
                    2 => value.doc_id = "another-document".into(),
                    3 => value.chunk_index = 5,
                    4 => value.passage_version = 999,
                    5 => value.next_chunk_index = Some(1),
                    6 => { value.start_char = Some(1); value.end_char = Some(2); }
                    7 => value.text = "x".repeat(MAX_STORED_PASSAGE_BYTES + 1),
                    _ => {}
                }
                let mut response = if mode == 8 {
                    // Deliberately omit Content-Length to test the streaming
                    // ceiling as well as the declared-length fast rejection.
                    axum::response::Response::new(axum::body::Body::from_stream(futures::stream::iter([
                        Ok::<_, std::convert::Infallible>(vec![b' '; MAX_STORED_PASSAGE_RESPONSE_BYTES + 1]),
                    ])))
                } else { Json(value).into_response() };
                if mode != 0 { response.headers_mut().insert(STORED_PASSAGE_VERSION_HEADER, "1".parse().unwrap()); }
                response
            }));
        let (url, server) = serve(router).await;
        let client = RemoteClient::new(url, "test");
        assert!(client.read_passage("book", "reader", 2).await.is_err(), "mode {mode}");
        server.abort();
    }
}
