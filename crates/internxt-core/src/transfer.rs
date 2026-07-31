//! Streaming network (bridge) transfer primitives — fully streaming, never holds
//! a whole file in RAM. These are the reusable core behind `upload-file`,
//! `upload-folder`, `download-file`, the WebDAV/FUSE backends and `sync`.
//!
//! Byte progress is reported through an optional [`ProgressSink`]; when `None`,
//! progress is silently discarded. No terminal output lives here.

use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use rand::RngExt;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::crypto::{self, Ctr};
use crate::network::NetworkApi;
use crate::progress::{noop_sink, ProgressSink};

// fs-only deps (the path-based upload / folder-create helpers).
#[cfg(feature = "fs")]
use crate::api::DriveApi;
use crate::network::PartRef;
#[cfg(feature = "fs")]
use std::path::Path;

const READ_CHUNK: usize = 1024 * 1024; // 1MB stream granularity
// Multipart applies to any source (file or live stream) above this size —
// only the single-PUT path has a hard object-size ceiling on the storage
// side, so anything bigger must be sliced regardless of where it comes from.
const MULTIPART_THRESHOLD: u64 = 100 * 1024 * 1024; // 100MB
const PART_SIZE: usize = 15 * 1024 * 1024; // 15MB
const UPLOAD_CONCURRENCY: usize = 10;
#[cfg(feature = "fs")]
const FOLDER_CREATE_RETRIES: usize = 2;
#[cfg(feature = "fs")]
const RETRY_DELAYS_MS: [u64; 2] = [500, 1000];

/// Encrypt + upload a file's bytes to the network, returning the network file id.
/// Picks single-part or multipart based on size. Shared by upload-file / upload-folder.
/// `pb` = an optional progress sink to report byte progress into (e.g. a shared
/// bar for folder uploads). When `None`, progress is discarded.
///
/// Requires the `fs` feature (reads from a filesystem path). For a non-fs source
/// use [`upload_stream_to_network`].
#[cfg(feature = "fs")]
pub async fn upload_file_to_network(
    net: &NetworkApi,
    bucket: &str,
    mnemonic: &str,
    path: &Path,
    size: u64,
    pb: Option<Arc<dyn ProgressSink>>,
) -> Result<String> {
    let mut index = [0u8; 32];
    rand::rng().fill(&mut index);
    let iv = index[0..16].to_vec();
    let key = crypto::generate_file_key(mnemonic, bucket, &index)?;
    let pb = pb.unwrap_or_else(noop_sink);

    if size > MULTIPART_THRESHOLD {
        let file = tokio::fs::File::open(path).await?;
        upload_multipart(net, bucket, size, file, &key, &iv, &index, &pb).await
    } else {
        let file = tokio::fs::File::open(path).await?;
        upload_single(net, bucket, size, file, &key, &iv, &index, &pb).await
    }
}

/// Encrypt + upload `size` bytes from an arbitrary reader (e.g. stdin, or a live
/// WebDAV/FUSE/SMB write body), returning the network file id. Picks single-part
/// or multipart based on size, same threshold as [`upload_file_to_network`] — the
/// reader only needs to be read sequentially once, so it doesn't need to be
/// seekable: multipart here reads forward and buffers each 15MB part before
/// dispatching it, rather than re-slicing an already-buffered file.
pub async fn upload_stream_to_network<R>(
    net: &NetworkApi,
    bucket: &str,
    mnemonic: &str,
    reader: R,
    size: u64,
    pb: Option<Arc<dyn ProgressSink>>,
) -> Result<String>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut index = [0u8; 32];
    rand::rng().fill(&mut index);
    let iv = index[0..16].to_vec();
    let key = crypto::generate_file_key(mnemonic, bucket, &index)?;
    let pb = pb.unwrap_or_else(noop_sink);
    if size > MULTIPART_THRESHOLD {
        upload_multipart(net, bucket, size, reader, &key, &iv, &index, &pb).await
    } else {
        upload_single(net, bucket, size, reader, &key, &iv, &index, &pb).await
    }
}

