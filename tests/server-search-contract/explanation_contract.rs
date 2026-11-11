use crate::desktop::remote_client::RemoteClient;
use axum::{
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use crisp_index_protocol::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    (
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(socket, router).await.unwrap() }),
    )
}
fn capabilities() -> SearchCapabilities {
    SearchCapabilities {
        api_version: 1,
        query_plan_version: 1,
        query_explanation_version: QUERY_EXPLANATION_VERSION,
        stored_passage_version: STORED_PASSAGE_VERSION,
        query_language_version: QUERY_LANGUAGE_VERSION,
        fts_schema_version: FTS_SCHEMA_VERSION,
        fts_ready: true,
        strict_hybrid_constraints: true,
        ..Default::default()
    }
}
fn request() -> QueryExplanationRequest {
    QueryExplanationRequest {
        explanation_version: 1,
        doc_id: "book".into(),
        owner_id: "reader".into(),
        chunk_index: 0,
        query: "cafe".into(),
        lexical_query: "cafe".into(),
        query_intent: "advanced".into(),
        mode: "text".into(),
        fuzzy: false,
        synonyms: false,
    }
}
fn response() -> QueryExplanation {
    QueryExplanation {
        explanation_version: 1,
        query: "cafe".into(),
        lexical_query: "cafe".into(),
        query_intent: "advanced".into(),
        mode: "text".into(),
        fuzzy: false,
        synonyms: false,
        passage: StoredPassage {
            passage_version: 1,
            doc_id: "book".into(),
            owner_id: "reader".into(),
            text: "café red fox".into(),
            start_char: Some(0),
            end_char: Some(13),
            offset_unit: "utf8_bytes".into(),
            truncated: false,
            chunk_index: 0,
            passage_number: 1,
            chunk_count: 1,
            previous_chunk_index: None,
            next_chunk_index: None,
            source_hash: Some("source".into()),
            indexed_at: Some(123),
            container_member: None,
            container_source: None,
            container_coverage: None,
        },
        lexical_document_match: Some(true),
        semantic_evidence: false,
        clauses: Some(ExplainClause {
            kind: "term".into(),
            label: "cafe".into(),
            matched: true,
            field: None,
            matched_fields: vec![],
            children: vec![],
        }),
        spans: vec![EvidenceSpan {
            start: 0,
            end: 5,
            kind: "term".into(),
            clause: "cafe".into(),
        }],
        spans_truncated: false,
        scope: QUERY_EXPLANATION_SCOPE.into(),
        notes: vec![],
    }
}
fn chunk(owner: &str, index: i32, text: &str) -> IngestChunk {
    serde_json::from_value(serde_json::json!({"doc_id":"book","owner_id":owner,"chunk_index":index,
        "full_text":text,"embedding":[1.0,0.0],"chunk_start_char":index*1000,"chunk_end_char":index*1000+text.len() as i32,
        "filename":"book.txt","title":"Annual report","location_uri":format!("test://{owner}/book"),
        "ext":"txt","language":"en","source_hash":"fingerprint","tags":[]})).unwrap()
}

#[tokio::test]
async fn actual_route_binds_owner_query_plan_full_document_truth_and_selected_source_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let vector = Arc::new(
        crate::index::VectorStore::open_or_create(dir.path(), 2)
            .await
            .unwrap(),
    );
    let fts = Arc::new(crate::fts::FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index = Arc::new(crate::index::SearchIndex::new(vector, fts));
    index.ensure_ready().await.unwrap();
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None,
            chunks: vec![
                chunk("reader", 0, "café red fox."),
                chunk("reader", 4, "silver moon"),
                chunk("other", 0, "private violet text"),
            ],
        })
        .await
        .unwrap();
    let state = Arc::new(crate::state::SharedState {
        index: index.clone(),
        queue: crate::queue::TaskQueue::open_or_create(dir.path(), Default::default()).unwrap(),
        embedder: None,
        config: crate::state::ServerConfig {
            api_key: "explain-secret".into(),
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
        .route(
            "/v1/passages/explain",
            post(crate::routes::explanations::explain),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_bearer,
        ))
        .with_state(state);
    let (url, server) = serve(router).await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        http.post(format!("{url}/v1/passages/explain"))
            .json(&request())
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let client = RemoteClient::new(&url, "explain-secret");
    let result = client.explain_passage(&request()).await.unwrap();
    assert_eq!(result.lexical_document_match, Some(true));
    assert_eq!(
        &result.passage.text[result.spans[0].start..result.spans[0].end],
        "café"
    );
    assert_eq!(
        (result.passage.passage_number, result.passage.chunk_count),
        (1, 2)
    );
    for (query, matched, expected) in [
        ("red AND moon", true, vec!["red"]),
        ("\"fox silver\"", false, vec![]),
        ("red pre/0 fox", true, vec!["red fox"]),
        ("title:annual", true, vec![]),
        ("rde~1", true, vec!["red"]),
        ("private", false, vec![]),
    ] {
        let mut request = request();
        request.query = query.into();
        request.lexical_query = query.into();
        let result = client.explain_passage(&request).await.unwrap();
        assert_eq!(result.lexical_document_match, Some(matched), "{query}");
        assert_eq!(
            result
                .spans
                .iter()
                .map(|span| &result.passage.text[span.start..span.end])
                .collect::<Vec<_>>(),
            expected,
            "{query}"
        );
    }
    let mut natural = request();
    natural.query = "Red + fox?".into();
    natural.lexical_query = "fox OR red".into();
    natural.query_intent = "natural".into();
    natural.mode = "hybrid".into();
    let natural_result = client.explain_passage(&natural).await.unwrap();
    assert_eq!(natural_result.lexical_document_match, Some(true));
    assert!(natural_result.semantic_evidence);
    natural.lexical_query = "private".into();
    assert!(client
        .explain_passage(&natural)
        .await
        .unwrap_err()
        .to_string()
        .contains("lexical plan"));
    let mut vector = request();
    vector.mode = "vector".into();
    vector.lexical_query.clear();
    vector.query = "Why + /?".into();
    let semantic = client.explain_passage(&vector).await.unwrap();
    assert_eq!(semantic.lexical_document_match, None);
    assert!(semantic.clauses.is_none() && semantic.spans.is_empty() && semantic.semantic_evidence);
    let mut other = request();
    other.owner_id = "other".into();
    other.query = "private".into();
    other.lexical_query = "private".into();
    assert_eq!(
        client
            .explain_passage(&other)
            .await
            .unwrap()
            .lexical_document_match,
        Some(true)
    );
    other.owner_id = "missing".into();
    assert!(client.explain_passage(&other).await.is_err());
    let mut changed = chunk("reader", 0, "revised source");
    changed.source_hash = "updated".into();
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None,
            chunks: vec![changed],
        })
        .await
        .unwrap();
    let latest = client.explain_passage(&request()).await.unwrap();
    assert_eq!(latest.lexical_document_match, Some(false));
    assert!(latest.spans.is_empty());
    assert_eq!(latest.passage.source_hash.as_deref(), Some("updated"));
    server.abort();
}

