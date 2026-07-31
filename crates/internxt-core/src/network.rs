//! Network (bridge) client. Mirrors og/sdk network + og/inxt-js uploadV2/multipart.
//! Basic auth = bridgeUser : sha256(userId).hex
//! All transfers stream — no full-file buffering (supports 100GB files).

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use bytes::Bytes;
use futures_util::Stream;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::{Body, Client, Response};
use serde_json::json;
use std::time::Duration;

use crate::config;
use crate::crypto;
use crate::models::{DownloadLinksResponse, FinishUploadResponse, StartUploadResponse};

/// Connecting to a reachable host should be fast; anything slower almost
/// certainly means a dead peer or a firewalled black hole.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Idle-read timeout for the download/metadata leg (start/finish upload,
/// download-links, and shard GETs) — reqwest resets it on every successful
/// response-body read, so it only fires on a truly stalled/idle connection
/// (dead peer, black hole, unresponsive presigned URL) and never caps total
/// download duration.
///
/// Deliberately **not** applied to the upload (PUT) leg: reqwest 0.13's
/// `read_timeout` is a single non-resetting deadline covering connect →
/// body-send → response-headers, so on a PUT (whose 200 OK only arrives
/// after the whole body has been received by the server) it behaves as a
/// hard cap on *total upload duration* rather than an idle-stall detector —
/// it would abort a slow-but-healthy upload (e.g. a slow `--stdin` producer,
/// or any large file over a modest link) exactly as readily as a genuinely
/// dead connection. See `upload_client` below.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Timeouts for [`NetworkApi`]'s HTTP clients. `read` applies only to the
/// download/metadata leg (see [`DEFAULT_READ_TIMEOUT`]) — the upload leg
/// only ever gets `connect`, never a read/total timeout, since reqwest can't
/// express "abort on stall" for a streamed PUT without also aborting slow
/// producers. Pass `read: None` to disable idle-stall detection on downloads
/// too.
#[derive(Clone, Copy, Debug)]
pub struct NetworkTimeouts {
    pub connect: Duration,
    pub read: Option<Duration>,
}

impl Default for NetworkTimeouts {
    fn default() -> Self {
        NetworkTimeouts {
            connect: DEFAULT_CONNECT_TIMEOUT,
            read: Some(DEFAULT_READ_TIMEOUT),
        }
    }
}

#[derive(Clone)]
pub struct NetworkApi {
    /// Metadata calls (start/finish upload, download-links) + shard GETs.
    client: Client,
    /// Shard/part PUTs only — connect timeout, no read/total timeout (see
    /// [`DEFAULT_READ_TIMEOUT`] doc for why).
    upload_client: Client,
    base: String,
    auth_header: HeaderValue,
}

/// One uploaded part reference for the multipart finish payload.
#[derive(Clone)]
pub struct PartRef {
    pub part_number: u32,
    pub etag: String,
}

impl NetworkApi {
    pub fn new(bridge_user: &str, user_id: &str) -> Self {
        Self::with_timeouts(bridge_user, user_id, NetworkTimeouts::default())
    }

    /// Same as [`Self::new`], with caller-adjustable timeouts (e.g. so
    /// `internxt-cli-rust` can widen or disable them via a flag/env var).
    pub fn with_timeouts(bridge_user: &str, user_id: &str, timeouts: NetworkTimeouts) -> Self {
        let password = crypto::network_password(user_id);
        let token = format!("{bridge_user}:{password}");
        let encoded = B64.encode(token.as_bytes());

        let mut builder = Client::builder().connect_timeout(timeouts.connect);
        if let Some(read) = timeouts.read {
            builder = builder.read_timeout(read);
        }
        let client = builder.build().unwrap_or_default();

        let upload_client = Client::builder()
            .connect_timeout(timeouts.connect)
            .build()
            .unwrap_or_default();

        NetworkApi {
            client,
            upload_client,
            base: config::network_url(),
            auth_header: HeaderValue::from_str(&format!("Basic {encoded}")).unwrap(),
        }
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        if let Ok(v) = HeaderValue::from_str(&config::client_version()) {
            h.insert("internxt-version", v);
        }
        if let Ok(v) = HeaderValue::from_str(&config::client_name()) {
            h.insert("internxt-client", v);
        }
        h.insert(AUTHORIZATION, self.auth_header.clone());
        h
    }

