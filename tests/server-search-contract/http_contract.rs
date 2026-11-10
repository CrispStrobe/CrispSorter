use crate::desktop::{remote_client::RemoteClient, IndexBackend};
use axum::{
    routing::{get, post},
    Json, Router,
};
use crisp_index_protocol::{
    SearchCapabilities, SearchFilters, FTS_SCHEMA_VERSION, QUERY_LANGUAGE_VERSION,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn capabilities() -> SearchCapabilities {
    SearchCapabilities {
        api_version: 1,
        query_language_version: QUERY_LANGUAGE_VERSION,
        fts_schema_version: FTS_SCHEMA_VERSION,
        fts_ready: true,
        strict_hybrid_constraints: true,
        searchable_fields: vec!["title".into(), "body".into(), "headings".into()],
        chunk_boundary_policy: "verified offsets".into(),
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

#[tokio::test]
async fn legacy_server_receives_plain_queries_but_never_explicit_syntax() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let router = Router::new().route(
        "/v1/search",
        post(
            move |Json(request): Json<crisp_index_protocol::SearchRequest>| {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    assert!(request.query_language_version.is_none());
                    Json(Vec::<crisp_index_protocol::SearchHit>::new())
                }
            },
        ),
    );
    let (url, server) = serve(router).await;
    let client = RemoteClient::new(url, "");
    for query in [
        "alpha AND beta",
        "title:alpha",
        "\"alpha beta\"",
        "alpha pre/1 beta",
        "alpha~1",
        "alpha*",
    ] {
        assert!(client
            .search_text(query, &SearchFilters::default(), 20)
            .await
            .unwrap_err()
            .to_string()
            .contains("does not advertise"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(client
        .search_text("alpha beta", &Default::default(), 20)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn negotiated_results_require_the_server_version_header() {
    let router = Router::new()
        .route("/v1/capabilities", get(|| async { Json(capabilities()) }))
        .route(
            "/v1/search",
            post(|| async { Json(Vec::<crisp_index_protocol::SearchHit>::new()) }),
        );
    let (url, server) = serve(router).await;
    let client = RemoteClient::new(url, "");
    assert!(client
        .search_text("alpha AND beta", &Default::default(), 20)
        .await
        .unwrap_err()
        .to_string()
        .contains("did not confirm"));
    server.abort();
}

#[tokio::test]
async fn incompatible_capabilities_fail_before_sending_the_query() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let router = Router::new()
        .route(
            "/v1/capabilities",
            get(|| async {
                let mut value = capabilities();
                value.strict_hybrid_constraints = false;
                Json(value)
            }),
        )
        .route(
            "/v1/search",
            post(move || {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    Json(Vec::<crisp_index_protocol::SearchHit>::new())
                }
            }),
        );
    let (url, server) = serve(router).await;
    let client = RemoteClient::new(url, "");
    assert!(client
        .search_hybrid("alpha AND beta", &[1.0, 0.0], &Default::default(), 20)
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn actual_search_route_rejects_invalid_requests_before_embedding_and_confirms_grammar() {
    let dir = tempfile::tempdir().unwrap();
    let vector = Arc::new(
        crate::index::VectorStore::open_or_create(dir.path(), 2)
            .await
            .unwrap(),
    );
    let fts = Arc::new(crate::fts::FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index = Arc::new(crate::index::SearchIndex::new(vector, fts));
    index.ensure_ready().await.unwrap();
    let state = Arc::new(crate::state::SharedState {
        index,
        queue: crate::queue::TaskQueue::open_or_create(dir.path(), Default::default()).unwrap(),
        embedder: Some(crate::embedder::ServerEmbedder),
        config: crate::state::ServerConfig {
            api_key: "".into(),
            port: 0,
            data_dir: dir.path().to_owned(),
            embed_dims: 2,
            server_embed_enabled: true,
            queue_lease_secs: 30,
            queue_retry_base_ms: 1000,
            queue_max_attempts: 3,
        },
    });
    let router = Router::new()
        .route(
            "/v1/capabilities",
            get(crate::routes::capabilities::capabilities),
        )
        .route("/v1/search", post(crate::routes::search::search))
        .with_state(state);
    let (url, server) = serve(router).await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    for body in [
        serde_json::json!({"mode":"hybrid","query":"alpha AND"}),
        serde_json::json!({"mode":"text","query":"alpha","limit":0}),
        serde_json::json!({"mode":"bogus","query":"alpha"}),
        serde_json::json!({"mode":"hybrid","query":"title:"}),
    ] {
        assert_eq!(
            http.post(format!("{url}/v1/search"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            422
        );
    }
    assert_eq!(crate::embedder::CALLS.load(Ordering::SeqCst), 0);
    let mismatch = http
        .post(format!("{url}/v1/search"))
        .json(&serde_json::json!({"mode":"text","query":"alpha","query_language_version":999}))
        .send()
        .await
        .unwrap();
    assert_eq!(mismatch.status(), 409);
    let client = RemoteClient::new(url, "");
    assert!(client
        .search_text("alpha AND beta", &Default::default(), 20)
        .await
        .unwrap()
        .is_empty());
    server.abort();
}

#[tokio::test]
async fn scoped_http_mutations_encode_ids_and_preserve_other_owners() {
    let dir = tempfile::tempdir().unwrap();
    let vector = Arc::new(
        crate::index::VectorStore::open_or_create(dir.path(), 2)
            .await
            .unwrap(),
    );
    let fts = Arc::new(crate::fts::FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index = Arc::new(crate::index::SearchIndex::new(vector, fts));
    index.ensure_ready().await.unwrap();
    let id = "shared /?id";
    let chunks=["owner'a","owner-b"].into_iter().map(|owner|serde_json::from_value(serde_json::json!({
        "doc_id":id,"chunk_index":0,"chunk_total":1,"full_text":"alpha body","embedding":[1.0,0.0],
        "filename":"copy.txt","title":owner,"ext":"txt","language":"en","location_uri":format!("test://{owner}"),
        "owner_id":owner,"source_hash":"identical content"
    })).unwrap()).collect();
    index
        .ingest_batch(&crisp_index_protocol::IngestBatch { chunks })
        .await
        .unwrap();
    let state = Arc::new(crate::state::SharedState {
        index: index.clone(),
        queue: crate::queue::TaskQueue::open_or_create(dir.path(), Default::default()).unwrap(),
        embedder: None,
        config: crate::state::ServerConfig {
            api_key: "".into(),
            port: 0,
            data_dir: dir.path().to_owned(),
            embed_dims: 2,
            server_embed_enabled: false,
            queue_lease_secs: 30,
            queue_retry_base_ms: 1000,
            queue_max_attempts: 3,
        },
    });
    let router = Router::new()
        .route(
            "/v1/capabilities",
            get(crate::routes::capabilities::capabilities),
        )
        .route("/v1/search", post(crate::routes::search::search))
        .route(
            "/v1/docs/{doc_id}",
            axum::routing::delete(crate::routes::docs::delete_doc),
        )
        .route(
            "/v1/docs/{doc_id}/location",
            post(crate::routes::docs::update_location),
        )
        .with_state(state);
    let (url, server) = serve(router).await;
    let client = RemoteClient::new(url, "");
    assert!(client
        .delete_doc(id)
        .await
        .unwrap_err()
        .to_string()
        .contains("409"));
    assert!(client
        .update_location(id, "test://ambiguous")
        .await
        .unwrap_err()
        .to_string()
        .contains("409"));
    client
        .update_location_scoped(id, "test://moved", Some("owner'a"))
        .await
        .unwrap();
    let a = SearchFilters {
        owner_id: Some("owner'a".into()),
        ..Default::default()
    };
    let b = SearchFilters {
        owner_id: Some("owner-b".into()),
        ..Default::default()
    };
    let changed = client.search_text("alpha", &a, 20).await.unwrap();
    assert_eq!(changed[0].location_uri, "test://moved");
    assert!(changed[0].indexed_at > 0);
    assert_eq!(
        client.search_text("alpha", &b, 20).await.unwrap()[0].location_uri,
        "test://owner-b"
    );
    client.delete_doc_scoped(id, Some("owner'a")).await.unwrap();
    assert!(client
        .search_text("alpha", &a, 20)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(client.search_text("alpha", &b, 20).await.unwrap().len(), 1);
    assert_eq!(
        index
            .fts
            .tombstones_page(0, index.fts.sync_horizon().unwrap(), None, 10)
            .unwrap()[0]
            .owner_id,
        "owner'a"
    );
    server.abort();
}

#[tokio::test]
async fn exact_uri_http_move_preserves_copies_and_rejects_ambiguous_sources_and_destinations() {
    let dir = tempfile::tempdir().unwrap();
    let vector = Arc::new(
        crate::index::VectorStore::open_or_create(dir.path(), 2)
            .await
            .unwrap(),
    );
    let fts = Arc::new(crate::fts::FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index = Arc::new(crate::index::SearchIndex::new(vector, fts));
    index.ensure_ready().await.unwrap();
    let old = "/source/file's.txt";
    let new = "/sorted/renamed.txt";
    let wire = |owner: &str, id: &str, uri: &str, chunk: i32, total: i32| {
        serde_json::from_value(serde_json::json!({
        "doc_id":id,"chunk_index":chunk,"chunk_total":total,"full_text":"alpha body","embedding":[1.0,0.0],
        "filename":"copy.txt","title":id,"ext":"txt","language":"en","location_uri":uri,
        "owner_id":owner,"source_hash":"preserved content"
    })).unwrap()
    };
    index
        .ingest_batch(&crisp_index_protocol::IngestBatch {
            chunks: vec![
                wire("a", "shared", old, 0, 2),
                wire("a", "shared", old, 1, 2),
                wire("b", "shared", "/other-machine/source.txt", 0, 1),
                wire("c", "ambiguous-c", "/ambiguous.txt", 0, 1),
                wire("d", "ambiguous-d", "/ambiguous.txt", 0, 1),
                wire("a", "destination", "/occupied.txt", 0, 1),
            ],
        })
        .await
        .unwrap();
    let state = Arc::new(crate::state::SharedState {
        index: index.clone(),
        queue: crate::queue::TaskQueue::open_or_create(dir.path(), Default::default()).unwrap(),
        embedder: None,
        config: crate::state::ServerConfig {
            api_key: "".into(),
            port: 0,
            data_dir: dir.path().to_owned(),
            embed_dims: 2,
            server_embed_enabled: false,
            queue_lease_secs: 30,
            queue_retry_base_ms: 1000,
            queue_max_attempts: 3,
        },
    });
    let router = Router::new()
        .route(
            "/v1/docs/location/by-uri",
            post(crate::routes::docs::update_location_by_uri),
        )
        .route(
            "/v1/docs/{doc_id}/location",
            post(crate::routes::docs::update_location),
        )
        .with_state(state);
    let (url, server) = serve(router).await;
    let client = RemoteClient::new(url, "");
    let a = SearchFilters {
        owner_id: Some("a".into()),
        ..Default::default()
    };
    let before = index
        .search_text("title:shared", &a, 20)
        .await
        .unwrap()
        .remove(0);
    client.update_location_by_uri(old, new).await.unwrap();
    let after = index
        .search_text("title:shared", &a, 20)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(after.doc_id, before.doc_id);
    assert_eq!(after.owner_id, before.owner_id);
    assert_eq!(after.source_hash, before.source_hash);
    assert_eq!(after.location_uri, new);
    assert!(after.indexed_at > before.indexed_at);
    let b = SearchFilters {
        owner_id: Some("b".into()),
        ..Default::default()
    };
    assert_eq!(
        index.search_text("alpha", &b, 20).await.unwrap()[0].location_uri,
        "/other-machine/source.txt"
    );
    for (source, destination) in [("/ambiguous.txt", "/unsafe.txt"), (new, "/occupied.txt")] {
        assert!(client
            .update_location_by_uri(source, destination)
            .await
            .unwrap_err()
            .to_string()
            .contains("409"));
    }
    assert_eq!(
        index.search_text("title:shared", &a, 20).await.unwrap()[0].location_uri,
        new
    );
    assert_eq!(
        index.search_text("title:shared", &a, 20).await.unwrap()[0].indexed_at,
        after.indexed_at
    );
    assert_eq!(index.stats().await.unwrap(), (6, 5));
    client
        .update_location_by_uri("/unindexed.txt", "/unused.txt")
        .await
        .unwrap();
    assert!(client
        .update_location_by_uri("", new)
        .await
        .unwrap_err()
        .to_string()
        .contains("422"));
    server.abort();
}
