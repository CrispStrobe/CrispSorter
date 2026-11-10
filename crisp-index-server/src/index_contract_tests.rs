use super::*;

fn wire(
    owner: &str,
    id: &str,
    index: i32,
    text: &str,
    total: Option<i32>,
    start: Option<i32>,
) -> IngestChunk {
    IngestChunk {
        doc_id: id.into(),
        owner_id: owner.into(),
        chunk_index: index,
        chunk_total: total,
        chunk_start_char: start,
        chunk_end_char: start.map(|start| start + text.len() as i32),
        full_text: text.into(),
        full_text_md: None,
        headings: Some(vec!["Report".into()]),
        embedding: vec![1.0, 0.0],
        title: Some(format!("Title {id}")),
        author: None,
        year: Some(2024),
        filename: format!("{id}.txt"),
        ext: "txt".into(),
        language: "en".into(),
        location_uri: format!("test://{owner}/{id}"),
        source_hash: id.into(),
        tags: Vec::new(),
    }
}
async fn open(path: &Path) -> SearchIndex {
    let vector = Arc::new(VectorStore::open_or_create(path, 2).await.unwrap());
    let fts = Arc::new(FtsStore::open_or_create(&path.join("fts-v2")).unwrap());
    let index = SearchIndex::new(vector, fts);
    index.ensure_ready().await.unwrap();
    index
}
fn owner(value: &str) -> SearchFilters {
    SearchFilters {
        owner_id: Some(value.into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn complete_replacements_remove_tails_and_duplicates_and_preserve_other_owners() {
    let dir = tempfile::tempdir().unwrap();
    let index = open(dir.path()).await;
    let batch = IngestBatch {
        chunks: vec![
            wire("a", "same", 0, "alpha beta", Some(2), Some(0)),
            wire("a", "same", 1, "beta gamma", Some(2), Some(6)),
            wire("b", "same", 0, "other owner", Some(1), Some(0)),
        ],
    };
    index.ingest_batch(&batch).await.unwrap();
    index.ingest_batch(&batch).await.unwrap();
    assert_eq!(index.vector.count().await.unwrap(), 3);
    assert_eq!(
        index
            .search_text("\"alpha beta gamma\"", &owner("a"), 20)
            .await
            .unwrap()
            .len(),
        1
    );
    let replacement = IngestBatch {
        chunks: vec![wire("a", "same", 0, "replacement", Some(1), Some(0))],
    };
    index.ingest_batch(&replacement).await.unwrap();
    index.ingest_batch(&replacement).await.unwrap();
    assert_eq!(index.vector.count().await.unwrap(), 2);
    assert!(index
        .search_text("gamma", &owner("a"), 20)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        index
            .search_text("replacement", &owner("a"), 20)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        index
            .search_text("other", &owner("b"), 20)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(index.delete_doc("same").await.unwrap_err().downcast_ref::<AmbiguousDocument>().is_some());
    assert!(index.update_location("same","test://ambiguous").await.is_err());
    index.delete_scoped_doc("same",Some("a")).await.unwrap();
    assert_eq!(index.vector.count().await.unwrap(),1);
    assert_eq!(index.search_text("other",&owner("b"),20).await.unwrap().len(),1);
    index.delete_scoped_doc("same",Some("b")).await.unwrap();
    assert_eq!(index.vector.count().await.unwrap(), 0);
    assert!(index
        .search_text("replacement OR other", &Default::default(), 20)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn partial_chunk_retry_keeps_siblings_and_invalid_offsets_leave_old_document_intact() {
    let dir = tempfile::tempdir().unwrap();
    let index = open(dir.path()).await;
    index
        .ingest_batch(&IngestBatch {
            chunks: vec![
                wire("a", "one", 0, "alpha", Some(2), None),
                wire("a", "one", 1, "beta", Some(2), None),
            ],
        })
        .await
        .unwrap();
    index
        .ingest_batch(&IngestBatch {
            chunks: vec![wire("a", "one", 1, "gamma", Some(2), None)],
        })
        .await
        .unwrap();
    assert_eq!(index.vector.count().await.unwrap(), 2);
    assert_eq!(
        index
            .search_text("alpha AND gamma", &owner("a"), 20)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(index
        .search_text("beta", &owner("a"), 20)
        .await
        .unwrap()
        .is_empty());
    let mut invalid = wire("a", "one", 0, "corrupt", Some(1), Some(0));
    invalid.chunk_end_char = Some(2);
    assert!(index
        .ingest_batch(&IngestBatch {
            chunks: vec![invalid]
        })
        .await
        .is_err());
    assert!(index.fts.pending().unwrap().is_empty());
    assert_eq!(
        index
            .search_text("alpha AND gamma", &owner("a"), 20)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn reads_repair_a_vector_commit_that_was_interrupted_before_fts_publication() {
    let dir = tempfile::tempdir().unwrap();
    let index = open(dir.path()).await;
    index
        .ingest_batch(&IngestBatch {
            chunks: vec![wire("a", "one", 0, "before", Some(1), Some(0))],
        })
        .await
        .unwrap();
    let staged = index
        .vector
        .prepare_upsert(&[wire("a", "one", 0, "after", Some(1), Some(0))])
        .await
        .unwrap();
    index.fts.mark_pending(&staged.keys).unwrap();
    index.vector.commit_upsert(&staged).await.unwrap();
    assert!(!index.fts.capabilities().unwrap().fts_ready);
    drop(index);
    let reopened = open(dir.path()).await;
    assert!(reopened
        .search_text("before", &owner("a"), 20)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        reopened
            .search_text("after", &owner("a"), 20)
            .await
            .unwrap()[0]
            .snippet,
        "after"
    );
    assert!(reopened.fts.pending().unwrap().is_empty());
    // A pending intent before a vector commit repairs the unchanged old version.
    reopened
        .fts
        .mark_pending(&[DocumentKey {
            owner_id: "a".into(),
            doc_id: "one".into(),
        }])
        .unwrap();
    assert_eq!(
        reopened
            .search_text("after", &owner("a"), 20)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn strict_hybrid_candidates_obey_boolean_phrase_field_and_owner_constraints() {
    let dir = tempfile::tempdir().unwrap();
    let index = open(dir.path()).await;
    let mut yes = wire("a", "yes", 0, "alpha beta", Some(1), Some(0));
    yes.title = Some("Allowed title".into());
    yes.embedding = vec![0.8, 0.2];
    let no = wire("a", "no", 0, "alpha excluded beta", Some(1), Some(0));
    let other = wire("b", "other", 0, "alpha beta", Some(1), Some(0));
    index
        .ingest_batch(&IngestBatch {
            chunks: vec![yes, no, other],
        })
        .await
        .unwrap();
    for query in [
        "alpha AND NOT excluded",
        "\"alpha beta\"",
        "alpha pre/0 beta",
        "title:\"allowed title\"",
    ] {
        let hits = index
            .search_hybrid(query, &[1.0, 0.0], &owner("a"), 20)
            .await
            .unwrap();
        assert_eq!(
            hits.iter()
                .map(|hit| hit.doc_id.as_str())
                .collect::<Vec<_>>(),
            vec!["yes"],
            "{query}"
        );
        assert!(hits.iter().all(|hit| hit.owner_id == "a"));
    }
    assert_eq!(
        index
            .search_hybrid("unrelated natural question", &[1.0, 0.0], &owner("a"), 20)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(index
        .search_hybrid("alpha AND", &[1.0, 0.0], &owner("a"), 20)
        .await
        .is_err());
}

#[tokio::test]
async fn migration_reads_every_legacy_chunk_without_inventing_adjacency() {
    let dir = tempfile::tempdir().unwrap();
    let vector = Arc::new(VectorStore::open_or_create(dir.path(), 2).await.unwrap());
    let schema = build_lance_schema(2);
    let chunks = [
        wire("a", "legacy", 0, "opening alpha", None, None),
        wire("a", "legacy", 1, "beta ending", None, None),
    ];
    let batches = chunks
        .iter()
        .map(|chunk| chunk_to_record_batch(chunk, 2, &schema).unwrap())
        .collect::<Vec<_>>();
    let reader = arrow_array::RecordBatchIterator::new(
        batches
            .into_iter()
            .map(Ok::<RecordBatch, arrow_schema::ArrowError>),
        schema,
    );
    vector.table.add(reader).execute().await.unwrap();
    let legacy = dir.path().join("fts");
    std::fs::create_dir(&legacy).unwrap();
    std::fs::write(legacy.join("prior"), "preserved").unwrap();
    let fts = Arc::new(FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index = SearchIndex::new(vector, fts);
    index.ensure_ready().await.unwrap();
    assert_eq!(
        index
            .search_text("ending", &owner("a"), 20)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(index
        .search_text("\"alpha beta\"", &owner("a"), 20)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read_to_string(legacy.join("prior")).unwrap(),
        "preserved"
    );
}

#[tokio::test]
async fn finite_embedding_validation_and_unicode_snippets_are_safe() {
    let dir = tempfile::tempdir().unwrap();
    let index = open(dir.path()).await;
    let text = format!("{} marker", "é".repeat(301));
    index
        .ingest_batch(&IngestBatch {
            chunks: vec![wire("a", "unicode", 0, &text, Some(1), Some(0))],
        })
        .await
        .unwrap();
    let result = index
        .search_vector(&[1.0, 0.0], &owner("a"), 20)
        .await
        .unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].snippet.ends_with('…'));
    for invalid in [vec![0.0, 0.0], vec![f32::NAN, 0.0], vec![1.0]] {
        assert!(index
            .search_vector(&invalid, &owner("a"), 20)
            .await
            .is_err());
    }
    assert!(index
        .search_text("marker", &owner("a"), 0)
        .await
        .unwrap()
        .is_empty());
    assert!(index.search_text("marker", &owner("a"), 201).await.is_err());
}

#[tokio::test]
async fn migration_canonicalizes_legacy_metadata_and_embeddings_with_equal_timestamps() {
    let dir=tempfile::tempdir().unwrap();
    let vector=Arc::new(VectorStore::open_or_create(dir.path(),2).await.unwrap());
    let mut old=wire("a","legacy",0,"obsolete body",None,None);
    old.title=Some("Old title".into());old.location_uri="test://old".into();old.embedding=vec![1.0,0.0];
    let mut current=wire("a","legacy",0,"current body",None,None);
    current.title=Some("Current title".into());current.location_uri="test://current".into();current.embedding=vec![0.0,1.0];
    let neighbor=wire("a","neighbor",0,"neighbor text",None,None);
    let schema=build_lance_schema(2);
    let mut batches=Vec::new();
    for chunk in [&old,&current,&neighbor] {
        let batch=chunk_to_record_batch(chunk,2,&schema).unwrap();
        let mut columns=batch.columns().to_vec();
        columns[schema.index_of("indexed_at").unwrap()]=Arc::new(TimestampMillisecondArray::from(vec![1_000]));
        batches.push(RecordBatch::try_new(schema.clone(),columns).unwrap());
    }
    let reader=arrow_array::RecordBatchIterator::new(batches.into_iter().map(Ok::<RecordBatch,arrow_schema::ArrowError>),schema);
    vector.table.add(reader).execute().await.unwrap();
    let fts=Arc::new(FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index=SearchIndex::new(vector,fts);index.ensure_ready().await.unwrap();
    assert_eq!(index.vector.count().await.unwrap(),2);
    let hit=index.search_text("current",&owner("a"),20).await.unwrap().remove(0);
    assert_eq!(hit.title.as_deref(),Some("Current title"));assert_eq!(hit.location_uri,"test://current");
    assert!(index.search_text("obsolete",&owner("a"),20).await.unwrap().is_empty());
    let nearest=index.search_vector(&[1.0,0.0],&owner("a"),20).await.unwrap();
    assert_eq!(nearest[0].doc_id,"neighbor");
    assert_eq!(nearest.iter().find(|hit|hit.doc_id=="legacy").unwrap().snippet,"current body");
}

#[tokio::test]
async fn failed_legacy_rebuild_preserves_existing_index_and_vector_data() {
    let dir=tempfile::tempdir().unwrap();
    let vector=Arc::new(VectorStore::open_or_create(dir.path(),2).await.unwrap());
    let mut invalid=wire("a","invalid",0,"broken offsets",None,Some(0));invalid.chunk_end_char=Some(2);
    let schema=build_lance_schema(2);let batch=chunk_to_record_batch(&invalid,2,&schema).unwrap();
    vector.table.add(arrow_array::RecordBatchIterator::new(vec![Ok(batch)],schema)).execute().await.unwrap();
    let legacy=dir.path().join("fts");std::fs::create_dir(&legacy).unwrap();std::fs::write(legacy.join("prior"),"preserved").unwrap();
    let fts=Arc::new(FtsStore::open_or_create(&dir.path().join("fts-v2")).unwrap());
    let index=SearchIndex::new(vector,fts);
    assert!(index.ensure_ready().await.is_err());assert!(index.fts.needs_rebuild().unwrap());
    assert_eq!(index.vector.count().await.unwrap(),1);
    assert_eq!(std::fs::read_to_string(legacy.join("prior")).unwrap(),"preserved");
}

#[tokio::test]
async fn partial_updates_and_scoped_location_moves_advance_a_durable_document_clock() {
    let dir=tempfile::tempdir().unwrap();let index=open(dir.path()).await;
    let future=i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap()+86_400_000;
    index.fts.observe_timestamp(future).unwrap();
    index.ingest_batch(&IngestBatch{chunks:vec![wire("a","one",0,"alpha",Some(2),None),wire("a","one",1,"beta",Some(2),None)]}).await.unwrap();
    let key=DocumentKey{owner_id:"a".into(),doc_id:"one".into()};
    let rows=index.vector.fetch_by_keys(std::slice::from_ref(&key)).await.unwrap();
    let before=stored_chunks(&rows[0]).unwrap()[0].indexed_at;assert!(before>future);
    index.ingest_batch(&IngestBatch{chunks:vec![wire("a","one",1,"gamma",Some(2),None)]}).await.unwrap();
    let rows=index.vector.fetch_by_keys(std::slice::from_ref(&key)).await.unwrap();
    let chunks=rows.iter().flat_map(|batch|stored_chunks(batch).unwrap()).collect::<Vec<_>>();
    let updated=chunks[0].indexed_at;assert!(updated>before);assert!(chunks.iter().all(|chunk|chunk.indexed_at==updated));
    index.update_scoped_location("one","test://moved",Some("a")).await.unwrap();
    let rows=index.vector.fetch_by_keys(std::slice::from_ref(&key)).await.unwrap();
    assert!(rows.iter().flat_map(|batch|stored_chunks(batch).unwrap()).all(|chunk|chunk.indexed_at>updated));
    assert_eq!(index.search_text("alpha",&owner("a"),20).await.unwrap()[0].location_uri,"test://moved");
    assert_eq!(index.stats().await.unwrap(),(2,1));
}
