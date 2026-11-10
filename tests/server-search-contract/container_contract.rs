//! Actual authenticated group publication, member inspection and corpus reports.
use axum::{middleware,routing::{delete,get,post},Router};
use crisp_index_protocol::*;
use std::{sync::Arc,time::Duration};

struct Fixture {
    index:Arc<crate::index::SearchIndex>,client:reqwest::Client,url:String,
    server:tokio::task::JoinHandle<()>,_directory:tempfile::TempDir,
}
impl Drop for Fixture {fn drop(&mut self){self.server.abort();}}
async fn fixture()->Fixture {
    let directory=tempfile::tempdir().unwrap();
    let fts=Arc::new(crate::fts::FtsStore::open_or_create(&directory.path().join("fts-v2")).unwrap());
    let vector=Arc::new(crate::index::VectorStore::open_or_create(directory.path(),2).await.unwrap());
    let index=Arc::new(crate::index::SearchIndex::new(vector,fts));index.ensure_ready().await.unwrap();
    let queue=crate::queue::TaskQueue::open_or_create(directory.path(),Default::default()).unwrap();
    queue.spawn_worker(index.clone(),None);
    let state=Arc::new(crate::state::SharedState {index:index.clone(),queue,embedder:None,
        config:crate::state::ServerConfig {api_key:"member-secret".into(),port:0,data_dir:directory.path().into(),embed_dims:2,
            server_embed_enabled:false,queue_lease_secs:30,queue_retry_base_ms:100,queue_max_attempts:3}});
    let router=Router::new().route("/v1/capabilities",get(crate::routes::capabilities::capabilities))
        .route("/v1/ingest",post(crate::routes::ingest::ingest))
        .route("/v1/ingest/batch",post(crate::routes::ingest::ingest_batch))
        .route("/v1/ingest/containers",post(crate::routes::ingest::ingest_containers).layer(axum::extract::DefaultBodyLimit::max(64*1024*1024)))
        .route("/v1/tasks/{task_id}",get(crate::routes::tasks::task_status))
        .route("/v1/members/read",post(crate::routes::members::read_member))
        .route("/v1/passages/read",post(crate::routes::passages::read_passage))
        .route("/v1/search",post(crate::routes::search::search))
        .route("/v1/docs/{doc_id}",delete(crate::routes::docs::delete_doc))
        .route("/v1/docs/{doc_id}/location",post(crate::routes::docs::update_location))
        .route("/v1/search/complete",post(crate::routes::complete::summary))
        .route("/v1/search/complete/page",post(crate::routes::complete::page))
        .layer(middleware::from_fn_with_state(state.clone(),crate::auth::require_bearer)).with_state(state);
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let server=tokio::spawn(async move{axum::serve(listener,router).await.unwrap();});
    Fixture {index,client:reqwest::Client::builder().no_proxy().build().unwrap(),url:format!("http://{address}"),server,_directory:directory}
}

