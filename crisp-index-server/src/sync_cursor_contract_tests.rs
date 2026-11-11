use super::*;
use crisp_index_protocol::{SyncCursor, SYNC_CURSOR_VERSION};

fn wire(owner: &str, id: &str, text: &str) -> IngestChunk {
    IngestChunk {
        doc_id: id.into(),
        source_identity_version: None,
        owner_id: owner.into(),
        chunk_index: 0,
        chunk_total: Some(1),
        chunk_start_char: Some(0),
        chunk_end_char: Some(text.len() as i32),
        full_text: text.into(),
        full_text_md: None,
        headings: None,
        embedding: vec![1.0, 0.0],
        title: Some(id.into()),
        author: None,
        year: None,
        filename: format!("{id}.txt"),
        ext: "txt".into(),
        language: "en".into(),
        location_uri: format!("test://{owner}/{id}"),
        source_hash: format!("{owner}/{id}"),
        tags: vec![],
        ..Default::default()
    }
}

async fn open(path: &Path) -> SearchIndex {
    let vector = Arc::new(VectorStore::open_or_create(path, 2).await.unwrap());
    let fts = Arc::new(FtsStore::open_or_create(&path.join("fts-v2")).unwrap());
    let index = SearchIndex::new(vector, fts);
    index.ensure_ready().await.unwrap();
    index
}

#[tokio::test]
async fn sync_cursor_orders_timestamp_ties_and_pins_snapshot_across_writes() {
    let dir = tempfile::tempdir().unwrap();
    let index = open(dir.path()).await;
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None, chunks: vec![
                wire("b", "same", "one"),
                wire("a", "z", "two"),
                wire("a", "same", "three"),
                wire("a'owner", "quoted'id", "four"),
                wire("a", "early", "five"),
            ],
        })
        .await
        .unwrap();
    // Manufacture the exact millisecond tie that broke timestamp-only pages.
    index
        .vector
        .table
        .update()
        .column("indexed_at", "to_timestamp_millis(1000)")
        .execute()
        .await
        .unwrap();
    let first = index
        .vector
        .rows_since(&index.fts, 0, None, 2)
        .await
        .unwrap();
    assert!(first.has_more);
    assert_eq!(
        first
            .rows
            .iter()
            .map(|row| (&*row.owner_id, &*row.doc_id))
            .collect::<Vec<_>>(),
        vec![("a", "early"), ("a", "same")]
    );
    let cursor: SyncCursor = serde_json::from_str(first.next_cursor.as_ref().unwrap()).unwrap();
    let snapshot = cursor.snapshot_version;

    // Both a newly inserted earlier key and an updated earlier key must appear
    // in the next cycle, without shifting the in-flight immutable page order.
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None, chunks: vec![wire("a", "before", "new"), wire("a", "early", "updated")],
        })
        .await
        .unwrap();
    index
        .vector
        .table
        .update()
        .only_if("doc_id='before' OR doc_id='early'")
        .column("indexed_at", "to_timestamp_millis(1000)")
        .execute()
        .await
        .unwrap();
    let second = index
        .vector
        .rows_since(&index.fts, 0, Some(&cursor), 2)
        .await
        .unwrap();
    assert_eq!(second.snapshot_version, Some(snapshot));
    assert_eq!(
        second
            .rows
            .iter()
            .map(|row| (&*row.owner_id, &*row.doc_id))
            .collect::<Vec<_>>(),
        vec![("a", "z"), ("a'owner", "quoted'id")]
    );
    let next: SyncCursor = serde_json::from_str(second.next_cursor.as_ref().unwrap()).unwrap();
    let final_page = index
        .vector
        .rows_since(&index.fts, 0, Some(&next), 2)
        .await
        .unwrap();
    assert_eq!(
        final_page
            .rows
            .iter()
            .map(|row| (&*row.owner_id, &*row.doc_id))
            .collect::<Vec<_>>(),
        vec![("b", "same")]
    );
    assert!(!final_page.has_more && final_page.next_cursor.is_none());
    assert_eq!(final_page.max_indexed_at, 1000);

    // Never add one to the completed timestamp: same-time concurrent rows
    // preceding the old cursor remain visible on the next snapshot.
    let replay = index
        .vector
        .rows_since(&index.fts, final_page.max_indexed_at, None, 100)
        .await
        .unwrap();
    assert_eq!(replay.rows.len(), 6);
    assert!(replay.rows.iter().any(|row| row.doc_id == "before"));
    assert!(replay.snapshot_version.unwrap() > snapshot);
    // Snapshot checkout used an independent table handle; live writes remain possible.
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None, chunks: vec![wire("a", "still-writable", "live")],
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn sync_cursor_validates_boundaries_and_uses_exact_has_more() {
    let dir = tempfile::tempdir().unwrap();
    let index = open(dir.path()).await;
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None, chunks: vec![wire("a", "one", "hello"), wire("a", "two", "world")],
        })
        .await
        .unwrap();
    let page = index
        .vector
        .rows_since(&index.fts, 0, None, 2)
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert!(!page.has_more && page.next_cursor.is_none());
    assert!(index
        .vector
        .rows_since(&index.fts, 0, None, 0)
        .await
        .is_err());
    assert!(index
        .vector
        .rows_since(&index.fts, 0, None, 1001)
        .await
        .is_err());
    let mut cursor = SyncCursor {
        version: SYNC_CURSOR_VERSION,
        snapshot_version: u64::MAX,
        tombstone_horizon: i64::MAX,
        since_ms: 0,
        indexed_at: 0,
        owner_id: "a".into(),
        doc_id: "one".into(),
    };
    let error = index
        .vector
        .rows_since(&index.fts, 0, Some(&cursor), 2)
        .await
        .unwrap_err();
    assert!(error
        .downcast_ref::<sync_cursor::SyncSnapshotUnavailable>()
        .is_some());
    cursor.version += 1;
    assert!(index
        .vector
        .rows_since(&index.fts, 0, Some(&cursor), 2)
        .await
        .is_err());
    cursor.version = SYNC_CURSOR_VERSION;
    assert!(index
        .vector
        .rows_since(&index.fts, 1, Some(&cursor), 2)
        .await
        .is_err());
}

