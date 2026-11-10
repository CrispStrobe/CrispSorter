//! The production desktop transport never substitutes or silently downgrades members.
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use axum::{Json, Router, http::HeaderMap, routing::{get, post}};
use crisp_index_protocol::*;
use crate::desktop::{IndexBackend, remote_client::RemoteClient};

fn capabilities(version: u32) -> serde_json::Value {
    serde_json::json!({ "api_version":1,"metadata_filter_version":1,"metadata_filter_max_version":2,
        "query_language_version":QUERY_LANGUAGE_VERSION,"fts_schema_version":2,"active_fts_schema_version":FTS_SCHEMA_VERSION,
        "fts_ready":true,"strict_hybrid_constraints":true,"searchable_fields":["body"],
        "chunk_boundary_policy":"verified offsets","container_members_version":version })
}
fn locator(owner: &str) -> ContainerMemberLocator {
    ContainerMemberLocator { version:1,root_owner_id:owner.into(),root_doc_id:"root-occurrence".into(),
        root_location_uri:"/documents/mail.zip".into(),root_sha256:"a".repeat(64),entries:vec![
            ContainerEntry { kind:ContainerEntryKind::Zip,index:4,occurrence:0,name:"receipt.txt".into(),sha256:"b".repeat(64),size:30 }] }
}
fn receipt_header(name: &'static str) -> HeaderMap {
    let mut headers=HeaderMap::new();headers.insert(name,"1".parse().unwrap());headers
}
async fn serve(router:Router)->(String,tokio::task::JoinHandle<()>) {
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let task=tokio::spawn(async move {axum::serve(listener,router).await.unwrap();});(format!("http://{address}"),task)
}

#[tokio::test]
async fn member_lookup_negotiates_and_verifies_owner_identity_and_receipt() {
    let mode=Arc::new(AtomicUsize::new(0));let current=mode.clone();
    let router=Router::new().route("/v1/capabilities",get(||async{Json(capabilities(1))}))
        .route("/v1/members/read",post(move |headers:HeaderMap,Json(request):Json<ContainerMemberRequest>|{
            let mode=current.clone();async move {
                assert_eq!(headers["authorization"],"Bearer test-token");
                let selected=mode.load(Ordering::SeqCst);
                let mut receipt=ContainerMemberResponse {version:1,owner_id:request.owner_id.clone(),doc_id:request.doc_id,locator:Some(locator(&request.owner_id))};
                if selected==1 {receipt.owner_id="another-owner".into();}
                if selected==2 {receipt.locator.as_mut().unwrap().entries[0].name="substituted.txt".into();}
                if selected==4 {receipt.locator=None;}
                (if selected==3 {HeaderMap::new()} else {receipt_header(CONTAINER_MEMBERS_VERSION_HEADER)},Json(receipt))
            }
        }));
    let (url,server)=serve(router).await;let client=RemoteClient::new(url,"test-token");let expected=locator("owner-a");let id=expected.member_id().unwrap();
    assert_eq!(client.read_container_member(&id,"owner-a").await.unwrap(),Some(expected));
    for bad in 1..=3 {mode.store(bad,Ordering::SeqCst);assert!(client.read_container_member(&id,"owner-a").await.is_err());}
    mode.store(4,Ordering::SeqCst);assert!(client.read_container_member("ordinary","owner-a").await.unwrap().is_none());
    assert!(client.read_container_member(&id,"").await.is_err());server.abort();
}

#[tokio::test]
async fn older_servers_never_receive_member_lookup_or_group_publication() {
    let calls=Arc::new(AtomicUsize::new(0));let seen=calls.clone();let writes=calls.clone();
    let router=Router::new().route("/v1/capabilities",get(||async{Json(capabilities(0))}))
        .route("/v1/members/read",post(move ||{let calls=seen.clone();async move {calls.fetch_add(1,Ordering::SeqCst);Json(serde_json::json!({}))}}))
        .route("/v1/ingest/containers",post(move ||{let calls=writes.clone();async move {calls.fetch_add(1,Ordering::SeqCst);Json(serde_json::json!({}))}}));
    let (url,server)=serve(router).await;let client=RemoteClient::new(url,"");
    assert!(client.read_container_member(&locator("a").member_id().unwrap(),"a").await.unwrap_err().to_string().contains("versioned container"));
    let batch=IngestBatch {container_group_version:None,chunks:vec![IngestChunk {container_source:Some(ContainerSourceReceipt {
        kind:ContainerKind::Zip,sha256:"a".repeat(64),size:100 }),..Default::default()}]};
    assert!(client.ingest_batch(&batch).await.is_err());assert_eq!(calls.load(Ordering::SeqCst),0);server.abort();
}

