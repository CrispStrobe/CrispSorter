//! Production desktop transport and report publication against controlled peers.
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
use axum::{Json, Router, extract::Path, http::{HeaderMap, StatusCode}, response::IntoResponse, routing::{delete, get, post}};
use crisp_index_protocol::*;
use crate::desktop::{IndexBackend, remote_client::{RemoteClient, complete_search_scope, wire_filters}, remote_summary,
    schema::SearchFilters, search_report::{ReportFormat, ReportSort, ScanCancellation}};

const GENERATION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
fn capabilities() -> SearchCapabilities {
    SearchCapabilities { api_version:1, metadata_filter_version:1, metadata_filter_max_version:METADATA_FILTER_VERSION, stored_passage_version:STORED_PASSAGE_VERSION,
        query_language_version:QUERY_LANGUAGE_VERSION, fts_schema_version:2, active_fts_schema_version:FTS_SCHEMA_VERSION, fts_ready:true,
        strict_hybrid_constraints:true, searchable_fields:vec!["body".into(),"translated".into()], chunk_boundary_policy:"verified offsets".into(),
        complete_search_version:COMPLETE_SEARCH_VERSION, metadata_schema_version:METADATA_SCHEMA_VERSION, embedding_query_version:1,
        metadata_fields:["owner_id","language","year_min","year_max","ext","tag","source_hash_prefix","doc_id_scope","tags",
            "parent_dir_prefix","url_domain","audio_duration_min_seconds","audio_duration_max_seconds","image_camera_make",
            "image_camera_model","prefer_translated_lang","doc_status","mounted_volume_ids"].into_iter().map(str::to_owned).collect() }
}
fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new(); headers.insert(COMPLETE_SEARCH_VERSION_HEADER,"1".parse().unwrap()); headers
}
fn facet(total: u64) -> CompleteFacet {
    CompleteFacet { values:if total == 0 { vec![] } else { vec![CompleteFacetValue {value:"reviewed".into(),documents:total}] },
        distinct_values:u64::from(total > 0),omitted_values:0,missing_documents:0 }
}
fn summary(request: &CompleteSearchRequest, total: u64) -> CompleteSearchSummary {
    CompleteSearchSummary { version:1,scope:COMPLETE_SEARCH_SCOPE.into(),query:request.scope.query.clone(),filters:request.scope.filters.clone(),
        generation:GENERATION.into(),request_id:request.request_id.clone(),session_id:"session-1".into(),
        expires_at_ms:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64+60_000,
        matching_documents:total,scanned_rows:total+12,facet_value_limit:25,
        facets:CompleteFacets {extensions:facet(total),languages:facet(total),years:facet(total),tags:facet(total)} }
}
fn document(index: usize) -> CompleteDocument {
    CompleteDocument {doc_id:format!("doc-{index:04}"),owner_id:"reader".into(),representative_chunk_index:2,
        title:Some(format!("Title {index:04}")),filename:Some(format!("file-{index:04}.pdf")),author:Some("Author".into()),
        location_uri:format!("test://source/{index}"),url:Some("https://example.test/source".into()),source_hash:"source-hash".into(),
        indexed_at:1000,extension:Some("pdf".into()),language:Some("en".into()),year:Some(2026),tags:vec!["reviewed".into()]}
}
async fn serve(router: Router) -> (String,tokio::task::JoinHandle<()>) {
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let address=listener.local_addr().unwrap();
    let server=tokio::spawn(async move {axum::serve(listener,router).await.unwrap();}); (format!("http://{address}"),server)
}
#[derive(Default)]
struct Peer {
    calls: AtomicUsize,
    pages: AtomicUsize,
    closed: AtomicUsize,
    destination_calls: AtomicUsize,
    stale: AtomicBool,
    protected: AtomicBool,
    hold_scan: AtomicBool,
    malformed_page: AtomicUsize,
    scope: std::sync::Mutex<Option<CompleteSearchScope>>,
}
fn peer_router(peer: Arc<Peer>, total: usize) -> Router {
    let scan=peer.clone(); let page=peer.clone(); let generation=peer.clone(); let destination=peer.clone(); let close=peer.clone();
    Router::new().route("/v1/capabilities",get(||async{Json(capabilities())}))
        .route("/v1/search/generation",post(move |Json(scope):Json<CompleteSearchScope>| {
            let peer=generation.clone();async move {(headers(),Json(CompleteGeneration {version:1,scope:COMPLETE_SEARCH_SCOPE.into(),
                query:scope.query,filters:scope.filters,generation:if peer.stale.load(Ordering::SeqCst){"b".repeat(64)}else{GENERATION.into()}}))}
        }))
        .route("/v1/search/complete",post(move |Json(request):Json<CompleteSearchRequest>| {
            let peer=scan.clone();async move {
                peer.calls.fetch_add(1,Ordering::SeqCst); *peer.scope.lock().unwrap()=Some(request.scope.clone());
                if peer.hold_scan.load(Ordering::SeqCst) { std::future::pending::<()>().await; }
                (headers(),Json(summary(&request,total as u64)))
            }
        }))
        .route("/v1/search/complete/page",post(move |Json(request):Json<CompletePageRequest>| {
            let peer=page.clone();async move {
                peer.pages.fetch_add(1,Ordering::SeqCst);
                if peer.stale.load(Ordering::SeqCst) { return (StatusCode::CONFLICT,"The index generation changed").into_response(); }
                let scope=peer.scope.lock().unwrap().clone().unwrap();
                let end=(request.offset as usize+request.limit).min(total);
                let mut documents=(request.offset as usize..end).map(document).collect::<Vec<_>>();
                let mode=peer.malformed_page.load(Ordering::SeqCst);
                if mode==1 && request.offset>0 && !documents.is_empty(){documents[0]=document(0);}
                if mode==2 && !documents.is_empty(){documents[0].owner_id="other-owner".into();}
                if mode==3 {documents.reverse();}
                let next=(end<total).then_some(end as u64);
                (headers(),Json(CompletePage {version:1,scope:COMPLETE_SEARCH_SCOPE.into(),query:scope.query,filters:scope.filters,
                    generation:GENERATION.into(),offset:request.offset,documents,next_offset:if mode==4 {Some(request.offset)}else{next},
                    matching_documents:total as u64+u64::from(mode==5)})).into_response()
            }
        }))
        .route("/v1/search/complete/destination",post(move |Json(request):Json<CompleteDestinationRequest>| {
            let peer=destination.clone();async move {
                peer.destination_calls.fetch_add(1,Ordering::SeqCst);
                assert!(!request.destination_paths.is_empty());
                if peer.stale.load(Ordering::SeqCst) {return (StatusCode::CONFLICT,"The index generation changed").into_response();}
                (headers(),Json(CompleteDestinationResponse {version:1,generation:request.generation,protected:peer.protected.load(Ordering::SeqCst)})).into_response()
            }
        }))
        .route("/v1/search/complete/{request_id}",delete(move |Path(request_id):Path<String>| {
            let peer=close.clone();async move {peer.closed.fetch_add(1,Ordering::SeqCst);
                (headers(),Json(CompleteCancelResponse {version:1,request_id,cancelled:true}))}
        }))
}
fn filters() -> SearchFilters { SearchFilters {owner_id:Some("reader".into()),..Default::default()} }
fn options(client:&RemoteClient,path:std::path::PathBuf,format:ReportFormat) -> remote_summary::RemoteExportOptions {
    remote_summary::RemoteExportOptions {request_id:"export-request-1".into(),path,format,sort:ReportSort::Filename,
        expected_generation:client.bind_generation(GENERATION),protected_roots:vec![]}
}