#[tokio::test]
async fn sync_pages_freeze_deletion_horizon_and_newer_reingest_suppresses_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    let index = open(dir.path()).await;
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None, chunks: vec![
                wire("a", "first", "first"),
                wire("a", "middle", "middle"),
                wire("a", "third", "third"),
            ],
        })
        .await
        .unwrap();
    let first = index
        .vector
        .rows_since(&index.fts, 0, None, 1)
        .await
        .unwrap();
    let cursor: SyncCursor = serde_json::from_str(first.next_cursor.as_ref().unwrap()).unwrap();
    index.delete_scoped_doc("middle", Some("a")).await.unwrap();
    let second = index
        .vector
        .rows_since(&index.fts, 0, Some(&cursor), 1)
        .await
        .unwrap();
    assert_eq!(second.rows[0].doc_id, "middle");
    assert!(second.deletions.is_empty());
    let next: SyncCursor = serde_json::from_str(second.next_cursor.as_ref().unwrap()).unwrap();
    let final_page = index
        .vector
        .rows_since(&index.fts, 0, Some(&next), 1)
        .await
        .unwrap();
    assert_eq!(final_page.rows[0].doc_id, "third");
    assert!(final_page.deletions.is_empty() && !final_page.has_more);
    let current = index
        .vector
        .rows_since(&index.fts, final_page.max_indexed_at, None, 100)
        .await
        .unwrap();
    assert_eq!(current.deletions.len(), 1);
    assert_eq!(current.deletions[0].doc_id, "middle");
    assert!(current.deletions[0].indexed_at > cursor.tombstone_horizon);
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None, chunks: vec![wire("a", "middle", "restored")],
        })
        .await
        .unwrap();
    let restored = index
        .vector
        .rows_since(&index.fts, 0, None, 100)
        .await
        .unwrap();
    assert!(restored.deletions.is_empty());
    assert!(restored.rows.iter().any(|row| row.doc_id == "middle"));
    index
        .ingest_batch(&IngestBatch {
            container_group_version: None, chunks: vec![wire("b", "middle", "other owner")],
        })
        .await
        .unwrap();
    index.delete_scoped_doc("middle", Some("a")).await.unwrap();
    let scoped = index
        .vector
        .rows_since(&index.fts, 0, None, 100)
        .await
        .unwrap();
    assert_eq!(scoped.deletions.len(), 1);
    assert_eq!(scoped.deletions[0].owner_id, "a");
    assert!(scoped
        .rows
        .iter()
        .any(|row| row.doc_id == "middle" && row.owner_id == "b"));
}
