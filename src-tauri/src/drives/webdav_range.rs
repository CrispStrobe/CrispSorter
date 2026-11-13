//! Validate the single byte interval before returning bytes to range consumers.
use anyhow::{anyhow, ensure, Context, Result};
use reqwest::{blocking::Response, header, StatusCode};
use std::io::Read;

// Disable transforms even if another dependency enables reqwest decoders.
// Range offsets and lengths describe wire representation bytes, not decoded data.
pub(super) fn identity_client(
    builder: reqwest::blocking::ClientBuilder,
) -> reqwest::blocking::ClientBuilder {
    builder.no_gzip().no_brotli().no_zstd().no_deflate()
}

pub(super) fn validate_request(start: u64, end: u64) -> Result<()> {
    ensure!(
        end.checked_sub(start)
            .and_then(|n| n.checked_add(1))
            .and_then(|n| n.checked_add(1))
            .is_some(),
        "invalid or overflowing WebDAV byte range"
    );
    Ok(())
}

pub(super) fn read_response(response: Response, start: u64, end: u64) -> Result<Vec<u8>> {
    validate_request(start, end)?;
    ensure!(
        response.status() == StatusCode::PARTIAL_CONTENT,
        "WebDAV range GET returned {}; server did not honor Range",
        response.status()
    );
    for encoding in response.headers().get_all(header::CONTENT_ENCODING) {
        ensure!(
            encoding
                .to_str()
                .map(|s| s.eq_ignore_ascii_case("identity"))
                .unwrap_or(false),
            "encoded WebDAV range response is unsupported"
        );
    }
    let mut values = response.headers().get_all(header::CONTENT_RANGE).iter();
    let value = values
        .next()
        .ok_or_else(|| anyhow!("WebDAV range response is missing Content-Range"))?;
    ensure!(values.next().is_none(), "ambiguous WebDAV Content-Range");
    let value = value.to_str().context("invalid WebDAV Content-Range")?;
    let (unit, range) = value
        .split_once(' ')
        .ok_or_else(|| anyhow!("invalid WebDAV Content-Range"))?;
    ensure!(
        unit.eq_ignore_ascii_case("bytes"),
        "unsupported WebDAV range unit"
    );
    let (span, total) = range
        .split_once('/')
        .ok_or_else(|| anyhow!("invalid WebDAV Content-Range"))?;
    let (first, last) = span
        .split_once('-')
        .ok_or_else(|| anyhow!("invalid WebDAV Content-Range"))?;
    let number = |text: &str| -> Result<u64> {
        ensure!(
            !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()),
            "invalid WebDAV range number"
        );
        text.parse().context("WebDAV range number overflow")
    };
    let first = number(first)?;
    let last = number(last)?;
    let total = if total == "*" {
        None
    } else {
        Some(number(total)?)
    };
    ensure!(
        first == start && last >= first && last <= end,
        "WebDAV Content-Range does not match requested interval"
    );
    if let Some(total) = total {
        ensure!(last < total, "invalid WebDAV Content-Range total");
    }
    // Callers cannot discover a silently omitted suffix from Vec<u8>. Only a
    // known EOF justifies clipping the requested end; unknown totals must cover
    // the complete requested interval. RFC 9110 sections 14.4 and 15.3.7.
    ensure!(
        last == end || total == last.checked_add(1),
        "incomplete WebDAV range response before EOF"
    );
    let length = last
        .checked_sub(first)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| anyhow!("WebDAV range length overflow"))?;
    let limit = length
        .checked_add(1)
        .ok_or_else(|| anyhow!("WebDAV range is too large"))?;
    let mut bytes = Vec::new();
    // Do not buffer an unbounded error body from a server falsely claiming 206.
    response
        .take(limit)
        .read_to_end(&mut bytes)
        .context("reading WebDAV range body")?;
    ensure!(
        u64::try_from(bytes.len()).ok() == Some(length),
        "WebDAV range response size mismatch"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn fetch(content_range: Option<&str>, body: &str, start: u64, end: u64) -> Result<Vec<u8>> {
        let mut server = Server::new();
        let mut mock = server
            .mock("GET", "/file")
            .match_header("range", format!("bytes={start}-{end}").as_str())
            .match_header("accept-encoding", "identity")
            .with_status(206)
            .with_body(body);
        if let Some(value) = content_range {
            mock = mock.with_header("content-range", value);
        }
        let mock = mock.create();
        let response = identity_client(reqwest::blocking::Client::builder())
            .build()
            .unwrap()
            .get(format!("{}/file", server.url()))
            .header(header::RANGE, format!("bytes={start}-{end}"))
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .unwrap();
        let result = read_response(response, start, end);
        mock.assert();
        result
    }

    #[test]
    fn exact_ranges_and_known_eof_clipping_return_only_requested_bytes() {
        assert_eq!(fetch(Some("bytes 4-7/12"), "part", 4, 7).unwrap(), b"part");
        assert_eq!(fetch(Some("bytes 4-7/*"), "part", 4, 7).unwrap(), b"part");
        assert_eq!(fetch(Some("bytes 4-5/6"), "rt", 4, 99).unwrap(), b"rt");
    }

    #[test]
    fn wrong_missing_ambiguous_or_truncated_intervals_never_return_bytes() {
        for value in [
            None,
            Some("bytes 0-3/12"),
            Some("bytes 4-8/12"),
            Some("bytes 4-6/12"),
            Some("bytes 4-6/*"),
            Some("bytes 4-7/7"),
            Some("bytes 7-4/12"),
            Some("bytes */12"),
            Some("bytes 4-7/12, bytes 4-7/12"),
            Some("items 4-7/12"),
            Some("bytes +4-7/12"),
            Some("bytes 4-7/18446744073709551616"),
        ] {
            assert!(fetch(value, "part", 4, 7).is_err(), "accepted {value:?}");
        }
        for body in ["par", "part too long"] {
            assert!(fetch(Some("bytes 4-7/12"), body, 4, 7).is_err());
        }
        assert!(fetch(Some("bytes 0-18446744073709551615/*"), "", 0, u64::MAX).is_err());
    }

    #[test]
    fn ignored_range_and_duplicate_content_range_are_rejected() {
        let mut server = Server::new();
        let ignored = server
            .mock("GET", "/ignored")
            .with_status(200)
            .with_body("whole file")
            .create();
        let duplicate = server
            .mock("GET", "/duplicate")
            .with_status(206)
            .with_header("content-range", "bytes 4-7/12")
            .with_header("content-range", "bytes 4-7/12")
            .with_body("part")
            .create();
        let client = identity_client(reqwest::blocking::Client::builder())
            .build()
            .unwrap();
        for path in ["ignored", "duplicate"] {
            assert!(read_response(
                client
                    .get(format!("{}/{path}", server.url()))
                    .send()
                    .unwrap(),
                4,
                7
            )
            .is_err());
        }
        ignored.assert();
        duplicate.assert();
    }

    #[test]
    fn oversized_body_consumption_is_bounded_by_advertised_interval() {
        // The bounded Read policy is exercised with an actual HTTP response,
        // including absence of a trustworthy length in the range metadata.
        assert!(fetch(Some("bytes 4-7/*"), &"x".repeat(128 * 1024), 4, 7).is_err());
    }
    #[test]
    fn encoding_unsatisfied_ranges_and_request_overflow_remain_errors() {
        let mut server = Server::new();
        let encoded = server
            .mock("GET", "/encoded")
            .with_status(206)
            .with_header("content-range", "bytes 4-7/12")
            .with_header("content-encoding", "gzip")
            .with_body("part")
            .create();
        let unsatisfied = server
            .mock("GET", "/unsatisfied")
            .with_status(416)
            .with_header("content-range", "bytes */4")
            .create();
        let client = identity_client(reqwest::blocking::Client::builder())
            .build()
            .unwrap();
        for path in ["encoded", "unsatisfied"] {
            assert!(read_response(
                client
                    .get(format!("{}/{path}", server.url()))
                    .send()
                    .unwrap(),
                4,
                7
            )
            .is_err());
        }
        encoded.assert();
        unsatisfied.assert();
        assert!(validate_request(0, u64::MAX).is_err());
        assert!(validate_request(1, u64::MAX).is_err());
        assert!(validate_request(9, 8).is_err());
        assert!(validate_request(u64::MAX, u64::MAX).is_ok());
        assert_eq!(
            fetch(
                Some("bytes 18446744073709551615-18446744073709551615/*"),
                "x",
                u64::MAX,
                u64::MAX
            )
            .unwrap(),
            b"x"
        );
    }
    #[test]
    fn oversized_body_is_rejected_without_waiting_for_the_rest_of_the_response() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            sync::mpsc,
            thread,
            time::Duration,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (close, wait_close) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-7/*\r\nContent-Length: 100000000\r\nConnection: close\r\n\r\nxxxxx").unwrap();
            // Only five bytes of a claimed 100 MB response arrive. The client
            // must reject after span+1 without waiting for this connection to close.
            let _ = wait_close.recv_timeout(Duration::from_secs(10));
        });
        let (finished, result) = mpsc::channel();
        let client = thread::spawn(move || {
            let client = identity_client(reqwest::blocking::Client::builder())
                .timeout(Duration::from_secs(8))
                .build()
                .unwrap();
            let response = client.get(format!("http://{address}/file")).send().unwrap();
            finished.send(read_response(response, 4, 7)).unwrap();
        });
        let early = result.recv_timeout(Duration::from_secs(5));
        let _ = close.send(());
        server.join().unwrap();
        client.join().unwrap();
        assert!(early
            .expect("range read waited for the oversized body")
            .is_err());
    }
}