#[test]
fn wire_predicates_and_expansion_preserve_literal_metadata_and_original_flags() {
    let filters=SearchFilters {owner_id:Some("owner'a".into()),tags:vec!["required".into()],tag:Some("reviewed".into()),
        ext:vec!["pdf".into()],language:Some("en".into()),year_min:Some(2020),year_max:Some(2026),doc_id_scope:vec!["same".into()],
        parent_dir_prefix:Some("/folder_100%/".into()),url_domain:Some("example.test/a_b%".into()),source_hash_prefix:Some("abc".into()),
        audio_duration_min_seconds:Some(1.5),audio_duration_max_seconds:Some(30.0),image_camera_make:Some("Maker".into()),
        image_camera_model:Some("Model".into()),prefer_translated_lang:Some("de".into()),doc_status:Some("approved".into()),
        mounted_volume_ids:Some(vec!["recorded-volume".into()]),synonyms:true,fuzzy:true,..Default::default()};
    let wire=wire_filters(&filters).unwrap();
    assert_eq!(wire.parent_dir_prefix,filters.parent_dir_prefix); assert_eq!(wire.tags,filters.tags);
    assert_eq!(wire.mounted_volume_ids,filters.mounted_volume_ids); assert_eq!(wire.audio_duration_min_seconds,Some(1.5));
    let scope=complete_search_scope("begin AND title:\"Exact phrase\"",&filters).unwrap();
    assert!(scope.query.contains("commence~1")); assert!(scope.query.contains("title:\"Exact phrase\""));
    assert_eq!(scope.filters,wire); assert!(filters.fuzzy && filters.synonyms);
    let mut unsupported=filters.clone();unsupported.omni_search=true;assert!(wire_filters(&unsupported).is_err());
}