/// Single presigned-URL upload, body streamed straight from a reader through CTR.
async fn upload_single<R>(
    net: &NetworkApi,
    bucket: &str,
    size: u64,
    reader: R,
    key: &[u8; 32],
    iv: &[u8],
    index: &[u8],
    pb: &Arc<dyn ProgressSink>,
) -> Result<String>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let start = net.start_upload(bucket, size, 1).await?;
    let slot = start
        .uploads
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no upload slot returned"))?;
    let url = slot.url.ok_or_else(|| anyhow!("no upload url returned"))?;

    let hasher = Arc::new(Mutex::new(Sha256::new()));

    // Streaming state moved into the body producer.
    struct St<R> {
        reader: R,
        ctr: Ctr,
        hasher: Arc<Mutex<Sha256>>,
        pb: Arc<dyn ProgressSink>,
    }
    let st = St {
        reader,
        ctr: Ctr::new(key, iv),
        hasher: hasher.clone(),
        pb: pb.clone(),
    };

    let body = futures_util::stream::unfold(st, |mut st| async move {
        let mut buf = vec![0u8; READ_CHUNK];
        match st.reader.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                st.ctr.apply(&mut buf);
                st.hasher.lock().unwrap().update(&buf);
                st.pb.inc(n as u64);
                Some((Ok::<Bytes, std::io::Error>(Bytes::from(buf)), st))
            }
            Err(e) => Some((Err(e), st)),
        }
    });

    net.put_stream(&url, size, body).await?;

    let digest = hasher.lock().unwrap().clone().finalize();
    let hash = hex::encode(crypto::ripemd160(&digest));

    let finish = net
        .finish_upload(bucket, &hex::encode(index), &hash, &slot.uuid)
        .await?;
    Ok(finish.id)
}

/// Multipart upload: continuous CTR stream sliced into 15MB parts, PUT concurrently.
/// Generic over any sequential reader — it reads forward once and buffers each
/// part before dispatching, so it doesn't need the source to be seekable.
#[allow(clippy::too_many_arguments)]
async fn upload_multipart<R>(
    net: &NetworkApi,
    bucket: &str,
    size: u64,
    mut reader: R,
    key: &[u8; 32],
    iv: &[u8],
    index: &[u8],
    pb: &Arc<dyn ProgressSink>,
) -> Result<String>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let num_parts = size.div_ceil(PART_SIZE as u64) as u32;
    let start = net.start_upload(bucket, size, num_parts).await?;
    let slot = start
        .uploads
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no upload slot returned"))?;
    let urls = slot.urls.ok_or_else(|| anyhow!("no upload urls returned"))?;
    let upload_id = slot
        .upload_id
        .ok_or_else(|| anyhow!("no UploadId returned"))?;

    let mut hasher = Sha256::new();
    let mut ctr = Ctr::new(key, iv);

    let sem = Arc::new(tokio::sync::Semaphore::new(UPLOAD_CONCURRENCY));
    let mut handles = Vec::new();
    let mut part_buf: Vec<u8> = Vec::with_capacity(PART_SIZE);
    let mut part_number: u32 = 1;
    let mut read_buf = vec![0u8; READ_CHUNK];

    loop {
        let n = reader.read(&mut read_buf).await?;
        if n == 0 {
            break;
        }
        let mut chunk = read_buf[..n].to_vec();
        ctr.apply(&mut chunk);
        hasher.update(&chunk);
        part_buf.extend_from_slice(&chunk);

        while part_buf.len() >= PART_SIZE {
            let rest = part_buf.split_off(PART_SIZE);
            let body = std::mem::replace(&mut part_buf, rest);
            dispatch_part(net, &urls, &sem, pb, &mut handles, part_number, body).await?;
            part_number += 1;
        }
    }
    if !part_buf.is_empty() {
        let body = std::mem::take(&mut part_buf);
        dispatch_part(net, &urls, &sem, pb, &mut handles, part_number, body).await?;
    }

    let mut parts = Vec::with_capacity(handles.len());
    let mut iter = handles.into_iter();
    let mut result: Result<()> = Ok(());
    for h in iter.by_ref() {
        match h.await {
            Ok(Ok(p)) => parts.push(p),
            Ok(Err(e)) => {
                result = Err(e);
                break;
            }
            Err(e) => {
                result = Err(anyhow!("part task panicked: {e}"));
                break;
            }
        }
    }
    if let Err(e) = result {
        // Some parts may still be uploading in the background; a dropped
        // JoinHandle does NOT cancel the task, so abort the rest explicitly
        // to stop wasting bandwidth. Note: there's no server-side "abort
        // multipart upload" API exposed by the bridge today, so the
        // incomplete multipart session on the storage side may still linger
        // until it expires there — that's a separate, larger fix.
        for h in iter {
            h.abort();
        }
        return Err(e);
    }
    parts.sort_by_key(|p| p.part_number);

    let digest = hasher.finalize();
    let hash = hex::encode(crypto::ripemd160(&digest));

    let finish = net
        .finish_multipart_upload(bucket, &hex::encode(index), &hash, &slot.uuid, &upload_id, &parts)
        .await?;
    Ok(finish.id)
}