#[tokio::test]
async fn legacy_explanation_capabilities_fail_before_requesting_evidence() {
    for mode in 0..4 {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut router = Router::new().route(
            "/v1/passages/explain",
            post(move || {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    Json(response())
                }
            }),
        );
        if mode != 0 {
            router = router.route(
                "/v1/capabilities",
                get(move || async move {
                    let mut caps = capabilities();
                    match mode {
                        1 => caps.query_explanation_version = 0,
                        2 => caps.query_explanation_version = 999,
                        _ => caps.fts_ready = false,
                    };
                    Json(caps)
                }),
            );
        }
        let (url, server) = serve(router).await;
        assert!(RemoteClient::new(url, "secret")
            .explain_passage(&request())
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }
}

#[tokio::test]
async fn client_rejects_unconfirmed_wrong_scope_and_invalid_evidence_receipts() {
    for mode in 0..17 {
        let router = Router::new()
            .route("/v1/capabilities", get(|| async { Json(capabilities()) }))
            .route(
                "/v1/passages/explain",
                post(move || async move {
                    let mut value = response();
                    match mode {
                        0 => {}
                        1 => value.query = "another".into(),
                        2 => value.lexical_query = "different".into(),
                        3 => value.query_intent = "natural".into(),
                        4 => value.mode = "hybrid".into(),
                        5 => value.fuzzy = true,
                        6 => value.synonyms = true,
                        7 => value.passage.owner_id = "other".into(),
                        8 => value.passage.chunk_index = 2,
                        9 => value.spans[0].end = 4,
                        10 => value.spans[0].end = 99,
                        11 => value.spans[0].kind = "semantic".into(),
                        12 => value.clauses.as_mut().unwrap().matched = false,
                        13 => value.scope = "passage_only".into(),
                        14 => value.explanation_version = 999,
                        15 => value.spans = vec![value.spans[0].clone(); 257],
                        16 => value.semantic_evidence = true,
                        _ => unreachable!(),
                    }
                    let mut result = Json(value).into_response();
                    if mode != 0 {
                        result
                            .headers_mut()
                            .insert(QUERY_EXPLANATION_VERSION_HEADER, "1".parse().unwrap());
                    }
                    result
                }),
            );
        let (url, server) = serve(router).await;
        assert!(
            RemoteClient::new(url, "secret")
                .explain_passage(&request())
                .await
                .is_err(),
            "mode {mode}"
        );
        server.abort();
    }
}

#[test]
fn protocol_rejects_semantic_word_attribution_and_inconsistent_boolean_nodes() {
    let mut request = request();
    let mut value = response();
    value.validate_for(&request).unwrap();
    request.mode = "vector".into();
    request.lexical_query.clear();
    value.mode = "vector".into();
    value.lexical_query.clear();
    value.semantic_evidence = true;
    assert!(value.validate_for(&request).is_err());
    value.lexical_document_match = None;
    value.clauses = None;
    value.spans.clear();
    value.validate_for(&request).unwrap();
    let mut value = response();
    value.clauses = Some(ExplainClause {
        kind: "and".into(),
        label: "AND".into(),
        matched: true,
        field: None,
        matched_fields: vec![],
        children: vec![ExplainClause {
            kind: "term".into(),
            label: "missing".into(),
            matched: false,
            field: None,
            matched_fields: vec![],
            children: vec![],
        }],
    });
    assert!(value
        .validate_for(&super::explanation_contract::request())
        .is_err());
}
