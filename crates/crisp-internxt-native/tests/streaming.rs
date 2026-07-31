use crisp_internxt_native::{crypt, InternxtNativeClient, InternxtSession};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

fn read_request(stream: &mut TcpStream) -> (String, String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end;
    loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = end + 4;
            break;
        }
    }
    let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value)
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < header_end + length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&buffer[..read]);
    }
    let request_line = headers.lines().next().unwrap().to_owned();
    (
        request_line,
        headers,
        bytes[header_end..header_end + length].to_vec(),
    )
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
}

fn respond_bytes(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: {content_type}\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();
}

#[test]
fn upload_path_streams_ciphertext_and_finishes_file_entry() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let captured_put = Arc::new(Mutex::new(Vec::new()));
    let captured_finish = Arc::new(Mutex::new(Value::Null));
    let put_attempts = Arc::new(Mutex::new(0usize));
    let put_copy = Arc::clone(&captured_put);
    let finish_copy = Arc::clone(&captured_finish);
    let attempts_copy = Arc::clone(&put_attempts);
    let server = thread::spawn(move || {
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            let (request, _headers, body) = read_request(&mut stream);
            let path = request.split_whitespace().nth(1).unwrap();
            match path {
                p if p.contains("/files/start") => {
                    respond(
                        &mut stream,
                        "200 OK",
                        &format!(
                            r#"{{"uploads":[{{"uuid":"shard","url":"http://{address}/part"}}]}}"#
                        ),
                    );
                }
                "/part" => {
                    let mut attempts = attempts_copy.lock().unwrap();
                    *attempts += 1;
                    if *attempts == 1 {
                        respond(&mut stream, "500 Internal Server Error", "retry");
                    } else {
                        *put_copy.lock().unwrap() = body;
                        respond(&mut stream, "200 OK", "");
                    }
                }
                p if p.ends_with("/files/finish") => {
                    *finish_copy.lock().unwrap() = serde_json::from_slice(&body).unwrap();
                    respond(&mut stream, "200 OK", r#"{"id":"network-file"}"#);
                }
                "/files" => respond(&mut stream, "200 OK", "{}"),
                other => panic!("unexpected test request: {other}"),
            }
        }
    });

    let path = unique_path("upload");
    let plaintext = b"stream this file without buffering the request body".to_vec();
    std::fs::write(&path, &plaintext).unwrap();
    let bucket = "00".repeat(12);
    let session = InternxtSession {
        drive_api_url: format!("http://{address}"),
        network_url: format!("http://{address}"),
        email: "test@example.invalid".to_owned(),
        token: "token".to_owned(),
        new_token: "new-token".to_owned(),
        mnemonic: "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".to_owned(),
        user_id: "user".to_owned(),
        root_folder_id: "root".to_owned(),
        bridge_user: "bridge".to_owned(),
        bucket_id: bucket.clone(),
    };
    let client = InternxtNativeClient::new(&session.drive_api_url, &session.new_token).unwrap();
    client
        .upload_path(&session, "folder", "streamed", "txt", &path)
        .unwrap();
    server.join().unwrap();

    let finish = captured_finish.lock().unwrap().clone();
    let index = hex::decode(finish["index"].as_str().unwrap()).unwrap();
    let index: [u8; 32] = index.try_into().unwrap();
    let mut decrypted = captured_put.lock().unwrap().clone();
    assert_eq!(index.len(), 32);
    crypt(
        &mut decrypted,
        &session.mnemonic,
        &session.bucket_bytes().unwrap(),
        &index,
    );
    assert_eq!(decrypted, plaintext);
    assert_eq!(*put_attempts.lock().unwrap(), 2);
    assert_eq!(finish["shards"][0]["uuid"], "shard");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn ranged_download_assembles_out_of_order_safe_ciphertext() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let bucket = "00".repeat(12);
    let index = [7u8; 32];
    let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let plaintext = vec![0x5au8; 30 * 1024 * 1024 + 17];
    let mut ciphertext = plaintext.clone();
    crypt(
        &mut ciphertext,
        mnemonic,
        &hex::decode(&bucket).unwrap().try_into().unwrap(),
        &index,
    );
    let ciphertext = Arc::new(ciphertext);
    let ciphertext_server = Arc::clone(&ciphertext);
    let bucket_server = bucket.clone();
    let plaintext_len = plaintext.len();
    let server = thread::spawn(move || {
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            let (request, headers, _) = read_request(&mut stream);
            let path = request.split_whitespace().nth(1).unwrap();
            match path {
                "/files/file/meta" => respond(
                    &mut stream,
                    "200 OK",
                    &format!(
                        r#"{{"bucket":"{}","fileId":"net","size":"{}"}}"#,
                        bucket_server, plaintext_len
                    ),
                ),
                p if p.ends_with("/files/net/info") => respond(
                    &mut stream,
                    "200 OK",
                    &format!(
                        r#"{{"shards":[{{"url":"http://{address}/object"}}],"index":"{}"}}"#,
                        hex::encode(index)
                    ),
                ),
                "/object" => {
                    let range = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(name, _)| name.eq_ignore_ascii_case("range"))
                                .map(|(_, value)| value)
                        })
                        .unwrap()
                        .trim()
                        .strip_prefix("bytes=")
                        .unwrap();
                    let (start, end) = range.split_once('-').unwrap();
                    let start: usize = start.parse().unwrap();
                    let end: usize = end.parse().unwrap();
                    respond_bytes(
                        &mut stream,
                        "206 Partial Content",
                        "application/octet-stream",
                        &ciphertext_server[start..=end],
                    );
                }
                other => panic!("unexpected ranged test request: {other}"),
            }
        }
    });
    let session = InternxtSession {
        drive_api_url: format!("http://{address}"),
        network_url: format!("http://{address}"),
        email: "test@example.invalid".to_owned(),
        token: "token".to_owned(),
        new_token: "new-token".to_owned(),
        mnemonic: mnemonic.to_owned(),
        user_id: "user".to_owned(),
        root_folder_id: "root".to_owned(),
        bridge_user: "bridge".to_owned(),
        bucket_id: bucket,
    };
    let client = InternxtNativeClient::new(&session.drive_api_url, &session.new_token).unwrap();
    let output = unique_path("ranged-download");
    client
        .download_file_to_path_ranged(&session, "file", &output)
        .unwrap();
    server.join().unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), plaintext);
    std::fs::remove_file(output).unwrap();
}