async fn dispatch_part(
    net: &NetworkApi,
    urls: &[String],
    sem: &Arc<tokio::sync::Semaphore>,
    pb: &Arc<dyn ProgressSink>,
    handles: &mut Vec<tokio::task::JoinHandle<Result<PartRef>>>,
    part_number: u32,
    body: Vec<u8>,
) -> Result<()> {
    let url = urls
        .get((part_number - 1) as usize)
        .ok_or_else(|| anyhow!("missing presigned url for part {part_number}"))?
        .clone();
    let permit = sem.clone().acquire_owned().await.unwrap();
    let net = net.clone();
    let pb = pb.clone();
    let len = body.len() as u64;
    handles.push(tokio::spawn(async move {
        let _permit = permit;
        let etag = net.put_part(&url, body).await?;
        pb.inc(len);
        Ok(PartRef { part_number, etag })
    }));
    Ok(())
}

/// Stream a network file's decrypted bytes into an arbitrary writer. Reusable
/// core behind `download-file`, the WebDAV GET handler and the FUSE read path.
///
/// `range` optionally restricts output to a byte window `(start, len)`. With
/// per-shard sizes the CTR keystream is seeked to `start` and only the covering
/// shards are fetched (boundary shards byte-ranged over HTTP); otherwise it falls
/// back to decrypting from byte 0 and discarding the prefix. No progress / status
/// output — the caller owns that.
pub async fn download_file_to_writer<W>(
    net: &NetworkApi,
    mnemonic: &str,
    bucket: &str,
    file_id: &str,
    out: &mut W,
    range: Option<(u64, u64)>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let links = net.get_download_links(bucket, file_id).await?;
    if matches!(links.version, None | Some(1)) {
        return Err(anyhow!("File version 1 not supported"));
    }

    let index = hex::decode(&links.index)?;
    let iv = &index[0..16];
    let key = crypto::generate_file_key(mnemonic, bucket, &index)?;

    let mut shards = links.shards.clone();
    shards.sort_by_key(|s| s.index);

    let mut ctr = Ctr::new(&key, iv);

    let (start, end) = match range {
        // Exclusive byte window `[start, end)`, clamped to the file size.
        Some((start, len)) => {
            let start = start.min(links.size);
            (start, start.saturating_add(len).min(links.size))
        }
        // Whole file.
        None => (0, links.size),
    };
    if end <= start {
        out.flush().await?;
        return Ok(());
    }

    // The whole file is one continuous CTR stream sliced into shards (ordered by
    // `index`). With per-shard sizes we can seek the keystream to `start`, skip
    // shards entirely below the window, and byte-range the boundary shards over
    // HTTP — so a partial read only fetches the covered bytes. A single-shard
    // file needs no per-shard size (the shard spans the whole file); a
    // multi-shard file whose sizes the API omitted falls back to decrypting
    // from byte 0 and discarding the prefix (correct, but re-fetches it).
    let sizes_known = shards.len() == 1 || shards.iter().all(|s| s.size > 0);

    if sizes_known {
        ctr.seek(start);
        let mut base: u64 = 0;
        for shard in &shards {
            let ssize = if shards.len() == 1 { links.size } else { shard.size };
            let shard_start = base;
            let shard_end = base + ssize; // exclusive
            base = shard_end;
            if shard_end <= start {
                continue; // entirely before the window
            }
            if shard_start >= end {
                break; // entirely after the window
            }
            let ov_start = start.max(shard_start);
            let ov_end = end.min(shard_end); // exclusive
            let local_start = ov_start - shard_start;
            let local_end_incl = ov_end - shard_start - 1; // inclusive, S3 semantics
            let whole = local_start == 0 && ov_end == shard_end;
            let resp = if whole {
                net.download_shard_stream(&shard.url).await?
            } else {
                net.download_shard_range_stream(&shard.url, local_start, local_end_incl)
                    .await?
            };
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let mut bytes = chunk?.to_vec();
                ctr.apply(&mut bytes);
                out.write_all(&bytes).await?;
            }
        }
    } else {
        // Fallback: multi-shard file with unknown sizes. Decrypt continuously
        // from the start and skip the prefix bytes before `start`.
        let mut to_skip = start;
        let mut remaining = end - start;
        'shards: for shard in &shards {
            let resp = net.download_shard_stream(&shard.url).await?;
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let mut bytes = chunk?.to_vec();
                ctr.apply(&mut bytes);
                let mut slice: &[u8] = &bytes;
                if to_skip > 0 {
                    let drop = (to_skip as usize).min(slice.len());
                    slice = &slice[drop..];
                    to_skip -= drop as u64;
                }
                let take = (remaining as usize).min(slice.len());
                slice = &slice[..take];
                remaining -= take as u64;
                if !slice.is_empty() {
                    out.write_all(slice).await?;
                }
                if remaining == 0 {
                    break 'shards;
                }
            }
        }
    }
    out.flush().await?;
    Ok(())
}

