//! Exercise the same independent-index ingest pattern as the image fixtures.
//! Each task owns its directory; failures must not be hidden by retries.
use crate::index::{ingest::chunk_row_id, schema::DocumentChunk, LocalIndex};
use anyhow::Context;

const ROUNDS: usize = 20;
const INDEXES_PER_ROUND: usize = 6;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_independent_image_indexes_commit_fts() {
    let mode = if crate::fts_probe::enabled() { "single-buffer trace" } else { "production control" };
    eprintln!("Windows FTS {mode}: {ROUNDS} rounds x {INDEXES_PER_ROUND} independent indexes = {} attempts; no retries", ROUNDS * INDEXES_PER_ROUND);
    let mut failures = Vec::new();
    let mut completed = 0;
    for round in 0..ROUNDS {
        let mut jobs = Vec::new();
        for task in 0..INDEXES_PER_ROUND {
            jobs.push((task, tokio::spawn(async move {
                let dir = tempfile::tempdir().context("create independent fixture")?;
                eprintln!("Windows FTS {mode}: round {round}, index {task}, fixture {:?}", dir.path().file_name().unwrap());
                let index = LocalIndex::open_or_create(dir.path(), 4).await.context("open independent index")?;
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
                    .context("independent index ingest and full-text commit")?;
                Ok::<(), anyhow::Error>(())
            })));
        }
        // Await every task, and continue independent rounds after errors. These
        // are fresh fixtures, never retries of a failed transaction.
        for (task, job) in jobs {
            completed += 1;
            let failure = match job.await {
                Ok(Ok(())) => continue,
                Ok(Err(error)) => format!("round {round}, index {task}: {error:#}"),
                Err(error) => format!("round {round}, index {task}: task failed: {error}"),
            };
            eprintln!("Windows FTS {mode} failure: {failure}");
            failures.push(failure);
        }
    }
    eprintln!("Windows FTS {mode}: {completed} attempts completed, {} failures", failures.len());
    assert_eq!(completed, ROUNDS * INDEXES_PER_ROUND);
    assert!(failures.is_empty(), "Windows FTS {mode} failures:\n{}", failures.join("\n"));
}

// Windows permits reading a read-only file, but rejects replacing it. Target
// only Tantivy's final metadata publication: segment writes and writer locking
// remain real and writable. Unix file mode bits do not model this failure.
#[cfg(windows)]
mod commit_failure {
    use super::*;
    use crate::index::FtsIndex;
    use std::{fs, path::{Path, PathBuf}};
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    use tantivy::directory::{Directory, TerminatingWrite};

