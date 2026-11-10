//! Real authenticated routes, storage snapshots and complete match enumeration.
use axum::{middleware,routing::{delete,get,post},Router};
use crisp_index_protocol::*;
use std::{collections::BTreeSet,sync::Arc,time::Duration};

struct Fixture {
    index:Arc<crate::index::SearchIndex>, http:reqwest::Client, url:String,
    server:tokio::task::JoinHandle<()>, _directory:tempfile::TempDir,
}
impl Drop for Fixture {fn drop(&mut self){self.server.abort();}}
async fn fixture()->Fixture {
    let dir=tempfile::tempdir().unwrap();
    let fts=Arc::new(crate::fts::FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let vector=Arc::new(crate::index::VectorStore::open_or_create(dir.path(),2).await.unwrap());
    let index=Arc::new(crate::index::SearchIndex::new(vector,fts)); index.ensure_ready().await.unwrap();
    let state=Arc::new(crate::state::SharedState {index:index.clone(),queue:crate::queue::TaskQueue::open_or_create(dir.path(),Default::default()).unwrap(),embedder:None,
        config:crate::state::ServerConfig {api_key:"complete-secret".into(),port:0,data_dir:dir.path().into(),embed_dims:2,server_embed_enabled:false,
            queue_lease_secs:30,queue_retry_base_ms:1000,queue_max_attempts:3}});
    let router=Router::new().route("/v1/capabilities",get(crate::routes::capabilities::capabilities))
        .route("/v1/search",post(crate::routes::search::search))
        .route("/v1/search/generation",post(crate::routes::complete::generation))
        .route("/v1/search/complete",post(crate::routes::complete::summary))
        .route("/v1/search/complete/page",post(crate::routes::complete::page))
        .route("/v1/search/complete/destination",post(crate::routes::complete::destination))
        .route("/v1/search/complete/{request_id}",delete(crate::routes::complete::cancel))
        .layer(middleware::from_fn_with_state(state.clone(),crate::auth::require_bearer)).with_state(state);
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let address=listener.local_addr().unwrap();
    let server=tokio::spawn(async move{axum::serve(listener,router).await.unwrap();});
    Fixture {index,http:reqwest::Client::builder().no_proxy().build().unwrap(),url:format!("http://{address}"),server,_directory:dir}
}
fn chunk(owner:&str,id:&str,text:&str)->IngestChunk {
    IngestChunk {doc_id:id.into(),owner_id:owner.into(),chunk_index:0,full_text:text.into(),embedding:vec![1.0,0.0],filename:format!("{id}.txt"),
        ext:"txt".into(),language:"en".into(),location_uri:format!("file:///docs/{owner}/{id}.txt"),source_hash:format!("{id:0>64}"),..Default::default()}
}
fn scope(owner:&str)->CompleteSearchScope {CompleteSearchScope {version:1,query:"needle AND NOT excluded".into(),filters:SearchFilters {owner_id:Some(owner.into()),..Default::default()}}}
fn request(scope:CompleteSearchScope)->CompleteSearchRequest {CompleteSearchRequest {scope,request_id:uuid::Uuid::new_v4().to_string(),expected_generation:None}}
async fn call<T:serde::Serialize>(fixture:&Fixture,path:&str,body:&T)->reqwest::Response {
    fixture.http.post(format!("{}{}",fixture.url,path)).bearer_auth("complete-secret").json(body).send().await.unwrap()
}

#[tokio::test]
async fn complete_http_counts_all_633_documents_with_facets_and_enumerates_every_owner_scoped_page() {
    let fixture=fixture().await;let owner="reader'one";
    let mut chunks:Vec<_>=(0..633).map(|n|{
        let mut row=chunk(owner,&format!("{n:04}"),"needle source"); row.year=(n%7!=0).then_some(2000+n%20);
        row.tags=vec!["shared".into(),format!("topic{:02}",n%31)];
        if n==632 {row.chunk_index=9;} row
    }).collect();
    chunks.push(chunk("other","0000","needle private"));chunks.push(chunk(owner,"excluded","needle excluded"));
    fixture.index.ingest_batch(&IngestBatch {chunks}).await.unwrap();
    let request=request(scope(owner));
    assert_eq!(fixture.http.post(format!("{}/v1/search/complete",fixture.url)).json(&request).send().await.unwrap().status(),401);
    let response=call(&fixture,"/v1/search/complete",&request).await;
    assert_eq!(response.status(),200);assert_eq!(response.headers().get(COMPLETE_SEARCH_VERSION_HEADER).unwrap(),"1");
    let result:CompleteSearchSummary=response.json().await.unwrap();result.validate_for(&request).unwrap();
    assert_eq!(result.matching_documents,633);assert_eq!(result.facets.tags.distinct_values,32);assert_eq!(result.facets.tags.omitted_values,7);
    assert_eq!(result.facets.tags.values[0].value,"shared");assert_eq!(result.facets.tags.values[0].documents,633);
    assert_eq!(result.facets.years.missing_documents,91);
    let mut offset=0;let mut identities=BTreeSet::new();let mut years=Vec::new();
    loop {
        let page_request=CompletePageRequest {version:1,session_id:result.session_id.clone(),generation:result.generation.clone(),offset,limit:73,sort:CompleteSort::YearAsc};
        let response=call(&fixture,"/v1/search/complete/page",&page_request).await;
        assert_eq!(response.status(),200);assert_eq!(response.headers().get(COMPLETE_SEARCH_VERSION_HEADER).unwrap(),"1");
        let bytes=response.bytes().await.unwrap();assert!(bytes.len()<=MAX_COMPLETE_RESPONSE_BYTES);
        let page:CompletePage=serde_json::from_slice(&bytes).unwrap();page.validate_for(&request.scope,&page_request).unwrap();
        assert_eq!(page.matching_documents,633);
        for doc in page.documents {assert_eq!(doc.owner_id,owner);assert!(identities.insert(doc.doc_id));years.push(doc.year);}
        match page.next_offset {Some(next)=>offset=next,None=>break}
    }
    assert_eq!(identities.len(),633);assert!(identities.contains("0632"));
    assert!(years.windows(2).all(|pair|match(pair[0],pair[1]){(Some(a),Some(b))=>a<=b,(None,Some(_))=>false,_=>true}));
    let response=fixture.http.delete(format!("{}/v1/search/complete/{}",fixture.url,request.request_id)).bearer_auth("complete-secret").send().await.unwrap();
    assert_eq!(response.status(),200);
    let page=CompletePageRequest {version:1,session_id:result.session_id,generation:result.generation,offset:0,limit:10,sort:CompleteSort::Filename};
    assert_eq!(call(&fixture,"/v1/search/complete/page",&page).await.status(),410);
}

#[tokio::test]
async fn extended_metadata_filters_apply_before_limits_and_keep_legacy_capabilities_compatible() {
    let fixture=fixture().await;
    let mut good=chunk("owner","target","needle source");good.ext="pdf".into();good.tags=vec!["reviewed".into(),"topic".into()];
    good.parent_dir=Some("/home/%_Docs".into());good.url=Some("https://source.test/a%_/doc".into());good.audio_duration_seconds=Some(0.0);
    good.image_camera_make=Some("Camera%_".into());good.image_camera_model=Some("Model_1%".into());good.text_translated=Some("translatedneedle".into());
    good.text_translated_lang=Some("de".into());good.doc_status=Some("approved".into());good.volume_id=Some("live".into());
    let mut decoy=chunk("owner","decoy","needle source");decoy.parent_dir=Some("/home/xADocs".into());decoy.tags=vec!["reviewed".into()];
    decoy.url=Some("https://source.test/aZZ/doc".into());decoy.image_camera_make=Some("CameraZZ".into());decoy.image_camera_model=Some("ModelA1Z".into());
    decoy.text_translated_lang=Some("de".into());decoy.doc_status=Some("reviewed".into());decoy.volume_id=Some("cold".into());
    let mut foreign=good.clone();foreign.owner_id="foreign".into();
    fixture.index.ingest_batch(&IngestBatch {chunks:vec![good,decoy,foreign]}).await.unwrap();
    let caps:SearchCapabilities=fixture.http.get(format!("{}/v1/capabilities",fixture.url)).bearer_auth("complete-secret").send().await.unwrap().json().await.unwrap();
    assert_eq!(caps.metadata_filter_version,1);assert_eq!(caps.metadata_filter_max_version,2);
    assert_eq!(caps.fts_schema_version,2);assert_eq!(caps.active_fts_schema_version,3);assert_eq!(caps.complete_search_version,1);
    let filters=vec![
        serde_json::json!({"tags":["reviewed","topic"]}),serde_json::json!({"parent_dir_prefix":"/home/%_Docs"}),
        serde_json::json!({"url_domain":"source.test/a%_"}),serde_json::json!({"audio_duration_min_seconds":0,"audio_duration_max_seconds":0}),
        serde_json::json!({"image_camera_make":"Camera%_"}),serde_json::json!({"image_camera_model":"Model_1%"}),
        serde_json::json!({"prefer_translated_lang":"de"}),serde_json::json!({"doc_status":"approved"}),serde_json::json!({"mounted_volume_ids":["live"]}),
    ];
    for mut filters in filters {
        filters["owner_id"]="owner".into();filters["query"]="needle".into();filters["mode"]="text".into();filters["limit"]=1.into();filters["metadata_filter_version"]=2.into();
        let response=call(&fixture,"/v1/search",&filters).await;assert_eq!(response.status(),200,"{filters}");
        assert_eq!(response.headers().get(METADATA_FILTER_VERSION_HEADER).unwrap(),"2");
        let hits:Vec<SearchHit>=response.json().await.unwrap();assert_eq!(hits.len(),1);assert_eq!(hits[0].doc_id,"target");assert_eq!(hits[0].owner_id,"owner");
    }
    let v1=serde_json::json!({"query":"needle","mode":"text","owner_id":"owner","ext":["pdf"],"metadata_filter_version":1});
    let response=call(&fixture,"/v1/search",&v1).await;assert_eq!(response.status(),200);assert_eq!(response.headers().get(METADATA_FILTER_VERSION_HEADER).unwrap(),"1");
    let bad=serde_json::json!({"query":"needle","owner_id":"owner","parent_dir_prefix":"/home","metadata_filter_version":1});
    assert_eq!(call(&fixture,"/v1/search",&bad).await.status(),409);
    let translated=fixture.index.search_text("translated:translatedneedle",&SearchFilters {owner_id:Some("owner".into()),..Default::default()},10).await.unwrap();
    assert_eq!(translated.len(),1);assert_eq!(translated[0].doc_id,"target");
    let mut complete_scope=scope("owner");complete_scope.filters.tags=vec!["reviewed".into(),"topic".into()];complete_scope.filters.prefer_translated_lang=Some("de".into());
    let complete=fixture.index.complete_summary(request(complete_scope)).await.unwrap();assert_eq!(complete.matching_documents,1);
}

#[tokio::test]
async fn generation_receipts_reject_mutation_scope_reuse_and_stale_export_pages() {
    let fixture=fixture().await;fixture.index.ingest_batch(&IngestBatch {chunks:vec![chunk("owner","one","needle")]}).await.unwrap();
    let request=request(scope("owner"));let result=fixture.index.complete_summary(request.clone()).await.unwrap();
    let mut changed_scope=request.scope.clone();changed_scope.filters.owner_id=Some("other".into());
    assert_ne!(fixture.index.complete_generation(&changed_scope).await.unwrap().generation,result.generation);
    let mut stale=request.clone();stale.request_id=uuid::Uuid::new_v4().to_string();stale.expected_generation=Some(result.generation.clone());stale.scope=changed_scope;
    assert_eq!(call(&fixture,"/v1/search/complete",&stale).await.status(),409);
    fixture.index.ingest_batch(&IngestBatch {chunks:vec![chunk("owner","two","needle")]}).await.unwrap();
    let page=CompletePageRequest {version:1,session_id:result.session_id,generation:result.generation,offset:0,limit:200,sort:CompleteSort::Filename};
    assert_eq!(call(&fixture,"/v1/search/complete/page",&page).await.status(),409);
}

#[tokio::test]
async fn cancellation_interrupts_waiting_scans_and_session_bound_destination_checks() {
    let fixture=fixture().await;fixture.index.ingest_batch(&IngestBatch {chunks:vec![chunk("owner","one","needle")]}).await.unwrap();
    let initial=request(scope("owner"));let result=fixture.index.complete_summary(initial.clone()).await.unwrap();
    let lease=fixture.index.read_guard().await.unwrap();
    let destination=CompleteDestinationRequest {version:1,request_id:initial.request_id.clone(),generation:result.generation,scope:initial.scope,destination_paths:vec!["/reports/new.csv".into()]};
    let http=fixture.http.clone();let url=format!("{}/v1/search/complete/destination",fixture.url);
    let pending=tokio::spawn(async move{http.post(url).bearer_auth("complete-secret").json(&destination).send().await.unwrap()});
    tokio::task::yield_now().await;
    fixture.index.cancel_complete(&initial.request_id).unwrap();
    let response=tokio::time::timeout(Duration::from_secs(2),pending).await.unwrap().unwrap();assert!(matches!(response.status().as_u16(),409|410));
    let scan=request(scope("owner"));let http=fixture.http.clone();let url=format!("{}/v1/search/complete",fixture.url);let body=scan.clone();
    let pending=tokio::spawn(async move{http.post(url).bearer_auth("complete-secret").json(&body).send().await.unwrap()});
    tokio::task::yield_now().await;fixture.index.cancel_complete(&scan.request_id).unwrap();
    assert_eq!(tokio::time::timeout(Duration::from_secs(2),pending).await.unwrap().unwrap().status(),409);
    drop(lease);assert_eq!(fixture.index.stats().await.unwrap().1,1);
}

#[tokio::test]
async fn destination_guard_checks_nonmatching_other_owner_sources_and_windows_paths_on_linux() {
    let fixture=fixture().await;
    let mut protected=chunk("other","protected","unrelated");protected.location_uri="crisp+local://machine/C:/Docs/Source%20File.pdf".into();
    fixture.index.ingest_batch(&IngestBatch {chunks:vec![chunk("owner","one","needle"),protected]}).await.unwrap();
    let request=request(scope("owner"));let summary=fixture.index.complete_summary(request.clone()).await.unwrap();
    let body=CompleteDestinationRequest {version:1,request_id:request.request_id,generation:summary.generation,scope:request.scope,destination_paths:vec![r"c:\docs\source file.pdf".into()]};
    let response=call(&fixture,"/v1/search/complete/destination",&body).await;assert_eq!(response.status(),200);
    let receipt:CompleteDestinationResponse=response.json().await.unwrap();assert!(receipt.protected);
}
