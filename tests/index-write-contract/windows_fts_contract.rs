//! Exercise the same independent-index ingest pattern as the image fixtures.
//! Each task owns its directory; failures must not be hidden by retries.
use crate::index::{ingest::chunk_row_id, schema::DocumentChunk, LocalIndex};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_independent_image_indexes_commit_fts() {
    for round in 0..4 {
        let mut jobs = Vec::new();
        for task in 0..6 {
            jobs.push(tokio::spawn(async move {
                let dir = tempfile::tempdir().unwrap();
                let index = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
                let rows: Vec<DocumentChunk> = (0..8)
                    .map(|i| {
                        let id = format!("image-{round}-{task}-{i}");
                        serde_json::from_value(serde_json::json!({
                            "id": chunk_row_id(&id, -1), "doc_id": id, "owner_id": "reader",
                            "location_uri": format!("crisp+local://test/photos/{round}/{task}/{i}.jpg"),
                            "filename": format!("{i}.jpg"), "chunk_index": -1, "chunk_total": 0,
                            "source_hash": format!("hash-{round}-{task}-{i}"), "tags": [],
                            "metadata_json": "{\"level\":1,\"fs_size\":111111,\"parent_dir\":\"/photos\"}",
                            "indexed_at": 1700000000, "parent_dir": "/photos", "ext": "jpg"
                        }))
                        .unwrap()
                    })
                    .collect();
                index
                    .ingest_batch(&rows)
                    .await
                    .expect("independent index ingest and full-text commit");
            }));
        }
        for job in jobs {
            job.await.unwrap();
        }
    }
}
