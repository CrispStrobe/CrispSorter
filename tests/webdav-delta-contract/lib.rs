#![allow(dead_code)]
#[path = "../../src-tauri/src/sync/delta.rs"]
pub mod production_delta;
pub mod sync {
    pub use crate::production_delta as delta;
}
#[path = "../../src-tauri/src/drives/webdav_delta_download.rs"]
mod webdav_delta_download;

#[cfg(test)]
mod actual_wiring {
    use super::{production_delta::Blockmap, webdav_delta_download as delta_download};
    use anyhow::{anyhow, Result};
    use std::path::Path;
    #[derive(Clone)]
    struct ServerMap {
        blockmap: Blockmap,
        etag: Option<String>,
    }
    struct WebDavDrive {
        remote: Vec<u8>,
        fail_at: Option<u64>,
    }
    struct DeltaTransferResult {
        changed_blocks: usize,
        total_blocks: usize,
        transferred_bytes: u64,
        total_bytes: u64,
        etag: Option<String>,
    }
    impl WebDavDrive {
        fn fetch_delta_map(&self, _: &Path) -> Result<Option<ServerMap>> {
            Ok(Some(ServerMap {
                blockmap: super::production_delta::compute_blockmap_from_bytes(&self.remote, 1024)?,
                etag: Some("fixture".into()),
            }))
        }
        fn blockmap_from_server(map: &ServerMap) -> Result<Blockmap> {
            Ok(map.blockmap.clone())
        }
        fn read_range(&self, _: &Path, start: u64, end: u64) -> Result<Vec<u8>> {
            if self.fail_at == Some(start) {
                return Err(anyhow!("injected later HTTP failure"));
            }
            Ok(self.remote[start as usize..=end as usize].to_vec())
        }
    }
    include!(concat!(env!("OUT_DIR"), "/wiring.rs"));
    #[test]
    fn actual_delta_download_stages_before_mutation_and_reports_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        let original = vec![b'a'; 3072];
        std::fs::write(&path, &original).unwrap();
        let mut drive = WebDavDrive {
            remote: vec![b'b'; 3072],
            fail_at: Some(1024),
        };
        assert!(drive
            .delta_download_file(Path::new("remote"), &path)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        drive.fail_at = None;
        let result = drive
            .delta_download_file(Path::new("remote"), &path)
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                result.changed_blocks,
                result.total_blocks,
                result.transferred_bytes,
                result.total_bytes
            ),
            (3, 3, 3072, 3072)
        );
        assert_eq!(result.etag.as_deref(), Some("fixture"));
        assert_eq!(std::fs::read(path).unwrap(), drive.remote);
    }
}
