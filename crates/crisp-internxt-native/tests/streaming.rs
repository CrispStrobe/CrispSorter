use crisp_internxt_native::{crypt, InternxtNativeClient, InternxtSession};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

fn read_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
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
            let (request, body) = read_request(&mut stream);
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
