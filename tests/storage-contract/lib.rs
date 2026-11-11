//! Source-bound tests for mounted transfers, publication, cancellation and logs.
pub mod drives {
    include!(concat!(env!("OUT_DIR"), "/drives.rs"));
}

#[path = "../../src-tauri/src/sync/transfer_queue.rs"]
pub mod transfer_queue;

#[path = "../../src-tauri/src/operation_log.rs"]
pub mod operation_log;

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