/// Create a folder, retrying transient failures. If the API reports the folder
/// already exists, looks up and returns the *existing* folder's uuid instead of
/// giving up — paginating the parent's subfolders (via [`DriveApi::get_folder_subfolders`])
/// and matching by exact name, the same "list children, match by name" pattern
/// `internxt-cli-rust`'s serve-tree cache-miss fallback uses. This lets callers keep
/// using the returned uuid as a parent for further creates, instead of treating a
/// name collision as a hard failure. Returns `Ok(None)` only if that lookup fails to
/// find a match (e.g. a race where the folder was renamed/deleted between the
/// conflict and the lookup) or after non-conflict retries are exhausted.
///
/// Requires the `fs` feature (uses `tokio::time` for retry backoff).
#[cfg(feature = "fs")]
pub async fn create_folder_with_retry(
    api: &DriveApi,
    token: &str,
    name: &str,
    parent_uuid: &str,
) -> Result<Option<String>> {
    for attempt in 0..=FOLDER_CREATE_RETRIES {
        match api.create_folder(token, name, parent_uuid).await {
            Ok(v) => {
                let uuid = v["uuid"].as_str().unwrap_or_default().to_string();
                return Ok(Some(uuid));
            }
            Err(e) => {
                if e.to_string().to_lowercase().contains("already exists") {
                    return find_existing_folder(api, token, parent_uuid, name).await;
                }
                if attempt < FOLDER_CREATE_RETRIES {
                    tokio::time::sleep(std::time::Duration::from_millis(RETRY_DELAYS_MS[attempt]))
                        .await;
                } else {
                    return Err(e);
                }
            }
        }
    }
    Ok(None)
}