#[test]
fn interrupted_multipart_upload_reuses_checkpoint_and_skips_completed_parts() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let attempts = Arc::new(Mutex::new([0usize; 4]));
    let attempts_server = Arc::clone(&attempts);
    let server = thread::spawn(move || {
        let mut start_calls = 0usize;
        let mut finish_calls = 0usize;
        let mut create_calls = 0usize;
        while create_calls == 0 {
            let (mut stream, _) = listener.accept().unwrap();
            let (request, _headers, _body) = read_request(&mut stream);
            let path = request.split_whitespace().nth(1).unwrap();
            if path.contains("/files/start") {
                start_calls += 1;
                let urls = (1..=4)
                    .map(|part| format!("\"http://{address}/part{part}\""))
                    .collect::<Vec<_>>()
                    .join(",");
                respond(
                    &mut stream,
                    "200 OK",
                    &format!(
                        r#"{{"uploads":[{{"uuid":"shard","UploadId":"upload-id","urls":[{urls}]}}]}}"#
                    ),
                );
            } else if let Some(part) = path.strip_prefix("/part") {
                let part: usize = part.parse().unwrap();
                let mut counts = attempts_server.lock().unwrap();
                counts[part - 1] += 1;
                if part == 4 && counts[part - 1] <= 5 {
                    respond(&mut stream, "500 Internal Server Error", "retry");
                } else {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nETag: etag-{part}\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                }
            } else if path.ends_with("/files/finish") {
                finish_calls += 1;
                respond(&mut stream, "200 OK", r#"{"id":"network-file"}"#);
            } else if path == "/files" {
                create_calls += 1;
                respond(&mut stream, "200 OK", "{}");
            } else {
                panic!("unexpected resume endpoint: {path}");
            }
        }
        assert_eq!(start_calls, 1);
        assert_eq!(finish_calls, 1);
        assert_eq!(create_calls, 1);
    });

    let path = unique_path("resume-upload");
    let state = unique_path("resume-state");
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(100 * 1024 * 1024 + 1)
        .unwrap();
    let base = format!("http://{address}");
    let session = InternxtSession {
        drive_api_url: base.clone(),
        network_url: base.clone(),
        email: "test@example.invalid".into(),
        token: "token".into(),
        new_token: "new-token".into(),
        mnemonic: "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".into(),
        user_id: "user".into(),
        root_folder_id: "root".into(),
        bridge_user: "bridge".into(),
        bucket_id: "00".repeat(12),
    };
    let client = InternxtNativeClient::new(&base, "token").unwrap();
    let first =
        client.upload_path_with_resume_state(&session, "folder", "resume", "bin", &path, &state);
    assert!(first.is_err());
    let checkpoint = client.load_upload_resume_state(&state).unwrap().unwrap();
    assert_eq!(checkpoint.uuid, "shard");
    assert_eq!(checkpoint.upload_id, "upload-id");
    assert_eq!(
        checkpoint
            .etags
            .iter()
            .filter(|etag| etag.is_some())
            .count(),
        3
    );
    client
        .upload_path_with_resume_state(&session, "folder", "resume", "bin", &path, &state)
        .unwrap();
    server.join().unwrap();
    assert_eq!(attempts.lock().unwrap()[0], 1);
    assert_eq!(attempts.lock().unwrap()[1], 1);
    assert_eq!(attempts.lock().unwrap()[2], 1);
    assert_eq!(attempts.lock().unwrap()[3], 6);
    assert!(client.load_upload_resume_state(&state).unwrap().is_none());
    std::fs::remove_file(path).unwrap();
}

fn unique_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "crispsorter-internxt-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
