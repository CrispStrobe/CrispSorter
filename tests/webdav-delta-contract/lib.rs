#![allow(dead_code)]
#[path = "../../src-tauri/src/sync/delta.rs"]
pub mod production_delta;
pub mod sync {
    pub use crate::production_delta as delta;
}
#[path = "../../src-tauri/src/drives/webdav_delta_download.rs"]
mod webdav_delta_download;
#[path = "../../src-tauri/src/drives/webdav_delta_upload.rs"]
mod webdav_delta_upload;

#[cfg(test)]
mod actual_wiring {
    use super::{
        production_delta::Blockmap, webdav_delta_download as delta_download,
        webdav_delta_upload as delta_upload,
    };
    use anyhow::{anyhow, Context, Result};
    use std::cell::RefCell;
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom};
    mod reqwest {
        pub mod blocking {
            pub struct Body {
                pub reader: Box<dyn std::io::Read>,
                pub size: u64,
                pub streamed: bool,
            }
            impl From<Vec<u8>> for Body {
                fn from(bytes: Vec<u8>) -> Self {
                    let size = bytes.len() as u64;
                    Self {
                        reader: Box::new(std::io::Cursor::new(bytes)),
                        size,
                        streamed: false,
                    }
                }
            }
            impl Body {
                pub fn sized(reader: impl std::io::Read + 'static, size: u64) -> Self {
                    Self {
                        reader: Box::new(reader),
                        size,
                        streamed: true,
                    }
                }
            }
        }
        pub enum Method {
            POST,
        }
    }
    struct Request<'a> {
        calls: &'a RefCell<Vec<String>>,
        url: String,
        body: Option<reqwest::blocking::Body>,
        status: u16,
    }
    impl Request<'_> {
        fn header(self, _: &str, _: impl Into<String>) -> Self {
            self
        }
        fn body(mut self, body: reqwest::blocking::Body) -> Self {
            self.body = Some(body);
            self
        }
        fn send(mut self) -> Result<Response> {
            if let Some(body) = &mut self.body {
                assert_eq!(
                    body.streamed,
                    body.size > delta_upload::REPLAYABLE_BODY_LIMIT as u64
                );
                // Exercise the owned stream through bounded reads, including
                // its exact sized-body contract, without a HTTP dependency.
                let mut buf = [0u8; 8192];
                let mut count = 0;
                loop {
                    let n = body.reader.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    assert!(buf[..n].iter().all(|byte| *byte == b'b'));
                    count += n as u64;
                }
                assert_eq!(count, body.size);
            }
            self.calls.borrow_mut().push(self.url);
            Ok(Response(self.status))
        }
    }
    struct Response(u16);
    struct Status(u16);
    impl Status {
        fn is_success(&self) -> bool {
            (200..300).contains(&self.0)
        }
    }
    impl std::fmt::Display for Status {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }
    impl Response {
        fn status(&self) -> Status {
            Status(self.0)
        }
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
                body: None,
                status: if url.starts_with("blocks") {
                    self.fail_at.unwrap_or(200) as u16
                } else {
                    200
                },
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
    fn actual_delta_upload_streams_small_file_with_maximum_advertised_block() {
        let file = tempfile::NamedTempFile::new().unwrap();
        const SIZE: usize = super::webdav_delta_upload::REPLAYABLE_BODY_LIMIT + 123;
        std::fs::write(file.path(), vec![b'b'; SIZE]).unwrap();
        let remote = super::production_delta::compute_blockmap_from_bytes(
            &vec![b'a'; SIZE],
            u32::MAX as usize,
        )
        .unwrap();
        let drive = WebDavDrive {
            remote: vec![],
            fail_at: None,
            map_override: Some(remote),
            calls: RefCell::new(Vec::new()),
        };
        let result = drive
            .delta_upload_file(file.path(), Path::new("remote"))
            .unwrap()
            .unwrap();
        assert_eq!(result.transferred_bytes, SIZE as u64);
        assert_eq!(result.total_blocks, 1);
        assert_eq!(
            *drive.calls.borrow(),
            [
                format!("blocks?offset=0&size={SIZE}"),
                format!("finalize?size={SIZE}")
            ]
        );
    }

    #[test]
    fn unhandled_redirects_fail_before_finalize() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let data = vec![b'b'; super::webdav_delta_upload::REPLAYABLE_BODY_LIMIT + 1];
        std::fs::write(file.path(), &data).unwrap();
        for status in [307, 308] {
            let drive = WebDavDrive {
                remote: vec![],
                fail_at: Some(status),
                map_override: Some(
                    super::production_delta::compute_blockmap_from_bytes(
                        &vec![b'a'; data.len()],
                        u32::MAX as usize,
                    )
                    .unwrap(),
                ),
                calls: RefCell::new(Vec::new()),
            };
            assert!(drive
                .delta_upload_file(file.path(), Path::new("remote"))
                .is_err());
            assert_eq!(drive.calls.borrow().len(), 1);
            assert!(drive.calls.borrow()[0].starts_with("blocks?"));
        }
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