#[tokio::test]
async fn expanded_hybrid_preserves_semantic_input_and_requires_its_receipt() {
    for receipt in [false,true] {
        let router=Router::new().route("/v1/capabilities",get(||async{Json(capabilities())}))
            .route("/v1/search",post(move |Json(request):Json<SearchRequest>|async move {
                assert!(request.query.as_ref().unwrap().contains("commence~1"));
                assert_eq!(request.embedding_query.as_deref(),Some("begin"));assert_eq!(request.embedding_query_version,Some(1));
                let mut header=HeaderMap::new();header.insert(QUERY_VERSION_HEADER,"1".parse().unwrap());
                if receipt {header.insert(EMBEDDING_QUERY_VERSION_HEADER,"1".parse().unwrap());}
                (header,Json(Vec::<SearchHit>::new()))
            }));
        let (url,server)=serve(router).await;let client=RemoteClient::new(url,"");
        let result=client.search_hybrid_server("begin",&SearchFilters{synonyms:true,fuzzy:true,..Default::default()},20).await;
        assert_eq!(result.is_ok(),receipt);if !receipt {assert!(result.unwrap_err().to_string().contains("semantic query"));}
        server.abort();
    }
}

#[tokio::test]
async fn per_chunk_ingest_preserves_predicate_metadata_and_translated_content() {
    let router=Router::new().route("/v1/ingest",post(|Json(chunk):Json<IngestChunk>|async move {
        assert_eq!(chunk.parent_dir.as_deref(),Some("/recorded/folder"));assert_eq!(chunk.volume_id.as_deref(),Some("recorded-volume"));
        assert_eq!(chunk.url.as_deref(),Some("https://example.test/source"));assert_eq!(chunk.text_translated.as_deref(),Some("translated body"));
        assert_eq!(chunk.text_translated_lang.as_deref(),Some("de"));assert_eq!(chunk.doc_status.as_deref(),Some("approved"));
        assert_eq!(chunk.audio_duration_seconds,Some(12.5));assert_eq!(chunk.image_camera_make.as_deref(),Some("Maker"));
        assert_eq!(chunk.image_camera_model.as_deref(),Some("Model"));Json(serde_json::json!({"chunk_count":1,"write_time_ms":1}))
    }));
    let (url,server)=serve(router).await;let client=RemoteClient::new(url,"");
    let chunk:crate::desktop::DocumentChunk=serde_json::from_value(serde_json::json!({
        "id":"row","doc_id":"doc","owner_id":"reader","location_uri":"test://source","chunk_index":2,"chunk_total":3,
        "indexed_at":1,"source_hash":"hash","tags":["reviewed"],"full_text":"body","embedding":[1.0,0.0],
        "parent_dir":"/recorded/folder","volume_id":"recorded-volume","url":"https://example.test/source",
        "text_translated":"translated body","text_translated_lang":"de","doc_status":"approved","audio_duration_seconds":12.5,
        "image_camera_make":"Maker","image_camera_model":"Model"
    })).unwrap();
    client.ingest(chunk).await.unwrap();server.abort();
}

