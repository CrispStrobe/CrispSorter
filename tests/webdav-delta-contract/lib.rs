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
    use anyhow::{anyhow, Context, Result};
    use std::cell::RefCell;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    mod reqwest {
        pub enum Method {
            POST,
        }
    }
    struct Request<'a> {
        calls: &'a RefCell<Vec<String>>,
        url: String,
    }
    impl Request<'_> {
        fn header(self, _: &str, _: impl Into<String>) -> Self {
            self
        }
        fn body(self, _: Vec<u8>) -> Self {
            self
        }
        fn send(self) -> Result<Response> {
            self.calls.borrow_mut().push(self.url);
            Ok(Response)
        }
    }
    struct Response;
    impl Response {
        fn error_for_status(self) -> Result<Self> {
            Ok(self)
        }
    }
    use std::path::Path;
    #[derive(Clone)]
    struct ServerMap {
        blockmap: Blockmap,
        etag: Option<String>,
    }
    struct WebDavDrive {
        remote: Vec<u8>,
        fail_at: Option<u64>,
        map_override: Option<Blockmap>,
        calls: RefCell<Vec<String>>,
    }
    struct DeltaTransferResult {
        changed_blocks: usize,
        total_blocks: usize,
        transferred_bytes: u64,
        total_bytes: u64,
        etag: Option<String>,
    }
    impl WebDavDrive {
        fn delta_path_url(&self, route: &str, _: &Path) -> Result<String> {
            Ok(route.into())
        }
        fn req(&self, _: reqwest::Method, url: &str) -> Request<'_> {
            Request {
                calls: &self.calls,
                url: url.into(),
            }
        }
        fn fetch_delta_map(&self, _: &Path) -> Result<Option<ServerMap>> {
            Ok(Some(ServerMap {
                blockmap: match &self.map_override {
                    Some(map) => map.clone(),
                    None => {
                        super::production_delta::compute_blockmap_from_bytes(&self.remote, 1024)?
                    }
                },
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
    fn actual_delta_upload_rejects_bad_layout_before_local_io_or_remote_writes() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("source");
        let data = vec![b'a'; 2048];
        let mut invalid =
            super::production_delta::compute_blockmap_from_bytes(&data, 1024).unwrap();
        invalid.blocks[1].offset = 0;
        let mut drive = WebDavDrive {
            remote: data.clone(),
            fail_at: None,
            map_override: Some(invalid),
            calls: RefCell::new(Vec::new()),
        };
        // The missing source demonstrates rejection before opening/scanning it.
        let error = drive
            .delta_upload_file(&local, Path::new("remote"))
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("invalid delta block offset"),
            "{error:#}"
        );
        assert!(drive.calls.borrow().is_empty());
        // With a real identical source the old implementation finalized the
        // malformed map after skipping all block uploads on matching hashes.
        std::fs::write(&local, &data).unwrap();
        assert!(drive
            .delta_upload_file(&local, Path::new("remote"))
            .is_err());
        assert!(drive.calls.borrow().is_empty());
        // Exercise the fake transport with a valid changed map too: the
        // extracted production method must send blocks and then finalize.
        drive.map_override = None;
        std::fs::write(&local, vec![b'b'; 2048]).unwrap();
        let result = drive
            .delta_upload_file(&local, Path::new("remote"))
            .unwrap()
            .unwrap();
        assert_eq!(result.changed_blocks, 2);
        assert_eq!(
            *drive.calls.borrow(),
            [
                "blocks?offset=0&size=1024",
                "blocks?offset=1024&size=1024",
                "finalize?size=2048"
            ]
        );
    }

    #[test]
    fn actual_delta_download_stages_before_mutation_and_reports_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        let original = vec![b'a'; 3072];
        std::fs::write(&path, &original).unwrap();
        let mut drive = WebDavDrive {
            remote: vec![b'b'; 3072],
            fail_at: Some(1024),
            map_override: None,
            calls: RefCell::new(Vec::new()),
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