/// Look up a direct subfolder of `parent_uuid` by exact (plain) name, paginating
/// through `get_folder_subfolders` (50 per page) until a match is found or the
/// listing is exhausted. Only `EXISTS`-status entries count — a trashed/deleted
/// folder with the same name doesn't count as a collision.
#[cfg(feature = "fs")]
async fn find_existing_folder(
    api: &DriveApi,
    token: &str,
    parent_uuid: &str,
    name: &str,
) -> Result<Option<String>> {
    let mut offset: u32 = 0;
    loop {
        let page = api.get_folder_subfolders(token, parent_uuid, offset).await?;
        let items = page
            .get("folders")
            .or_else(|| page.get("result"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let got = items.len() as u32;
        for item in &items {
            let status = item.get("status").and_then(|s| s.as_str()).unwrap_or("");
            if !status.is_empty() && status != "EXISTS" {
                continue;
            }
            let plain_name = item.get("plainName").and_then(|s| s.as_str()).unwrap_or("");
            if plain_name == name {
                let uuid = item.get("uuid").and_then(|s| s.as_str()).unwrap_or_default();
                if !uuid.is_empty() {
                    return Ok(Some(uuid.to_string()));
                }
            }
        }
        if got < 50 {
            return Ok(None);
        }
        offset += got;
    }
}

/// Tests for `create_folder_with_retry`'s "already exists" handling. `DriveApi`
/// has no mock/injection seam and this crate has no HTTP-mocking dev-dependency,
/// so these spin up a tiny hand-rolled HTTP/1.1 server on localhost and point
/// `DriveApi` at it via the `DRIVE_NEW_API_URL` env override (read fresh on every
/// `DriveApi::new()` call, see `config::drive_api_url`). Each mocked response sets
/// `Connection: close` so the client opens a fresh connection per call, keeping
/// "which request is this" trivial without a real request router.
#[cfg(all(test, feature = "fs"))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Serialize access to the `DRIVE_NEW_API_URL` env var: tests in this module
    /// run in separate threads by default, and the var is process-global.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(1000)))
            .ok();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                        let header_str = String::from_utf8_lossy(&buf[..pos]).to_string();
                        let cl: usize = header_str
                            .lines()
                            .find_map(|l| {
                                let lower = l.to_lowercase();
                                lower.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if buf.len() >= pos + 4 + cl {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    /// Spins up a background thread serving `responses.len()` sequential
    /// connections, in order, then returns the `http://host:port` base to point
    /// `DriveApi` at.
    fn mock_server(responses: Vec<(u16, &'static str, String)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (status, reason, body) in responses {
                if let Ok((mut stream, _)) = listener.accept() {
                    let _req = read_request(&mut stream);
                    let resp = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                }
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn already_exists_resolves_to_existing_uuid() {
        let _guard = ENV_LOCK.lock().await;
        let base = mock_server(vec![
            (
                409,
                "Conflict",
                r#"{"message":"Folder already exists"}"#.to_string(),
            ),
            (
                200,
                "OK",
                r#"{"folders":[{"uuid":"existing-uuid","plainName":"docs","status":"EXISTS"}]}"#
                    .to_string(),
            ),
        ]);
        unsafe { std::env::set_var("DRIVE_NEW_API_URL", &base) };
        let api = DriveApi::new();
        let result = create_folder_with_retry(&api, "tok", "docs", "parent-uuid")
            .await
            .unwrap();
        unsafe { std::env::remove_var("DRIVE_NEW_API_URL") };
        assert_eq!(result, Some("existing-uuid".to_string()));
    }

    #[tokio::test]
    async fn already_exists_but_lookup_finds_no_match_returns_none() {
        let _guard = ENV_LOCK.lock().await;
        let base = mock_server(vec![
            (
                409,
                "Conflict",
                r#"{"message":"Folder already exists"}"#.to_string(),
            ),
            (200, "OK", r#"{"folders":[]}"#.to_string()),
        ]);
        unsafe { std::env::set_var("DRIVE_NEW_API_URL", &base) };
        let api = DriveApi::new();
        let result = create_folder_with_retry(&api, "tok", "docs", "parent-uuid")
            .await
            .unwrap();
        unsafe { std::env::remove_var("DRIVE_NEW_API_URL") };
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn already_exists_skips_non_exists_status_and_paginates() {
        let _guard = ENV_LOCK.lock().await;
        // Page 1: 50 entries (forces a second page) none matching by name (one
        // matches by name but is TRASHED, so it must not count); page 2 has the
        // real match.
        let mut page1_items = vec![
            r#"{"uuid":"trashed-uuid","plainName":"docs","status":"TRASHED"}"#.to_string(),
        ];
        for i in 0..49 {
            page1_items.push(format!(r#"{{"uuid":"other-{i}","plainName":"other-{i}","status":"EXISTS"}}"#));
        }
        let page1 = format!(r#"{{"folders":[{}]}}"#, page1_items.join(","));
        let page2 =
            r#"{"folders":[{"uuid":"real-uuid","plainName":"docs","status":"EXISTS"}]}"#.to_string();
        let base = mock_server(vec![
            (
                409,
                "Conflict",
                r#"{"message":"Folder already exists"}"#.to_string(),
            ),
            (200, "OK", page1),
            (200, "OK", page2),
        ]);
        unsafe { std::env::set_var("DRIVE_NEW_API_URL", &base) };
        let api = DriveApi::new();
        let result = create_folder_with_retry(&api, "tok", "docs", "parent-uuid")
            .await
            .unwrap();
        unsafe { std::env::remove_var("DRIVE_NEW_API_URL") };
        assert_eq!(result, Some("real-uuid".to_string()));
    }

    #[tokio::test]
    async fn create_succeeds_returns_new_uuid_without_lookup() {
        let _guard = ENV_LOCK.lock().await;
        let base = mock_server(vec![(
            200,
            "OK",
            r#"{"uuid":"brand-new-uuid","plainName":"docs"}"#.to_string(),
        )]);
        unsafe { std::env::set_var("DRIVE_NEW_API_URL", &base) };
        let api = DriveApi::new();
        let result = create_folder_with_retry(&api, "tok", "docs", "parent-uuid")
            .await
            .unwrap();
        unsafe { std::env::remove_var("DRIVE_NEW_API_URL") };
        assert_eq!(result, Some("brand-new-uuid".to_string()));
    }

    /// An in-memory, non-seekable `AsyncRead` — stands in for a live WebDAV/FUSE
    /// write body, which (unlike a file) can only be read forward once.
    struct SliceReader {
        data: Vec<u8>,
        pos: usize,
    }

    impl AsyncRead for SliceReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let remaining = &self.data[self.pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            self.pos += n;
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// Unlike `mock_server` (fixed sequential responses — one connection per list
    /// entry), multipart dispatches several part PUTs concurrently, so this routes
    /// by method+path instead of connection order: `POST .../start` and
    /// `POST .../finish` return canned JSON, `PUT /partN` records the body length
    /// it received and returns a fake ETag.
    #[allow(clippy::type_complexity)]
    fn mock_network_server() -> (
        String,
        Arc<Mutex<Vec<(u32, usize)>>>,
        Arc<Mutex<String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let parts: Arc<Mutex<Vec<(u32, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let start_query: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let (parts2, start_query2) = (parts.clone(), start_query.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (parts, start_query) = (parts2.clone(), start_query2.clone());
                std::thread::spawn(move || handle_network_conn(&mut stream, &parts, &start_query));
            }
        });
        (base, parts, start_query)
    }

    fn handle_network_conn(
        stream: &mut std::net::TcpStream,
        parts: &Arc<Mutex<Vec<(u32, usize)>>>,
        start_query: &Arc<Mutex<String>>,
    ) {
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(2000)))
            .ok();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        let (method, path, body) = loop {
            match stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(pos) = find_subslice(&buf, b"\r\n\r\n") else {
                        continue;
                    };
                    let header_str = String::from_utf8_lossy(&buf[..pos]).to_string();
                    let mut req_line = header_str.lines().next().unwrap_or("").split_whitespace();
                    let method = req_line.next().unwrap_or("").to_string();
                    let path = req_line.next().unwrap_or("").to_string();
                    let cl: usize = header_str
                        .lines()
                        .find_map(|l| {
                            let lower = l.to_lowercase();
                            lower.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    while buf.len() < pos + 4 + cl {
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            Err(_) => break,
                        }
                    }
                    let body = buf[pos + 4..(pos + 4 + cl).min(buf.len())].to_vec();
                    break (method, path, body);
                }
                Err(_) => return,
            }
        };

        if method == "POST" && path.starts_with("/v2/buckets/") && path.contains("/files/start") {
            if let Some(q) = path.split('?').nth(1) {
                *start_query.lock().unwrap() = q.to_string();
            }
            let base = format!(
                "http://{}",
                stream.local_addr().map(|a| a.to_string()).unwrap_or_default()
            );
            let json = format!(
                r#"{{"uploads":[{{"uuid":"test-uuid","urls":["{base}/part1","{base}/part2"],"UploadId":"UPLOADID"}}]}}"#
            );
            write_response(stream, 200, "OK", "application/json", &json, &[]);
        } else if method == "PUT" && path.starts_with("/part") {
            let n: u32 = path.trim_start_matches("/part").parse().unwrap_or(0);
            parts.lock().unwrap().push((n, body.len()));
            let etag = format!("ETag: \"etag-{n}\"\r\n");
            write_response(stream, 200, "OK", "application/octet-stream", "", &[&etag]);
        } else if method == "POST" && path.contains("/files/finish") {
            write_response(
                stream,
                200,
                "OK",
                "application/json",
                r#"{"id":"finished-id"}"#,
                &[],
            );
        } else {
            write_response(stream, 404, "Not Found", "text/plain", "", &[]);
        }
    }

    fn write_response(
        stream: &mut std::net::TcpStream,
        status: u16,
        reason: &str,
        content_type: &str,
        body: &str,
        extra_headers: &[&str],
    ) {
        let extra: String = extra_headers.concat();
        let resp = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    }

    /// Regression test for the fix: `upload_stream_to_network`'s underlying
    /// multipart path used to be file-only (`upload_multipart` took `&Path`).
    /// Feeding it a plain in-memory, non-seekable reader over a body larger than
    /// one 15MB part must still slice it into multiple parts and PUT them
    /// individually, instead of requiring a seekable file source.
    #[tokio::test]
    async fn generic_reader_multipart_splits_into_parts() {
        let _guard = ENV_LOCK.lock().await;
        let (base, parts, start_query) = mock_network_server();
        unsafe { std::env::set_var("NETWORK_URL", &base) };

        let size = PART_SIZE + 5 * 1024 * 1024; // 20MB: 15MB + 5MB => 2 parts
        let data = vec![0xABu8; size];
        let reader = SliceReader { data, pos: 0 };

        let net = NetworkApi::new("bridge-user", "user-id");
        let key = [0u8; 32];
        let iv = vec![0u8; 16];
        let index = vec![0u8; 32];
        let pb = noop_sink();

        let result = upload_multipart(&net, "bucket", size as u64, reader, &key, &iv, &index, &pb)
            .await
            .unwrap();

        unsafe { std::env::remove_var("NETWORK_URL") };

        assert_eq!(result, "finished-id");
        let recorded = parts.lock().unwrap().clone();
        assert_eq!(recorded.len(), 2, "expected 2 parts, got {recorded:?}");
        let total: usize = recorded.iter().map(|(_, len)| *len).sum();
        assert_eq!(total, size);
        assert!(
            start_query.lock().unwrap().contains("multiparts=2"),
            "start-upload query was {:?}",
            start_query.lock().unwrap()
        );
    }
}