#[tokio::test]
async fn legacy_or_unready_peers_never_receive_complete_scope_requests() {
    for mode in 0..5 {
        let calls=Arc::new(AtomicUsize::new(0));let seen=calls.clone();
        let mut router=Router::new().route("/v1/search/generation",post(move ||{let calls=seen.clone();async move{
            calls.fetch_add(1,Ordering::SeqCst);Json(serde_json::json!({}))}}));
        if mode>0 {router=router.route("/v1/capabilities",get(move ||async move{let mut caps=capabilities();
            match mode {1=>caps.complete_search_version=0,2=>caps.fts_ready=false,3=>caps.active_fts_schema_version=2,_=>caps.metadata_schema_version=0};Json(caps)}));}
        let (url,server)=serve(router).await;let client=RemoteClient::new(url,"");
        assert!(client.complete_generation(&complete_search_scope("needle",&filters()).unwrap()).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst),0);server.abort();
    }
}

#[tokio::test]
async fn invalid_scope_generation_facets_and_version_receipts_are_never_accepted() {
    for mode in 0..5 {
        let router=Router::new().route("/v1/capabilities",get(||async{Json(capabilities())}))
            .route("/v1/search/complete",post(move |Json(request):Json<CompleteSearchRequest>|async move{
                let mut value=summary(&request,633);let mut header=headers();
                match mode {0=>{header.clear();},1=>value.filters.owner_id=Some("other-owner".into()),2=>value.generation="wrong".into(),
                    3=>value.request_id="other-request".into(),_=>value.facets.tags.values[0].documents=634};(header,Json(value))}));
        let (url,server)=serve(router).await;let client=RemoteClient::new(url,"");
        let request=CompleteSearchRequest{scope:complete_search_scope("needle",&filters()).unwrap(),request_id:"count-1".into(),expected_generation:None};
        assert!(client.complete_search(&request).await.is_err());server.abort();
    }
}

#[tokio::test]
async fn complete_remote_report_streams_all_pages_and_atomically_publishes_exact_scope() {
    let peer=Arc::new(Peer::default());let (url,server)=serve(peer_router(peer.clone(),633)).await;
    let client=Arc::new(RemoteClient::new(url,"secret"));let directory=tempfile::tempdir().unwrap();let path=directory.path().join("all.json");
    std::fs::write(&path,"previous report").unwrap();let cancellation=Arc::new(ScanCancellation::default());
    let prepared=remote_summary::prepare_export(client.clone(),"needle".into(),filters(),options(&client,path.clone(),ReportFormat::Json),cancellation.clone()).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(),"previous report");assert_eq!(peer.pages.load(Ordering::SeqCst),4);
    let receipt=prepared.publish(||Ok(())).await.unwrap();assert_eq!(receipt.documents,633);assert_eq!(receipt.scope,COMPLETE_SEARCH_SCOPE);
    assert_eq!(receipt.generation,client.bind_generation(GENERATION));assert!(!cancellation.cancel());
    let report:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report["documents"].as_array().unwrap().len(),633);assert_eq!(report["documents"][632]["docId"],"doc-0632");
    assert_eq!(report["scope"],COMPLETE_SEARCH_SCOPE);assert_eq!(report["filters"]["owner_id"],"reader");
    assert_eq!(peer.destination_calls.load(Ordering::SeqCst),2);assert_eq!(peer.closed.load(Ordering::SeqCst),1);
    server.abort();
}

