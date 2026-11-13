//! Actual status/map methods and types, with a deliberately minimal transport.
//! No Response::json/content_length API exists here: reads must be bounded.
use super::delta_metadata;
use anyhow::{anyhow, Context, Result};
use std::{
    cell::Cell,
    io::{self, Cursor, Read},
    path::Path,
    rc::Rc,
};

mod reqwest {
    pub enum Method {
        GET,
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct StatusCode(pub u16);
    impl StatusCode {
        pub const NOT_FOUND: Self = Self(404);
        pub fn is_success(self) -> bool {
            (200..300).contains(&self.0)
        }
    }
}

#[derive(Clone)]
struct Response {
    code: u16,
    body: Cursor<Vec<u8>>,
    read_count: Rc<Cell<usize>>,
}
impl Response {
    fn new(code: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            code,
            body: Cursor::new(body.into()),
            read_count: Rc::new(Cell::new(0)),
        }
    }
    fn status(&self) -> reqwest::StatusCode {
        reqwest::StatusCode(self.code)
    }
    fn send(self) -> Result<Self> {
        Ok(self)
    }
    fn error_for_status(self) -> Result<Self> {
        if (400..600).contains(&self.code) {
            Err(anyhow!("HTTP {}", self.code))
        } else {
            Ok(self)
        }
    }
}
impl Read for Response {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_count.set(self.read_count.get() + 1);
        self.body.read(buf)
    }
}
struct WebDavDrive {
    status: Response,
    map: Response,
}
impl WebDavDrive {
    fn delta_app_base(&self) -> Option<String> {
        Some("fixture".into())
    }
    fn delta_path_url(&self, _: &str, _: &Path) -> Result<String> {
        Ok("map".into())
    }
    fn req(&self, _: reqwest::Method, url: &str) -> Response {
        if url.ends_with("status") {
            self.status.clone()
        } else {
            self.map.clone()
        }
    }
}
include!(concat!(env!("OUT_DIR"), "/metadata_wiring.rs"));

const STATUS: &str =
    r#"{"app":"crispcloud_delta","blockSize":4194304,"algorithm":"adler32+sha256"}"#;
const MAP: &str = r#"{"filePath":"/file","totalSize":4,"blockSize":4194304,"blockCount":1,"signatures":[{"blockIndex":0,"offset":0,"size":4,"weakHash":67371529,"strongHash":"abcdef"}],"etag":"etag-1"}"#;
fn drive(status: Response, map: Response) -> WebDavDrive {
    WebDavDrive { status, map }
}

#[test]
fn valid_status_and_map_keep_fields_and_optional_app_detection() {
    let drive = drive(Response::new(200, STATUS), Response::new(200, MAP));
    let map = drive.fetch_delta_map(Path::new("file")).unwrap().unwrap();
    assert_eq!(
        (
            map.file_path.as_str(),
            map.total_size,
            map.block_size,
            map.block_count
        ),
        ("/file", 4, 4194304, 1)
    );
    let signature = &map.signatures[0];
    assert_eq!(
        (
            signature.block_index,
            signature.offset,
            signature.size,
            signature.weak_hash
        ),
        (0, 0, 4, 67371529)
    );
    assert_eq!(signature.strong_hash, "abcdef");
    assert_eq!(map.etag.as_deref(), Some("etag-1"));
    for status in [
        r#"{}"#,
        r#"{"app":"other","blockSize":1,"algorithm":"adler32+sha256"}"#,
    ] {
        let drive = self::drive(Response::new(200, status), Response::new(200, MAP));
        assert!(drive.fetch_delta_map(Path::new("file")).unwrap().is_none());
        assert_eq!(drive.map.read_count.get(), 0);
    }
}

