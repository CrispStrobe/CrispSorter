use crate::{
    production_proxy::{
        build_blocking_client_with_options, configure_blocking_builder, ProxyConfig,
    },
    webdav_range_response as range_response,
};
use anyhow::{Context, Result};
use std::{path::Path, time::Duration};
include!(concat!(env!("OUT_DIR"), "/webdav_range_wiring.rs"));

#[test]
fn actual_webdav_range_preserves_auth_and_ordinary_client_decompression() {
    let mut server = mockito::Server::new();
    // A valid gzip member for "part". With gzip features unified, an ordinary
    // reqwest client decodes this and removes Content-Encoding/Content-Length.
    let gzip = vec![
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 43, 72, 44, 42, 1, 0, 198, 112, 15, 73, 4, 0, 0, 0,
    ];
    let normal = server
        .mock("GET", "/dav/ordinary")
        .with_status(200)
        .with_header("content-encoding", "gzip")
        .with_body(gzip.clone())
        .create();
    let encoded = server
        .mock("GET", "/dav/encoded")
        .match_header("range", "bytes=4-7")
        .match_header("accept-encoding", "identity")
        .match_header("authorization", "Basic dXNlcjpwYXNz")
        .with_status(206)
        .with_header("content-range", "bytes 4-7/12")
        .with_header("content-encoding", "gzip")
        .with_body(gzip)
        .create();
    let range = server
        .mock("GET", "/dav/file%20name")
        .match_header("range", "bytes=4-7")
        .match_header("accept-encoding", "identity")
        .match_header("authorization", "Basic dXNlcjpwYXNz")
        .with_status(206)
        .with_header("content-range", "bytes 4-7/12")
        .with_body("part")
        .create();
    let drive = WebDavDrive::new_with_proxy(
        "test",
        format!("{}/dav/", server.url()),
        Some("user".into()),
        Some("pass".into()),
        false,
        &ProxyConfig::default(),
    )
    .unwrap();
    assert_eq!(
        drive
            .req(
                reqwest::Method::GET,
                &format!("{}/dav/ordinary", server.url())
            )
            .send()
            .unwrap()
            .bytes()
            .unwrap()
            .as_ref(),
        b"part"
    );
    assert!(drive
        .read_range(Path::new("encoded"), 4, 7)
        .unwrap_err()
        .to_string()
        .contains("encoded"));
    assert_eq!(
        drive.read_range(Path::new("file name"), 4, 7).unwrap(),
        b"part"
    );
    // Invalid/overflowing intervals fail before any request is constructed.
    assert!(drive
        .read_range(Path::new("never-requested"), 0, u64::MAX)
        .is_err());
    normal.assert();
    encoded.assert();
    range.assert();
}