#[tokio::test]
async fn invalid_page_sequences_never_replace_complete_previous_reports() {
    for mode in 1..=5 {
        let peer=Arc::new(Peer::default());peer.malformed_page.store(mode,Ordering::SeqCst);
        let (url,server)=serve(peer_router(peer.clone(),233)).await;let client=Arc::new(RemoteClient::new(url,""));
        let directory=tempfile::tempdir().unwrap();let path=directory.path().join("previous.json");std::fs::write(&path,"previous").unwrap();
        let result=remote_summary::prepare_export(client.clone(),"needle".into(),filters(),options(&client,path.clone(),ReportFormat::Json),Arc::new(ScanCancellation::default())).await;
        assert!(result.is_err(),"malformed mode {mode} accepted");assert_eq!(std::fs::read_to_string(&path).unwrap(),"previous");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(),1);server.abort();
    }
}

#[tokio::test]
async fn stale_generation_source_collisions_and_cancelled_publication_preserve_previous_files() {
    for mode in 0..3 {
        let peer=Arc::new(Peer::default());let (url,server)=serve(peer_router(peer.clone(),3)).await;
        let client=Arc::new(RemoteClient::new(url,""));let directory=tempfile::tempdir().unwrap();let path=directory.path().join("all.csv");
        std::fs::write(&path,"previous").unwrap();let cancellation=Arc::new(ScanCancellation::default());
        let prepared=remote_summary::prepare_export(client.clone(),"needle".into(),filters(),options(&client,path.clone(),ReportFormat::Csv),cancellation.clone()).await.unwrap();
        match mode {0=>peer.stale.store(true,Ordering::SeqCst),1=>peer.protected.store(true,Ordering::SeqCst),_=>{assert!(cancellation.cancel());}}
        assert!(prepared.publish(||Ok(())).await.is_err());assert_eq!(std::fs::read_to_string(&path).unwrap(),"previous");server.abort();
    }
}

#[tokio::test]
async fn cancellation_aborts_inflight_remote_scan_and_closes_its_request_identity() {
    let peer=Arc::new(Peer::default());peer.hold_scan.store(true,Ordering::SeqCst);
    let (url,server)=serve(peer_router(peer.clone(),633)).await;let client=Arc::new(RemoteClient::new(url,""));
    let cancellation=Arc::new(ScanCancellation::default());let token=cancellation.clone();
    let scan=tokio::spawn(async move{remote_summary::summarize(client,complete_search_scope("needle",&filters()).unwrap(),"cancel-1".into(),token).await});
    tokio::time::timeout(std::time::Duration::from_secs(5),async{while peer.calls.load(Ordering::SeqCst)==0 {tokio::task::yield_now().await;}}).await.unwrap();
    assert!(cancellation.cancel());let error=tokio::time::timeout(std::time::Duration::from_secs(2),scan).await.unwrap().unwrap().unwrap_err();
    assert!(error.to_string().contains("cancelled"));assert!(peer.closed.load(Ordering::SeqCst)>0);server.abort();
}

#[tokio::test]
async fn zero_match_csv_keeps_remote_query_scope_and_connection_switch_refuses_export() {
    let peer=Arc::new(Peer::default());let (url,server)=serve(peer_router(peer.clone(),0)).await;
    let client=Arc::new(RemoteClient::new(&url,"original"));let directory=tempfile::tempdir().unwrap();let path=directory.path().join("zero.csv");
    let prepared=remote_summary::prepare_export(client.clone(),"absent".into(),filters(),options(&client,path.clone(),ReportFormat::Csv),Arc::new(ScanCancellation::default())).await.unwrap();
    assert_eq!(prepared.publish(||Ok(())).await.unwrap().documents,0);
    let csv=std::fs::read_to_string(&path).unwrap();assert_eq!(csv.lines().count(),2);assert!(csv.contains("\"report\""));
    assert!(csv.contains(COMPLETE_SEARCH_SCOPE));assert!(csv.contains("absent"));
    let changed=Arc::new(RemoteClient::new(&url,"different"));let before=peer.calls.load(Ordering::SeqCst);
    let result=remote_summary::prepare_export(changed,"absent".into(),filters(),options(&client,path.clone(),ReportFormat::Csv),Arc::new(ScanCancellation::default())).await;
    assert!(result.is_err());assert_eq!(peer.calls.load(Ordering::SeqCst),before);assert_eq!(std::fs::read_to_string(&path).unwrap(),csv);server.abort();
}