#[tokio::test]
async fn interactive_search_retains_validated_member_metadata_in_the_local_result_shape() {
    let mode=Arc::new(AtomicUsize::new(0));let selected=mode.clone();
    let router=Router::new().route("/v1/capabilities",get(||async{Json(capabilities(1))}))
        .route("/v1/search",post(move ||{let mode=selected.clone();async move {
            let member=locator("reader");let mut hit=SearchHit {doc_id:member.member_id().unwrap(),owner_id:"reader".into(),
                location_uri:member.member_uri().unwrap(),source_hash:"b".repeat(64),snippet:"member body".into(),
                container_member:Some(member),..Default::default()};
            if mode.load(Ordering::SeqCst)==1 {hit.source_hash="c".repeat(64);}
            if mode.load(Ordering::SeqCst)==2 {hit.container_member=None;}
            (receipt_header(QUERY_VERSION_HEADER),Json(vec![hit]))
        }}));
    let (url,server)=serve(router).await;let client=RemoteClient::new(url,"");
    let hits=client.search_text("member",&Default::default(),20).await.unwrap();
    let metadata:serde_json::Value=serde_json::from_str(hits[0].metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["container_member"],serde_json::to_value(locator("reader")).unwrap());
    for bad in 1..=2 {mode.store(bad,Ordering::SeqCst);assert!(client.search_text("member",&Default::default(),20).await.is_err());}
    server.abort();
}

#[tokio::test]
async fn container_batches_use_the_atomic_group_endpoint_and_require_its_receipt() {
    let calls=Arc::new(AtomicUsize::new(0));let seen=calls.clone();let receipt=Arc::new(AtomicUsize::new(1));let enabled=receipt.clone();
    let router=Router::new().route("/v1/capabilities",get(||async{Json(capabilities(1))}))
        .route("/v1/ingest/batch",post(||async{panic!("Container groups must never use the legacy endpoint");#[allow(unreachable_code)] Json(serde_json::json!({}))}))
        .route("/v1/ingest/containers",post(move |Json(batch):Json<IngestBatch>|{let calls=seen.clone();let enabled=enabled.clone();async move {
            calls.fetch_add(1,Ordering::SeqCst);assert_eq!(batch.container_group_version,Some(1));assert_eq!(batch.chunks.len(),2);
            assert!(batch.chunks[0].container_source.is_some());assert!(batch.chunks[1].container_member.is_some());
            (if enabled.load(Ordering::SeqCst)==1 {receipt_header(CONTAINER_MEMBERS_VERSION_HEADER)}else{HeaderMap::new()},
                Json(BatchIngestResponse {task_id:"queued-container".into(),queue_depth:1,chunk_count:2}))
        }}));
    let (url,server)=serve(router).await;let client=RemoteClient::new(url,"");let member=locator("reader");
    let batch=IngestBatch {container_group_version:None,chunks:vec![
        IngestChunk {doc_id:member.root_doc_id.clone(),owner_id:"reader".into(),location_uri:member.root_location_uri.clone(),source_hash:"a".repeat(64),
            container_source:Some(ContainerSourceReceipt {kind:ContainerKind::Zip,sha256:"a".repeat(64),size:100}),..Default::default()},
        IngestChunk {doc_id:member.member_id().unwrap(),owner_id:"reader".into(),location_uri:member.member_uri().unwrap(),source_hash:"b".repeat(64),
            container_member:Some(member),..Default::default()}]};
    assert_eq!(client.ingest_batch(&batch).await.unwrap().task_id,"queued-container");
    receipt.store(0,Ordering::SeqCst);assert!(client.ingest_batch(&batch).await.unwrap_err().to_string().contains("did not confirm"));
    assert_eq!(calls.load(Ordering::SeqCst),2);server.abort();
}
