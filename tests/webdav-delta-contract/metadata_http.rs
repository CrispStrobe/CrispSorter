//! Standalone real-reqwest checks. See README for the cache-only rustc command.
#[allow(dead_code)]
#[path = "../../src-tauri/src/drives/webdav_delta_metadata.rs"]
mod delta_metadata;

use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::Duration,
};

fn try_response(wire: Vec<u8>) -> reqwest::Result<reqwest::blocking::Response> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if stream.read(&mut byte).unwrap() == 0 {
                return;
            }
            request.push(byte[0]);
        }
        // A bounded reader may deliberately close before the server finishes.
        let _ = stream.write_all(&wire);
    });
    reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .get(format!("http://{address}/metadata"))
        .send()
}
fn response(wire: Vec<u8>) -> reqwest::blocking::Response {
    try_response(wire).unwrap()
}
fn wire(headers: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = format!("HTTP/1.1 200 OK\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

#[test]
fn valid_fixed_chunked_and_close_delimited_responses_are_accepted() {
    for wire in [
        wire("Content-Length: 2\r\n", b"{}"),
        wire(
            "Transfer-Encoding: chunked\r\n",
            b"1\r\n{\r\n1\r\n}\r\n0\r\n\r\n",
        ),
        wire("", b"{}"),
    ] {
        assert_eq!(
            delta_metadata::read_bounded(response(wire), 2).unwrap(),
            b"{}"
        );
    }
}

#[test]
fn oversized_fixed_chunked_and_close_delimited_bodies_are_refused() {
    let bytes = vec![b' '; delta_metadata::STATUS_LIMIT + 1];
    let mut chunked = format!("{:x}\r\n", bytes.len()).into_bytes();
    chunked.extend_from_slice(&bytes);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    for wire in [
        wire(&format!("Content-Length: {}\r\n", bytes.len()), &bytes),
        wire("Transfer-Encoding: chunked\r\n", &chunked),
        wire("", &bytes),
    ] {
        let error =
            delta_metadata::read_bounded(response(wire), delta_metadata::STATUS_LIMIT).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("decoded response limit; use a full transfer"),
            "{error}"
        );
    }
}

#[test]
fn inflated_content_length_does_not_allocate_from_header_and_truncation_errors() {
    // HTTP framing defines the body. A smaller declared length would hide
    // trailing wire bytes from the response reader; no detection is claimed.
    let response = response(wire("Content-Length: 1099511627776\r\n", b"{}"));
    let error = delta_metadata::read_bounded(response, 64).unwrap_err();
    assert!(!error.to_string().contains("decoded response limit"));
    // Some invalid lengths fail in hyper's header parser before any body read.
    // Either parser rejection or bounded/truncated body failure is safe.
    if let Ok(response) = try_response(wire("Content-Length: 18446744073709551615\r\n", b"{}")) {
        assert!(delta_metadata::read_bounded(response, 64).is_err());
    }
}

#[test]
fn decoded_gzip_body_is_bounded_despite_small_encoded_content_length() {
    // gzip.compress(b' ' * 65537, mtime=0); encoded size is under 100 bytes.
    let gzip: &[u8] = &[
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 237, 193, 129, 0, 0, 0, 0, 195, 32, 149, 249, 83, 254,
        32, 85, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 192, 13, 1, 180, 186, 30, 1, 0, 1, 0,
    ];
    let response = response(wire(
        &format!(
            "Content-Encoding: gzip\r\nContent-Length: {}\r\n",
            gzip.len()
        ),
        gzip,
    ));
    let error = delta_metadata::read_bounded(response, delta_metadata::STATUS_LIMIT).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("65536-byte decoded response limit"),
        "{error}"
    );
}