pub(crate) fn group(owner:&str,uri:&str,entries:&[(&str,&str,u32)])->IngestBatch {
    let fingerprint="a".repeat(64);let root_id=crate::source_identity::occurrence_id(owner,uri,&fingerprint);
    let mut chunks=vec![IngestChunk {doc_id:root_id.clone(),source_identity_version:Some(1),chunk_index:0,chunk_total:Some(1),
        chunk_start_char:Some(0),chunk_end_char:Some(0),filename:"root.zip".into(),title:Some("Envelope".into()),ext:"zip".into(),
        location_uri:uri.into(),owner_id:owner.into(),source_hash:fingerprint.clone(),
        container_source:Some(ContainerSourceReceipt {kind:ContainerKind::Zip,sha256:fingerprint.clone(),size:100}),
        container_coverage:Some(ContainerCoverage {discovered:entries.len() as u32,extracted:entries.len() as u32,..Default::default()}),..Default::default()}];
    for (index,(name,text,occurrence)) in entries.iter().enumerate() {
        let locator=ContainerMemberLocator {version:1,root_owner_id:owner.into(),root_doc_id:root_id.clone(),root_location_uri:uri.into(),root_sha256:fingerprint.clone(),
            entries:vec![ContainerEntry {kind:ContainerEntryKind::Zip,index:index as u32,occurrence:*occurrence,name:(*name).into(),sha256:format!("{index:064x}"),size:20}]};
        chunks.push(IngestChunk {doc_id:locator.member_id().unwrap(),chunk_index:0,chunk_total:Some(1),chunk_start_char:Some(0),chunk_end_char:Some(text.len() as i32),
            full_text:(*text).into(),embedding:vec![1.0,0.0],filename:(*name).into(),ext:"txt".into(),language:"en".into(),owner_id:owner.into(),
            location_uri:locator.member_uri().unwrap(),source_hash:locator.entries[0].sha256.clone(),container_member:Some(locator),..Default::default()});
    }
    IngestBatch {container_group_version:Some(1),chunks}
}
async fn call<T:serde::Serialize>(fixture:&Fixture,path:&str,body:&T)->reqwest::Response {
    fixture.client.post(format!("{}{}",fixture.url,path)).bearer_auth("member-secret").json(body).send().await.unwrap()
}
async fn publish_and_wait(fixture:&Fixture,batch:&IngestBatch) {
    let response=call(fixture,"/v1/ingest/containers",batch).await;
    assert_eq!(response.status(),202);
    assert_eq!(response.headers().get(CONTAINER_MEMBERS_VERSION_HEADER).unwrap(),"1");
    let accepted:BatchIngestResponse=response.json().await.unwrap();
    let deadline=tokio::time::Instant::now()+Duration::from_secs(15);
    loop {
        let status:TaskStatusResponse=fixture.client.get(format!("{}/v1/tasks/{}",fixture.url,accepted.task_id))
            .bearer_auth("member-secret").send().await.unwrap().json().await.unwrap();
        match status.state.as_str(){"done"=>{assert_eq!(status.completed_chunks,batch.chunks.len());break;},
            "failed"=>panic!("group publication failed: {:?}",status.error),_=>{assert!(tokio::time::Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(20)).await;}}
    }
}
async fn hits(fixture:&Fixture,query:&str,owner:&str)->Vec<SearchHit> {
    let response=call(fixture,"/v1/search",&SearchRequest {query:Some(query.into()),mode:Some("text".into()),query_language_version:Some(1),owner_id:Some(owner.into()),..Default::default()}).await;
    assert_eq!(response.status(),200);response.json().await.unwrap()
}

#[tokio::test]
async fn authenticated_group_http_retains_independent_documents_receipts_and_complete_report_provenance() {
    let fixture=fixture().await;let batch=group("owner","/records/root.zip",&[("duplicate.txt","needle red",0),("duplicate.txt","needle fox",1)]);
    let unauthorized=fixture.client.post(format!("{}/v1/ingest/containers",fixture.url)).json(&batch).send().await.unwrap();assert_eq!(unauthorized.status(),401);
    publish_and_wait(&fixture,&batch).await;
    assert!(hits(&fixture,"red AND fox","owner").await.is_empty());
    let red=hits(&fixture,"red","owner").await;assert_eq!(red.len(),1);assert_eq!(red[0].container_member.as_ref().unwrap().entries[0].occurrence,0);
    let request=ContainerMemberRequest {version:1,owner_id:"owner".into(),doc_id:red[0].doc_id.clone()};
    let response=call(&fixture,"/v1/members/read",&request).await;assert_eq!(response.status(),200);
    assert_eq!(response.headers().get(CONTAINER_MEMBERS_VERSION_HEADER).unwrap(),"1");
    let member:ContainerMemberResponse=response.json().await.unwrap();member.validate_for(&request).unwrap();assert_eq!(member.locator,red[0].container_member);
    let wrong=ContainerMemberRequest {owner_id:"other-owner".into(),..request.clone()};assert_eq!(call(&fixture,"/v1/members/read",&wrong).await.status(),422);
    let passage_request=StoredPassageRequest {passage_version:1,doc_id:request.doc_id.clone(),owner_id:request.owner_id.clone(),chunk_index:0};
    let response=call(&fixture,"/v1/passages/read",&passage_request).await;assert_eq!(response.status(),200);
    let passage:StoredPassage=response.json().await.unwrap();passage.validate_for(&passage_request).unwrap();assert_eq!(passage.container_member,member.locator);
    let scope=CompleteSearchScope {version:1,query:"needle".into(),filters:SearchFilters {owner_id:Some("owner".into()),..Default::default()}};
    let request=CompleteSearchRequest {scope:scope.clone(),request_id:uuid::Uuid::new_v4().to_string(),expected_generation:None};
    let response=call(&fixture,"/v1/search/complete",&request).await;assert_eq!(response.status(),200);
    let summary:CompleteSearchSummary=response.json().await.unwrap();assert_eq!(summary.matching_documents,2);
    let page_request=CompletePageRequest {version:1,session_id:summary.session_id,generation:summary.generation,offset:0,limit:200,sort:CompleteSort::Filename};
    let page:CompletePage=call(&fixture,"/v1/search/complete/page",&page_request).await.json().await.unwrap();page.validate_for(&scope,&page_request).unwrap();
    assert_eq!(page.documents.len(),2);assert!(page.documents.iter().all(|document|document.container_member.is_some()));
    assert_eq!(fixture.index.stats().await.unwrap().0,3);
}