    /// `parts` = number of multipart slots (1 for single-part upload).
    pub async fn start_upload(
        &self,
        bucket: &str,
        size: u64,
        parts: u32,
    ) -> Result<StartUploadResponse> {
        let url = format!(
            "{}/v2/buckets/{}/files/start?multiparts={}",
            self.base, bucket, parts
        );
        let body = json!({ "uploads": [{ "index": 0, "size": size }] });
        let resp = self
            .client
            .post(url)
            .headers(self.headers())
            .json(&body)
            .send()
            .await?;
        let (status, text) = (resp.status(), resp.text().await.unwrap_or_default());
        if !status.is_success() {
            return Err(anyhow!("startUpload failed: HTTP {status}: {text}"));
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// PUT a streamed body of known length to a presigned url (single-part upload).
    pub async fn put_stream<S>(&self, url: &str, len: u64, stream: S) -> Result<()>
    where
        S: Stream<Item = std::io::Result<Bytes>> + Send + 'static,
    {
        let resp = self
            .upload_client
            .put(url)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, len)
            .body(Body::wrap_stream(stream))
            .send()
            .await?;
        if !resp.status().is_success() {
            let s = resp.status();
            let t = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to upload file: {s} {t}"));
        }
        Ok(())
    }

    /// PUT a single in-memory part, returns its ETag.
    pub async fn put_part(&self, url: &str, body: Vec<u8>) -> Result<String> {
        let len = body.len();
        let resp = self
            .upload_client
            .put(url)
            .header(CONTENT_LENGTH, len)
            .body(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let s = resp.status();
            let t = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Failed to upload part: {s} {t}"));
        }
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("ETag header was not returned"))?;
        Ok(etag)
    }

    pub async fn finish_upload(
        &self,
        bucket: &str,
        index_hex: &str,
        hash: &str,
        uuid: &str,
    ) -> Result<FinishUploadResponse> {
        let body = json!({
            "index": index_hex,
            "shards": [{ "hash": hash, "uuid": uuid }],
        });
        self.post_finish(bucket, body).await
    }

    pub async fn finish_multipart_upload(
        &self,
        bucket: &str,
        index_hex: &str,
        hash: &str,
        uuid: &str,
        upload_id: &str,
        parts: &[PartRef],
    ) -> Result<FinishUploadResponse> {
        let parts_json: Vec<_> = parts
            .iter()
            .map(|p| json!({ "PartNumber": p.part_number, "ETag": p.etag }))
            .collect();
        let body = json!({
            "index": index_hex,
            "shards": [{
                "hash": hash,
                "uuid": uuid,
                "UploadId": upload_id,
                "parts": parts_json,
            }],
        });
        self.post_finish(bucket, body).await
    }

    async fn post_finish(
        &self,
        bucket: &str,
        body: serde_json::Value,
    ) -> Result<FinishUploadResponse> {
        let url = format!("{}/v2/buckets/{}/files/finish", self.base, bucket);
        let resp = self
            .client
            .post(url)
            .headers(self.headers())
            .json(&body)
            .send()
            .await?;
        let (status, text) = (resp.status(), resp.text().await.unwrap_or_default());
        if !status.is_success() {
            return Err(anyhow!("finishUpload failed: HTTP {status}: {text}"));
        }
        Ok(serde_json::from_str(&text)?)
    }

    pub async fn get_download_links(
        &self,
        bucket: &str,
        file_id: &str,
    ) -> Result<DownloadLinksResponse> {
        let url = format!("{}/buckets/{}/files/{}/info", self.base, bucket, file_id);
        let mut headers = self.headers();
        headers.insert("x-api-version", HeaderValue::from_static("2"));
        let resp = self.client.get(url).headers(headers).send().await?;
        let (status, text) = (resp.status(), resp.text().await.unwrap_or_default());
        if !status.is_success() {
            return Err(anyhow!("getDownloadLinks failed: HTTP {status}: {text}"));
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// GET a shard url, returns the streaming response for chunked decrypt-to-disk.
    pub async fn download_shard_stream(&self, url: &str) -> Result<Response> {
        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("downloadShard failed: HTTP {}", resp.status()));
        }
        Ok(resp)
    }

    /// GET a byte range `[start, end]` (inclusive, S3 semantics) of a shard.
    /// Shard URLs are presigned S3 GETs, which honour `Range` — so a partial
    /// read fetches only the covered bytes instead of the whole shard.
    pub async fn download_shard_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Result<Response> {
        let resp = self
            .client
            .get(url)
            .header("Range", format!("bytes={start}-{end}"))
            .send()
            .await?;
        // 206 Partial Content on success; a server ignoring Range yields 200
        // with the whole body, which would break our offset math — reject it.
        if resp.status().as_u16() != 206 {
            return Err(anyhow!(
                "downloadShard range failed: expected HTTP 206, got {}",
                resp.status()
            ));
        }
        Ok(resp)
    }
}