    struct RestorePermissions { path: PathBuf, original: fs::Permissions }
    impl RestorePermissions {
        fn make_read_only(path: &Path) -> Self {
            let original = fs::metadata(path).unwrap().permissions();
            assert!(!original.readonly());
            let mut read_only = original.clone();
            read_only.set_readonly(true);
            fs::set_permissions(path, read_only).unwrap();
            Self { path: path.to_owned(), original }
        }
        fn restore(&self) -> std::io::Result<()> {
            fs::set_permissions(&self.path, self.original.clone())
        }
    }
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            // Cleanup must never panic while another assertion is unwinding.
            // The normal path below explicitly asserts restoration succeeds.
            let _ = self.restore();
        }
    }
    fn hold_without_delete_sharing(path: &Path) -> fs::File {
        // Read-only attributes did not prevent Rust 1.99 from deleting these
        // fixtures. Deny FILE_SHARE_DELETE explicitly to inject a real sharing
        // failure without relying on attribute-dependent removal behavior.
        fs::OpenOptions::new().read(true).share_mode(0x1 | 0x2)
            .open(path).unwrap() // FILE_SHARE_READ | FILE_SHARE_WRITE
    }
    fn pending_payloads(path: &Path) -> Vec<String> {
        let connection = rusqlite::Connection::open(path).unwrap();
        let mut statement = connection.prepare("SELECT payload FROM pending_fts_writes ORDER BY rowid").unwrap();
        let payloads = statement.query_map([], |row| row.get(0)).unwrap()
            .collect::<Result<Vec<String>, _>>().unwrap();
        payloads
    }
    fn chunk(body: &str) -> DocumentChunk {
        serde_json::from_value(serde_json::json!({
            "id": chunk_row_id("commit-fixture", 0), "doc_id": "commit-fixture",
            "owner_id": "reader", "location_uri": "crisp+local://test/commit.txt",
            "filename": "commit.txt", "chunk_index": 0, "chunk_total": 1,
            "source_hash": "commit-fixture", "tags": [], "full_text": body,
            "metadata_json": "{\"level\":3}", "indexed_at": 1700000000,
            "parent_dir": "/test", "ext": "txt"
        })).unwrap()
    }

    #[tokio::test]
    async fn windows_fts_atomic_commit_failure_retains_journal_and_replays_once_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let fts_path = dir.path().join("fts");
        let meta_path = fts_path.join("meta.json");
        let journal_path = dir.path().join("index-write-journal.sqlite");
        let local = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
        let fts = FtsIndex::open_or_create(&fts_path).unwrap();
        local.ingest_batch_with_fts(&[chunk("priorstable")], &[], &fts).await.unwrap();
        assert!(pending_payloads(&journal_path).is_empty());
        let old_meta = fs::read(&meta_path).unwrap();
        let baseline_files = fts.index.directory().list_managed_files();
        let untracked_path = fts_path.join("unmanaged.6.del");
        fs::write(&untracked_path, b"untracked fixture must survive").unwrap();
        let doc_id = fts.search("priorstable", &Default::default(), 10).unwrap()[0].doc_id.clone();

        eprintln!("Expected synthetic atomic-publication denial for {:?}/fts/meta.json; this is separate from stress failures", dir.path().file_name().unwrap());
        let restore = RestorePermissions::make_read_only(&meta_path);
        // Explicitly prove this injection does not prevent writer acquisition.
        drop(fts.writer().expect("read-only meta.json must still allow acquiring the writer"));
        let error = local.ingest_batch_with_fts(&[chunk("replacementonly")], &[], &fts).await.unwrap_err();
        assert!(format!("{error:#}").contains("committing full-text repair"), "{error:#}");
        assert!(!format!("{error:#}").contains("opening full-text writer"), "{error:#}");
        let io_error = error.chain().find_map(|cause| match cause.downcast_ref::<tantivy::TantivyError>() {
            Some(tantivy::TantivyError::IoError(error)) => Some(error),
            _ => None,
        }).expect("commit must fail with the actual filesystem I/O error");
        assert_eq!(io_error.raw_os_error(), Some(5));
        eprintln!("Expected synthetic denial verified inside full-text commit with raw OS code 5");
        assert_eq!(fs::read(&meta_path).unwrap(), old_meta, "failed atomic publication changed committed metadata");
        assert_eq!(fts.search("priorstable", &Default::default(), 10).unwrap().len(), 1);
        assert!(fts.search("replacementonly", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(local.table_ref().count_rows(None).await.unwrap(), 1);
        assert_eq!(local.fetch_full_text(&doc_id).await.unwrap(), "replacementonly");
        let pending = pending_payloads(&journal_path);
        assert_eq!(pending.len(), 1);
        let payload: serde_json::Value = serde_json::from_str(&pending[0]).unwrap();
        let generation = payload["generation"].as_str().unwrap();
        let receipt_rows: Vec<String> = serde_json::from_value(payload["receipt_rows"].clone()).unwrap();
        assert!(local.row_generation_committed(&receipt_rows, generation).await.unwrap());
        assert_eq!(payload["inputs"][0]["body"], "replacementonly");
        let orphan_deletes: Vec<_> = fts.index.directory().list_managed_files().difference(&baseline_files)
            .filter(|path| path.extension().is_some_and(|extension| extension == "del"))
            .cloned().collect();
        assert_eq!(orphan_deletes.len(), 1, "failed publication must leave the colliding managed deletion artifact");
        let orphan_path = fts_path.join(&orphan_deletes[0]);
        let orphan_bytes = fs::read(&orphan_path).unwrap();
        restore.restore().expect("restore original metadata permissions before reopening");
        drop(restore);
        drop(fts);
        drop(local);

        let reopened = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
        let reopened_fts = FtsIndex::open_or_create(&fts_path).unwrap();
        assert_eq!(pending_payloads(&journal_path), pending, "intent did not survive reopening");
        assert!(reopened.row_generation_committed(&receipt_rows, generation).await.unwrap());
        assert_eq!(reopened_fts.search("priorstable", &Default::default(), 10).unwrap().len(), 1);
        let held_writer = reopened_fts.writer().unwrap();
        let locked = reopened.read_guard(&reopened_fts).await.err().expect("recovery must respect an existing Tantivy writer");
        assert!(format!("{locked:#}").contains("opening full-text writer for recovery"));
        assert_eq!(pending_payloads(&journal_path), pending);
        assert_eq!(fs::read(&meta_path).unwrap(), old_meta);
        assert_eq!(fs::read(&orphan_path).unwrap(), orphan_bytes);
        drop(held_writer);

        let held_reader: tantivy::IndexReader = reopened_fts.index.reader_builder()
            .reload_policy(tantivy::ReloadPolicy::Manual).try_into().unwrap();
        let held_searcher = held_reader.searcher();
        let prior_query = tantivy::query::TermQuery::new(
            tantivy::Term::from_field_text(reopened_fts.fields.body, "priorstable"),
            tantivy::schema::IndexRecordOption::Basic);
        let committed_files: Vec<_> = reopened_fts.index.searchable_segment_metas().unwrap().iter()
            .flat_map(|meta| meta.list_files()).filter_map(|path| {
                fs::read(fts_path.join(&path)).ok().map(|bytes| (path, bytes))
            }).collect();
        assert!(!committed_files.is_empty());
        // Mapped snapshots can remain readable after obsolete paths are unlinked.
        // This fixture also demands path/byte preservation, so explicitly model
        // a reader that denies deletion rather than assuming mapping does that.
        let held_committed_files: Vec<_> = committed_files.iter()
            .map(|(path, _)| hold_without_delete_sharing(&fts_path.join(path))).collect();
        eprintln!("Expected snapshot path retention: committed-file readers deny delete sharing until released");

        eprintln!("Expected synthetic GC failures: delete-sharing-locked orphan and read-only managed-file manifest");
        let held_orphan = hold_without_delete_sharing(&orphan_path);
        let restore_manifest = RestorePermissions::make_read_only(&fts_path.join(".managed.json"));
        let gc_error = reopened.read_guard(&reopened_fts).await.err().expect("GC errors must retain the repair intent");
        assert!(format!("{gc_error:#}").contains("collecting orphaned full-text files before recovery"), "{gc_error:#}");
        assert_eq!(pending_payloads(&journal_path), pending);
        assert_eq!(fs::read(&meta_path).unwrap(), old_meta);
        restore_manifest.restore().expect("restore managed-file manifest permissions");
        drop(restore_manifest);
        let incomplete = reopened.read_guard(&reopened_fts).await.err().expect("partial GC must block replay");
        assert!(format!("{incomplete:#}").contains("cleanup is incomplete"), "{incomplete:#}");
        assert_eq!(pending_payloads(&journal_path), pending);
        assert_eq!(fs::read(&meta_path).unwrap(), old_meta);
        assert_eq!(fs::read(&orphan_path).unwrap(), orphan_bytes);
        assert_eq!(held_searcher.search(&prior_query, &tantivy::collector::Count).unwrap(), 1);
        for (path, bytes) in &committed_files { assert_eq!(fs::read(fts_path.join(path)).unwrap(), *bytes); }
        assert_eq!(fs::read(&untracked_path).unwrap(), b"untracked fixture must survive");
        drop(held_orphan);

        // A different obsolete managed file can be held by another reader.
        // Deny delete sharing explicitly so this Windows availability case is
        // deterministic even if the OS can unlink some other mapped handles.
        let obsolete = Path::new("obsolete-reader-held.bin");
        let obsolete_path = fts_path.join(obsolete);
        let owner = reopened_fts.writer().unwrap();
        let mut file = reopened_fts.index.directory().open_write(obsolete).unwrap();
        file.write_all(b"obsolete managed reader fixture").unwrap();
        file.terminate().unwrap();
        drop(owner);
        let obsolete_bytes = fs::read(&obsolete_path).unwrap();
        let held_file = hold_without_delete_sharing(&obsolete_path);
        let held_mapping = reopened_fts.index.directory().open_read(obsolete).unwrap();
        eprintln!("Expected synthetic availability block: obsolete mapped managed file denies delete sharing");
        let mapped = reopened.read_guard(&reopened_fts).await.err().expect("unrelated mapped-file cleanup must retain the repair intent");
        assert!(format!("{mapped:#}").contains("cleanup is incomplete"), "{mapped:#}");
        assert_eq!(pending_payloads(&journal_path), pending);
        assert_eq!(fs::read(&meta_path).unwrap(), old_meta);
        assert_eq!(fs::read(&obsolete_path).unwrap(), obsolete_bytes);
        assert_eq!(held_searcher.search(&prior_query, &tantivy::collector::Count).unwrap(), 1);
        for (path, bytes) in &committed_files { assert_eq!(fs::read(fts_path.join(path)).unwrap(), *bytes); }
        assert_eq!(fs::read(&untracked_path).unwrap(), b"untracked fixture must survive");
        drop(held_mapping);
        drop(held_file);

        { let _guard = reopened.read_guard(&reopened_fts).await.unwrap(); }
        assert!(pending_payloads(&journal_path).is_empty());
        assert!(!obsolete_path.exists(), "released obsolete managed file was not collected");
        assert_eq!(held_searcher.search(&prior_query, &tantivy::collector::Count).unwrap(), 1);
        for (path, bytes) in &committed_files { assert_eq!(fs::read(fts_path.join(path)).unwrap(), *bytes); }
        assert_eq!(fs::read(&untracked_path).unwrap(), b"untracked fixture must survive");
        assert_eq!(reopened_fts.search("replacementonly", &Default::default(), 10).unwrap().len(), 1);
        assert!(reopened_fts.search("priorstable", &Default::default(), 10).unwrap().is_empty());
        assert_eq!(reopened_fts.doc_count(), 1);
        assert_eq!(reopened.table_ref().count_rows(None).await.unwrap(), 1);
        assert_eq!(reopened.fetch_full_text(&doc_id).await.unwrap(), "replacementonly");
        let repaired_meta = fs::read(&meta_path).unwrap();
        assert_ne!(repaired_meta, old_meta);
        { let _guard = reopened.read_guard(&reopened_fts).await.unwrap(); }
        assert_eq!(fs::read(&meta_path).unwrap(), repaired_meta, "second recovery committed again");
        assert!(pending_payloads(&journal_path).is_empty());
        assert_eq!(reopened_fts.doc_count(), 1);
        drop(held_committed_files);
        drop(held_searcher);
        drop(held_reader);
        drop(reopened_fts);
        drop(reopened);
        let again = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
        let again_fts = FtsIndex::open_or_create(&fts_path).unwrap();
        { let _guard = again.read_guard(&again_fts).await.unwrap(); }
        assert!(pending_payloads(&journal_path).is_empty());
        assert_eq!(fs::read(&meta_path).unwrap(), repaired_meta, "later reopening replayed a completed intent");
        assert_eq!(again_fts.doc_count(), 1);
        assert_eq!(again_fts.search("replacementonly", &Default::default(), 10).unwrap().len(), 1);
        eprintln!("Synthetic atomic-publication denial recovered: one replay, no duplicate commit after subsequent recovery/reopen");
    }

    #[tokio::test]
    async fn windows_fts_atomic_commit_failure_cleanup_skips_uncommitted_intents_and_first_pass_writes() {
        let dir = tempfile::tempdir().unwrap();
        let fts_path = dir.path().join("fts");
        let journal_path = dir.path().join("index-write-journal.sqlite");
        let local = LocalIndex::open_or_create(dir.path(), 4).await.unwrap();
        let fts = FtsIndex::open_or_create(&fts_path).unwrap();
        local.ingest_batch_with_fts(&[chunk("priorstable")], &[], &fts).await.unwrap();
        let orphan = Path::new("uncommitted-orphan.del");
        let mut writer = fts.index.directory().open_write(orphan).unwrap();
        writer.write_all(b"managed orphan fixture").unwrap();
        writer.terminate().unwrap();
        let orphan_path = fts_path.join(orphan);
        let orphan_bytes = fs::read(&orphan_path).unwrap();
        eprintln!("Expected synthetic no-GC check: managed orphan denies delete sharing");
        let held_orphan = hold_without_delete_sharing(&orphan_path);
        let generation = "not-committed-in-vector-store";
        let payload = serde_json::json!({
            "generation": generation, "doc_ids": ["commit-fixture"],
            "replace_ids": ["commit-fixture"], "inputs": [],
            "receipt_rows": ["missing-vector-row"], "delete_only": false
        });
        rusqlite::Connection::open(&journal_path).unwrap().execute(
            "INSERT INTO pending_fts_writes(generation,payload) VALUES (?1,?2)",
            rusqlite::params![generation, payload.to_string()]).unwrap();
        let old_meta = fs::read(fts_path.join("meta.json")).unwrap();
        // No vector proof means no GC and no attempt to acquire the writer.
        let held_writer = fts.writer().unwrap();
        { let _guard = local.read_guard(&fts).await.unwrap(); }
        assert!(pending_payloads(&journal_path).is_empty());
        assert_eq!(fs::read(fts_path.join("meta.json")).unwrap(), old_meta);
        assert_eq!(fs::read(&orphan_path).unwrap(), orphan_bytes);
        assert!(fts.index.directory().list_managed_files().contains(orphan));
        drop(held_writer);
        // First-pass writes must not inherit replay's strict GC precondition.
        // Tantivy's ordinary post-commit GC may leave this share-locked orphan.
        local.ingest_batch_with_fts(&[chunk("firstpassonly")], &[], &fts).await.unwrap();
        assert!(pending_payloads(&journal_path).is_empty());
        assert_eq!(fts.search("firstpassonly", &Default::default(), 10).unwrap().len(), 1);
        assert_eq!(fts.doc_count(), 1);
        assert_eq!(fs::read(&orphan_path).unwrap(), orphan_bytes);
        drop(held_orphan);
    }

}