#[tokio::test]
async fn remote_root_refresh_move_and_owner_scoped_delete_preserve_member_identity() {
    let fixture=fixture().await;let original=group("a","/old/root.zip",&[("keep.txt","oldword",0),("gone.txt","staleword",0)]);
    let foreign=group("b","/old/root.zip",&[("keep.txt","foreignword",0)]);
    publish_and_wait(&fixture,&original).await;publish_and_wait(&fixture,&foreign).await;
    let original_id=hits(&fixture,"oldword","a").await[0].doc_id.clone();let root=original.chunks[0].doc_id.clone();
    let mut replacement=group("a","/old/root.zip",&[("keep.txt","newword",0)]);
    replacement.chunks[0].source_hash="b".repeat(64);replacement.chunks[0].container_source.as_mut().unwrap().sha256="b".repeat(64);
    for chunk in replacement.chunks.iter_mut().skip(1){let locator=chunk.container_member.as_mut().unwrap();locator.root_sha256="b".repeat(64);locator.entries[0].index=9;locator.entries[0].sha256="c".repeat(64);chunk.source_hash="c".repeat(64);}
    publish_and_wait(&fixture,&replacement).await;
    assert_eq!(hits(&fixture,"newword","a").await[0].doc_id,original_id);assert!(hits(&fixture,"oldword OR staleword","a").await.is_empty());
    assert_eq!(call(&fixture,&format!("/v1/docs/{root}/location?owner_id=a"),&UpdateLocationBody {new_uri:"/moved/root.zip".into()}).await.status(),200);
    let inspected=fixture.index.read_container_member(&ContainerMemberRequest {version:1,owner_id:"a".into(),doc_id:original_id.clone()}).await.unwrap();
    assert_eq!(inspected.locator.unwrap().root_location_uri,"/moved/root.zip");
    let response=fixture.client.delete(format!("{}/v1/docs/{root}?owner_id=a",fixture.url)).bearer_auth("member-secret").send().await.unwrap();assert_eq!(response.status(),200);
    assert!(hits(&fixture,"newword","a").await.is_empty());assert_eq!(hits(&fixture,"foreignword","b").await.len(),1);
}

#[tokio::test]
async fn incomplete_unversioned_and_cross_owner_groups_fail_before_any_publication() {
    let fixture=fixture().await;let original=group("owner","/root.zip",&[("keep.txt","stableword",0)]);
    publish_and_wait(&fixture,&original).await;
    let mut cases=Vec::new();
    let mut partial=original.clone();partial.chunks.remove(0);cases.push(partial);
    let mut partial=original.clone();partial.chunks[1].chunk_total=Some(2);cases.push(partial);
    let mut foreign=original.clone();foreign.chunks[1].owner_id="other".into();cases.push(foreign);
    let mut invalid=original.clone();invalid.chunks[1].container_member.as_mut().unwrap().entries[0].name="../unsafe.txt".into();cases.push(invalid);
    for invalid in cases {assert_eq!(call(&fixture,"/v1/ingest/containers",&invalid).await.status(),422);}
    assert_eq!(call(&fixture,"/v1/ingest/batch",&original).await.status(),409);
    assert_eq!(call(&fixture,"/v1/ingest",&original.chunks[1]).await.status(),409);
    assert_eq!(hits(&fixture,"stableword","owner").await.len(),1);
}
