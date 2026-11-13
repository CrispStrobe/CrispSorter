//! Source-bound tests for mounted transfers, publication, cancellation and logs.
pub mod drives {
    include!(concat!(env!("OUT_DIR"), "/drives.rs"));
}

#[path = "../../src-tauri/src/sync/transfer_queue.rs"]
pub mod transfer_queue;

#[path = "../../src-tauri/src/operation_log.rs"]
pub mod operation_log;

#[path = "../../src-tauri/src/sync/conflict.rs"]
pub mod sync_conflict_production;
#[path = "../../src-tauri/src/sync/paths.rs"]
pub mod sync_paths_production;
#[path = "../../src-tauri/src/sync/inventory.rs"]
pub mod sync_inventory_production;
pub mod sync {
    pub use crate::transfer_queue;
    pub use crate::sync_paths_production as paths;
    pub use crate::sync_inventory_production as inventory;
    pub use crate::sync_conflict_production as conflict;
    pub mod pairs {
        include!(concat!(env!("OUT_DIR"), "/pair_conflicts.rs"));
    }
}
mod file_conflicts {
    include!(concat!(env!("OUT_DIR"), "/mutation_conflicts.rs"));
}
#[cfg(test)]
mod file_conflict_tests;

static EVENTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn app_log(_level: &str, message: impl Into<String>) {
    EVENTS.lock().unwrap().push(message.into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn real_queue_emits_terminal_receipts_without_paths_or_provider_errors() {
        let queue = transfer_queue::TransferQueue::with_concurrency(1);
        let transfer = queue.submit_download("private-drive-label".into(),
            "private-document-name.pdf".into(), None,
            |_| anyhow::bail!("provider secret-token=secret-value"));
        let job_id = transfer.job_id;
        assert!(transfer.handle.await.unwrap().is_err());
        let events: Vec<serde_json::Value> = EVENTS.lock().unwrap().iter()
            .filter_map(|line| line.strip_prefix("operation "))
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["context"]["job_id"] == job_id && event["operation"] == "storage.download")
            .collect();
        assert_eq!(events.iter().filter(|event| event["phase"] == "started").count(), 1);
        assert_eq!(events.iter().filter(|event| event["phase"] == "failed").count(), 1);
        assert_eq!(events.iter().filter(|event| event["phase"] == "completed").count(), 0);
        let serialized = serde_json::to_string(&events).unwrap();
        for secret in ["private-drive-label", "private-document-name", "secret-token", "secret-value"] {
            assert!(!serialized.contains(secret), "storage log leaked {secret}");
        }
    }

    #[test]
    fn bounded_preview_checks_actual_bytes_instead_of_trusting_metadata() {
        use drives::CloudDrive;
        struct StaleMetadataDrive(drives::LocalDrive);
        impl CloudDrive for StaleMetadataDrive {
            fn label(&self) -> &str { "stale-metadata" }
            fn drive_type(&self) -> drives::DriveType { drives::DriveType::Local }
            fn list_dir(&self, _: &std::path::Path) -> anyhow::Result<Vec<drives::DirEntry>> { Ok(Vec::new()) }
            fn read_file(&self, _: &std::path::Path) -> anyhow::Result<Vec<u8>> { panic!("bounded reader used whole-file API") }
            fn write_file(&self, _: &std::path::Path, _: &[u8]) -> anyhow::Result<()> { anyhow::bail!("read-only") }
            fn delete(&self, _: &std::path::Path) -> anyhow::Result<()> { anyhow::bail!("read-only") }
            fn stat(&self, _: &std::path::Path) -> anyhow::Result<drives::FileStat> {
                Ok(drives::FileStat { size: 1, is_dir: false, mtime_unix: None })
            }
            fn read_file_to_writer(&self, path: &std::path::Path, writer: &mut dyn std::io::Write) -> anyhow::Result<u64> {
                self.0.read_file_to_writer(path, writer)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("grown.bin"), b"larger than the limit").unwrap();
        let drive = StaleMetadataDrive(drives::LocalDrive::new("local", directory.path()));
        assert!(drives::read_file_bounded(&drive, std::path::Path::new("grown.bin"), 5).is_err());
        assert_eq!(drives::read_file_bounded(&drive, std::path::Path::new("grown.bin"), 21).unwrap(), b"larger than the limit");
    }

    #[tokio::test]
    async fn real_local_upload_stream_is_verified_before_replacement() {
        use drives::CloudDrive;
        let directory = tempfile::tempdir().unwrap();
        let drive: Arc<dyn CloudDrive> = Arc::new(drives::LocalDrive::new("mounted", directory.path()));
        std::fs::write(directory.path().join("target.bin"), b"previous").unwrap();
        assert!(drive.write_file_from_reader(std::path::Path::new("target.bin"),
            &mut std::io::Cursor::new(b"short"), 100).is_err());
        assert_eq!(std::fs::read(directory.path().join("target.bin")).unwrap(), b"previous");
        let queue = transfer_queue::TransferQueue::new();
        let transfer = queue.submit_drive_upload("mounted".into(), drive, "target.bin".into(), b"complete".to_vec());
        transfer.handle.await.unwrap().unwrap();
        assert_eq!(std::fs::read(directory.path().join("target.bin")).unwrap(), b"complete");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[cfg(test)]
mod provider_search_tests {
    use super::drives::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingDrive {
        local: LocalDrive,
        listings: AtomicUsize,
        stats: AtomicUsize,
    }

    impl CountingDrive {
        fn new(root: &Path) -> Self {
            Self {
                local: LocalDrive::new("search-fixture", root),
                listings: AtomicUsize::new(0),
                stats: AtomicUsize::new(0),
            }
        }
    }

    impl CloudDrive for CountingDrive {
        fn label(&self) -> &str { "search-fixture" }
        fn drive_type(&self) -> DriveType { DriveType::Local }
        fn list_dir(&self, path: &Path) -> anyhow::Result<Vec<DirEntry>> {
            self.listings.fetch_add(1, Ordering::Relaxed);
            self.local.list_dir(path)
        }
        fn stat(&self, path: &Path) -> anyhow::Result<FileStat> {
            self.stats.fetch_add(1, Ordering::Relaxed);
            self.local.stat(path)
        }
        fn read_file(&self, path: &Path) -> anyhow::Result<Vec<u8>> { self.local.read_file(path) }
        fn write_file(&self, _: &Path, _: &[u8]) -> anyhow::Result<()> { anyhow::bail!("read-only fixture") }
        fn delete(&self, _: &Path) -> anyhow::Result<()> { anyhow::bail!("read-only fixture") }
    }

    fn scan(drive: &CountingDrive, depth: Option<usize>, budget: Option<usize>) -> WalkScan {
        walk_with_scan_budget(drive, Path::new(""), depth, budget, &mut |path, error| {
            panic!("unexpected listing failure at {}: {error}", path.display())
        })
    }

    #[test]
    fn provider_search_bounds_wide_empty_directory_tree() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..10 { std::fs::create_dir(root.path().join(format!("dir-{i}"))).unwrap(); }
        let drive = CountingDrive::new(root.path());
        let result = scan(&drive, None, Some(3));
        assert!(result.entries.is_empty());
        assert_eq!(result.scanned_entries, 3);
        assert!(result.limit_reached);
        assert_eq!(drive.listings.load(Ordering::Relaxed), 1);
        assert_eq!(drive.stats.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn provider_search_bounds_deep_directory_only_traversal() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b/c/d/e/f")).unwrap();
        let drive = CountingDrive::new(root.path());
        let result = scan(&drive, None, Some(3));
        assert!(result.entries.is_empty());
        assert_eq!(result.scanned_entries, 3);
        assert!(result.limit_reached);
        assert_eq!(drive.listings.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn provider_search_preserves_file_results_and_reports_actual_truncation() {
        let root = tempfile::tempdir().unwrap();
        for name in ["invoice-a.txt", "invoice-b.txt", "receipt.txt"] {
            std::fs::write(root.path().join(name), b"TOTAL: 42").unwrap();
        }
        let drive = CountingDrive::new(root.path());
        let result = scan(&drive, Some(2), Some(2));
        assert_eq!(result.scanned_entries, 2);
        assert!(result.limit_reached);
        assert_eq!(drive.stats.load(Ordering::Relaxed), 2);
        assert_eq!(result.entries.iter().map(|e| e.path.to_str().unwrap()).collect::<Vec<_>>(),
            ["invoice-a.txt", "invoice-b.txt"]);
        for entry in &result.entries {
            assert!(!entry.is_dir);
            assert_eq!(entry.size, Some(9));
            assert!(content_search_matches(&read_file_bounded(&drive, &entry.path, MAX_CONTENT_SEARCH_BYTES).unwrap(), "total"));
        }
        let complete = scan(&drive, Some(2), Some(3));
        assert_eq!(complete.scanned_entries, 3);
        assert_eq!(complete.entries.len(), 3);
        assert!(!complete.limit_reached, "exactly enough budget must not claim truncation");
    }

    #[test]
    fn provider_search_counts_directories_and_files_in_the_same_budget() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("a-dir")).unwrap();
        std::fs::write(root.path().join("b.txt"), b"file").unwrap();
        std::fs::write(root.path().join("a-dir/nested.txt"), b"nested").unwrap();
        let drive = CountingDrive::new(root.path());
        let result = scan(&drive, None, Some(2));
        assert_eq!(result.scanned_entries, 2);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].path, Path::new("b.txt"));
        assert!(result.limit_reached);
        assert_eq!(drive.listings.load(Ordering::Relaxed), 1);
        let complete = scan(&drive, None, None);
        assert_eq!(complete.scanned_entries, 3);
        assert_eq!(complete.entries.len(), 2);
        assert!(!complete.limit_reached);
    }

    #[test]
    fn provider_search_depth_bound_does_not_claim_scan_truncation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        let drive = CountingDrive::new(root.path());
        let result = scan(&drive, Some(0), Some(1));
        assert_eq!(result.scanned_entries, 1);
        assert!(!result.limit_reached);
        assert_eq!(drive.listings.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn provider_search_zero_budget_makes_no_provider_calls() {
        let root = tempfile::tempdir().unwrap();
        let drive = CountingDrive::new(root.path());
        let result = scan(&drive, None, Some(0));
        assert_eq!(result.scanned_entries, 0);
        assert!(result.entries.is_empty());
        assert!(result.limit_reached);
        assert_eq!(drive.listings.load(Ordering::Relaxed), 0);
        let empty = scan(&drive, None, Some(1));
        assert_eq!(empty.scanned_entries, 0);
        assert!(!empty.limit_reached);
    }

    #[test]
    fn provider_search_legacy_walk_wrappers_keep_file_and_error_semantics() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        std::fs::write(root.path().join("sub/f.txt"), b"x").unwrap();
        let drive = CountingDrive::new(root.path());
        let mut errors = 0;
        let mut on_error = |_: &Path, _: anyhow::Error| { errors += 1; };
        assert_eq!(walk(&drive, Path::new(""), None, &mut on_error).len(), 1);
        assert!(walk_limited(&drive, Path::new(""), None, Some(1), &mut on_error).is_empty());
        let missing = walk_with_scan_budget(&drive, Path::new("missing"), None, Some(1), &mut on_error);
        assert!(missing.entries.is_empty());
        assert_eq!(missing.scanned_entries, 0);
        assert!(!missing.limit_reached);
        assert_eq!(errors, 1);
    }
}