#[test]
fn large_valid_map_can_describe_over_390_gib_without_a_file_size_cap() {
    use std::fmt::Write;
    const BLOCKS: u64 = 100_000;
    const BLOCK_SIZE: u64 = 4 * 1024 * 1024;
    let mut map = format!(
        r#"{{"filePath":"/large","totalSize":{},"blockSize":{BLOCK_SIZE},"blockCount":{BLOCKS},"signatures":["#,
        BLOCKS * BLOCK_SIZE
    );
    for index in 0..BLOCKS {
        if index != 0 {
            map.push(',');
        }
        write!(map, r#"{{"blockIndex":{index},"offset":{},"size":{BLOCK_SIZE},"weakHash":1,"strongHash":"0000000000000000000000000000000000000000000000000000000000000000"}}"#, index * BLOCK_SIZE).unwrap();
    }
    map.push_str("]}");
    assert!(map.len() < delta_metadata::BLOCK_MAP_LIMIT);
    let drive = drive(Response::new(200, STATUS), Response::new(200, map));
    let map = drive.fetch_delta_map(Path::new("large")).unwrap().unwrap();
    assert_eq!(map.total_size, BLOCKS * BLOCK_SIZE);
    assert_eq!(map.signatures.len(), BLOCKS as usize);
}

#[test]
fn ordinary_http_and_json_error_branches_are_preserved() {
    for code in [301, 404, 500] {
        let drive = drive(Response::new(code, "not json"), Response::new(200, MAP));
        assert!(!drive.delta_sync_available().unwrap());
        assert_eq!(drive.status.read_count.get(), 0);
    }
    let absent = drive(Response::new(200, STATUS), Response::new(404, "not json"));
    assert!(absent.fetch_delta_map(Path::new("file")).unwrap().is_none());
    assert_eq!(absent.map.read_count.get(), 0);
    let denied = drive(Response::new(200, STATUS), Response::new(403, "not json"));
    assert_eq!(
        denied
            .fetch_delta_map(Path::new("file"))
            .unwrap_err()
            .to_string(),
        "HTTP 403"
    );
    assert_eq!(denied.map.read_count.get(), 0);
    let invalid = drive(Response::new(200, "not json"), Response::new(200, MAP));
    assert_eq!(
        invalid.delta_sync_available().unwrap_err().to_string(),
        "parsing crispcloud_delta status"
    );
    let invalid = drive(Response::new(200, STATUS), Response::new(200, "not json"));
    assert_eq!(
        invalid
            .fetch_delta_map(Path::new("file"))
            .unwrap_err()
            .to_string(),
        "parsing crispcloud_delta block map"
    );
    for map in [
        MAP.replace("\"blockCount\":1", "\"blockCount\":2"),
        MAP.replace("\"blockSize\":4194304", "\"blockSize\":0"),
    ] {
        let invalid = drive(Response::new(200, STATUS), Response::new(200, map));
        assert!(invalid
            .fetch_delta_map(Path::new("file"))
            .unwrap_err()
            .to_string()
            .contains("invalid crispcloud_delta block map"));
    }
}

#[test]
fn oversize_refusal_is_visible_in_the_string_rendered_by_tauri_and_retains_source() {
    let oversized = drive(
        Response::new(200, vec![b' '; delta_metadata::STATUS_LIMIT + 1]),
        Response::new(200, MAP),
    );
    let error = oversized.delta_sync_available().unwrap_err();
    assert!(error
        .to_string()
        .contains("reading crispcloud_delta status"));
    assert!(error
        .to_string()
        .contains("65536-byte decoded response limit; use a full transfer"));
    assert!(error
        .chain()
        .any(|cause| cause.downcast_ref::<io::Error>().is_some()));
    assert_eq!(oversized.map.read_count.get(), 0);
    let oversized = drive(
        Response::new(200, STATUS),
        Response::new(200, vec![b' '; delta_metadata::BLOCK_MAP_LIMIT + 1]),
    );
    let error = oversized.fetch_delta_map(Path::new("file")).unwrap_err();
    assert!(error
        .to_string()
        .contains("reading crispcloud_delta block map"));
    assert!(error
        .to_string()
        .contains("33554432-byte decoded response limit; use a full transfer"));
}
