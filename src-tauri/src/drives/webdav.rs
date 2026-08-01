//! Generic WebDAV `CloudDrive` impl.
//!
//! Talks plain HTTP to any RFC 4918 server.  Tested against:
//!   * Nextcloud / ownCloud  (`https://host/remote.php/dav/files/<user>/`)
//!   * mailbox.org           (`https://dav.mailbox.org/servlet/dav/<user>/`)
//!   * `filen webdav-start`  (Filen's local WebDAV server, no TLS by default)
//!   * `internxt webdav-enable`
//!   * Synology DSM
//!
//! Why not use the OS-mount path?  Because (a) WebDAV mounts are flaky on
//! macOS for unauthenticated paths, (b) we want streaming reads/writes
//! and proper auth handling without `davfs2`/Finder, and (c) one Rust
//! impl beats N FUSE setups across the user's machines.
//!
//! Auth: HTTP Basic (username + password from `DriveConfig`).  Bearer
//! tokens / Digest can be added later by reading the `password` field
//! and routing on a prefix (e.g. `bearer:eyJ…`) — out of scope here.
//!
//! Path semantics:
//!   * `DriveConfig.path` is the **base URL**, ending in `/` (e.g.
//!     `https://host/dav/`).
//!   * The `path` argument to `list_dir` / `read_file` / etc. is appended
//!     to that base.  Both `/Documents` and `Documents` work; we
//!     normalise leading `/`.
//!   * Each segment is percent-encoded so `Photos/2024 holiday.jpg`
//!     becomes `Photos/2024%20holiday.jpg`.
//!
//! Concurrency: `reqwest::blocking` works fine inside a Tokio runtime
//! when the trait method is called from `tauri::command`s, because
//! Tauri commands run on a worker thread.

use anyhow::{anyhow, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use crate::sync::proxy::{build_blocking_client_with_options, ProxyConfig};

use super::{CloudDrive, DirEntry, DriveCapabilities, DriveType, FileStat};

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:displayname/>
    <D:getcontentlength/>
    <D:getlastmodified/>
    <D:resourcetype/>
  </D:prop>
</D:propfind>"#;

pub struct WebDavDrive {
    label: String,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    client: reqwest::blocking::Client,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DeltaStatus {
    app: Option<String>,
    #[serde(rename = "blockSize")]
    block_size: Option<u32>,
    algorithm: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DeltaSignature {
    #[serde(rename = "blockIndex")]
    block_index: usize,
    offset: u64,
    size: u32,
    #[serde(rename = "weakHash")]
    weak_hash: u32,
    #[serde(rename = "strongHash")]
    strong_hash: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ServerBlockMap {
    #[serde(rename = "filePath")]
    file_path: String,
    #[serde(rename = "totalSize")]
    total_size: u64,
    #[serde(rename = "blockSize")]
    block_size: u32,
    #[serde(rename = "blockCount")]
    block_count: usize,
    signatures: Vec<DeltaSignature>,
    etag: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeltaTransferResult {
    pub changed_blocks: usize,
    pub total_blocks: usize,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub etag: Option<String>,
}

impl WebDavDrive {
    pub fn new(
        label: impl Into<String>,
        base_url: impl Into<String>,
        username: Option<String>,
        password: Option<String>,
        insecure_tls: bool,
    ) -> Self {
        Self::new_with_proxy(
            label,
            base_url,
            username,
            password,
            insecure_tls,
            &ProxyConfig::default(),
        )
        .expect("default WebDAV HTTP client must build")
    }

    /// Construct a WebDAV drive with the shared HTTP/SOCKS5 proxy policy.
    ///
    /// The legacy `new` constructor remains proxy-free for compatibility;
    /// registry/application code should use this boundary when a configured
    /// proxy is available.  Proxy credentials are supplied in-memory by the
    /// caller and are never part of the serialized drive metadata.
    pub fn new_with_proxy(
        label: impl Into<String>,
        base_url: impl Into<String>,
        username: Option<String>,
        password: Option<String>,
        insecure_tls: bool,
        proxy: &ProxyConfig,
    ) -> Result<Self> {
        let mut url = base_url.into();
        if !url.ends_with('/') {
            url.push('/');
        }
        let client =
            build_blocking_client_with_options(proxy, Some(Duration::from_secs(60)), insecure_tls)?;
        Ok(Self {
            label: label.into(),
            base_url: url,
            username,
            password,
            client,
        })
    }

    /// Build the full URL for a path relative to the drive root.
    /// Strips leading `/` and percent-encodes each segment.
    fn url_for(&self, path: &Path) -> String {
        let s = path.to_string_lossy();
        let trimmed = s.trim_start_matches('/');
        // Encode each segment, leaving the slashes intact.
        let encoded: Vec<String> = trimmed.split('/').map(percent_encode_segment).collect();
        format!("{}{}", self.base_url, encoded.join("/"))
    }

    fn req(&self, method: reqwest::Method, url: &str) -> reqwest::blocking::RequestBuilder {
        let mut b = self.client.request(method, url);
        if let Some(u) = &self.username {
            b = b.basic_auth(u, self.password.as_deref());
        }
        b
    }

    /// Return Nextcloud's OCS sharing endpoint when this looks like a
    /// Nextcloud WebDAV root. Other WebDAV servers must remain untouched:
    /// probing an arbitrary DAV server with an OCS request is not safe.
    fn nextcloud_ocs_url(&self) -> Option<String> {
        let mut url = reqwest::Url::parse(&self.base_url).ok()?;
        if !url.path().contains("/remote.php/") {
            return None;
        }
        url.set_path("/ocs/v2.php/apps/files_sharing/api/v1/shares");
        url.set_query(Some("format=json"));
        url.set_fragment(None);
        Some(url.to_string())
    }

    /// The optional CrispCloud server-app endpoint used by the patched
    /// Nextcloud and ownCloud clients. It is deliberately separate from
    /// ordinary WebDAV: a normal DAV server must fall back safely.
    fn delta_app_base(&self) -> Option<String> {
        let mut url = reqwest::Url::parse(&self.base_url).ok()?;
        if !url.path().contains("/remote.php/") {
            return None;
        }
        url.set_path("/index.php/apps/crispcloud_delta");
        url.set_query(None);
        url.set_fragment(None);
        Some(url.to_string().trim_end_matches('/').to_owned())
    }

    fn delta_path_url(&self, route: &str, path: &Path) -> Result<String> {
        let base = self
            .delta_app_base()
            .ok_or_else(|| anyhow!("WebDAV root is not a Nextcloud/ownCloud remote.php root"))?;
        let clean = path.to_string_lossy().replace('\\', "/");
        let encoded = percent_encode_segment(clean.trim_start_matches('/'));
        Ok(format!("{base}/api/{route}/{encoded}"))
    }

    /// Detect the optional crispcloud_delta server app.
    pub fn delta_sync_available(&self) -> Result<bool> {
        let Some(base) = self.delta_app_base() else {
            return Ok(false);
        };
        let response = self
            .req(reqwest::Method::GET, &format!("{base}/api/status"))
            .send()
            .context("checking crispcloud_delta status")?;
        if !response.status().is_success() {
            return Ok(false);
        }
        let status: DeltaStatus = response.json().context("parsing crispcloud_delta status")?;
        Ok(status.app.as_deref() == Some("crispcloud_delta")
            && status.algorithm.as_deref() == Some("adler32+sha256")
            && status.block_size.is_some())
    }

    fn fetch_delta_map(&self, path: &Path) -> Result<Option<ServerBlockMap>> {
        if !self.delta_sync_available()? {
            return Ok(None);
        }
        let url = self.delta_path_url("blockmap", path)?;
        let response = self.req(reqwest::Method::GET, &url).send()?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response.error_for_status()?;
        let map: ServerBlockMap = response
            .json()
            .context("parsing crispcloud_delta block map")?;
        if map.block_count != map.signatures.len() || map.block_size == 0 {
            return Err(anyhow!(
                "invalid crispcloud_delta block map for {}",
                map.file_path
            ));
        }
        Ok(Some(map))
    }

    fn blockmap_from_server(map: &ServerBlockMap) -> Result<crate::sync::delta::Blockmap> {
        let mut blocks = Vec::with_capacity(map.signatures.len());
        for signature in &map.signatures {
            let bytes = hex::decode(&signature.strong_hash).with_context(|| {
                format!("invalid strongHash for block {}", signature.block_index)
            })?;
            let strong_hash: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow!("strongHash must be 32 bytes"))?;
            blocks.push(crate::sync::delta::Block {
                offset: signature.offset,
                size: signature.size,
                weak_hash: signature.weak_hash,
                strong_hash,
            });
        }
        Ok(crate::sync::delta::Blockmap {
            file_size: map.total_size,
            block_size: map.block_size,
            blocks,
        })
    }

    /// Upload only changed blocks through crispcloud_delta. Returns None when
    /// the optional server app or a remote block map is unavailable, allowing
    /// callers to perform the normal full WebDAV upload.
    pub fn delta_upload_file(
        &self,
        local_path: &Path,
        remote_path: &Path,
    ) -> Result<Option<DeltaTransferResult>> {
        let Some(remote_map) = self.fetch_delta_map(remote_path)? else {
            return Ok(None);
        };
        let remote = Self::blockmap_from_server(&remote_map)?;
        let expected_etag = remote_map.etag.clone();
        let local = crate::sync::delta::compute_local_blockmap_against(local_path, &remote)?;
        let changed = crate::sync::delta::diff_blockmaps(&local, &remote)?;
        let total_bytes = local.file_size;
        let mut transferred_bytes = 0;
        let mut file = File::open(local_path).context("opening local delta upload")?;

        for block in &changed {
            let size = block.size as usize;
            let mut data = vec![0u8; size];
            file.seek(SeekFrom::Start(block.offset))?;
            file.read_exact(&mut data)?;
            let url = format!(
                "{}?offset={}&size={}",
                self.delta_path_url("blocks", remote_path)?,
                block.offset,
                block.size
            );
            let mut request = self
                .req(reqwest::Method::POST, &url)
                .header("Content-Type", "application/octet-stream");
            if let Some(etag) = &expected_etag {
                request = request.header("If-Match", format!("\"{etag}\""));
            }
            let response = request
                .body(data)
                .send()
                .with_context(|| format!("uploading delta block at {}", block.offset))?;
            response.error_for_status()?;
            transferred_bytes += block.size as u64;
        }

        let url = format!(
            "{}?size={}",
            self.delta_path_url("finalize", remote_path)?,
            total_bytes
        );
        let mut request = self.req(reqwest::Method::POST, &url);
        if let Some(etag) = &expected_etag {
            request = request.header("If-Match", format!("\"{etag}\""));
        }
        request
            .send()
            .context("finalizing crispcloud_delta upload")?
            .error_for_status()?;

        Ok(Some(DeltaTransferResult {
            changed_blocks: changed.len(),
            total_blocks: local.blocks.len(),
            transferred_bytes,
            total_bytes,
            etag: remote_map.etag,
        }))
    }

    /// Download only blocks that differ from an existing local file through
    /// WebDAV Range GET, using the server-app block map as the remote truth.
    pub fn delta_download_file(
        &self,
        remote_path: &Path,
        local_path: &Path,
    ) -> Result<Option<DeltaTransferResult>> {
        let Some(remote_map) = self.fetch_delta_map(remote_path)? else {
            return Ok(None);
        };
        if !local_path.exists() {
            return Ok(None);
        }
        let remote = Self::blockmap_from_server(&remote_map)?;
        let local = crate::sync::delta::compute_local_blockmap_against(local_path, &remote)?;
        let changed = crate::sync::delta::diff_blockmaps(&remote, &local)?;
        let mut file = OpenOptions::new().read(true).write(true).open(local_path)?;
        let mut transferred_bytes = 0;
        for block in &changed {
            let end = block.offset + block.size as u64 - 1;
            let data = self.read_range(remote_path, block.offset, end)?;
            if data.len() != block.size as usize {
                return Err(anyhow!("range response size mismatch at {}", block.offset));
            }
            file.seek(SeekFrom::Start(block.offset))?;
            file.write_all(&data)?;
            transferred_bytes += block.size as u64;
        }
        file.set_len(remote.file_size)?;
        Ok(Some(DeltaTransferResult {
            changed_blocks: changed.len(),
            total_blocks: remote.blocks.len(),
            transferred_bytes,
            total_bytes: remote.file_size,
            etag: remote_map.etag,
        }))
    }
}

/// Minimal RFC 3986 unreserved-set encoder.  Encode anything that isn't
/// `A–Z a–z 0–9 - . _ ~`.
fn percent_encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if safe {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// One <D:response> entry from a multi-status PROPFIND reply.
#[derive(Debug, Default, Clone)]
struct DavResponse {
    href: String,
    displayname: Option<String>,
    contentlength: Option<u64>,
    lastmodified: Option<String>,
    is_collection: bool,
}

/// Parse a multi-status PROPFIND XML body into a flat list of responses.
/// Resilient to unfamiliar properties (we only read what we ask for).
fn parse_propfind(body: &str) -> Result<Vec<DavResponse>> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut buf = Vec::new();

    let mut current: Option<DavResponse> = None;
    let mut path_stack: Vec<String> = Vec::new();
    let mut text_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = local_name(&e.name().as_ref());
                if local == "response" {
                    current = Some(DavResponse::default());
                }
                path_stack.push(local);
                text_buf.clear();
            }
            Ok(Event::Empty(e)) => {
                let local = local_name(&e.name().as_ref());
                // <D:collection/> inside resourcetype → it's a folder.
                if local == "collection" && path_stack.iter().any(|p| p == "resourcetype") {
                    if let Some(ref mut r) = current {
                        r.is_collection = true;
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if let Ok(s) = e.xml_content() {
                    text_buf.push_str(&s);
                }
            }
            Ok(Event::End(e)) => {
                let local = local_name(&e.name().as_ref());
                if let Some(ref mut r) = current {
                    match local.as_str() {
                        "href" => r.href = text_buf.trim().to_owned(),
                        "displayname" if !text_buf.trim().is_empty() => {
                            r.displayname = Some(text_buf.trim().to_owned())
                        }
                        "getcontentlength" if !text_buf.trim().is_empty() => {
                            r.contentlength = text_buf.trim().parse().ok()
                        }
                        "getlastmodified" if !text_buf.trim().is_empty() => {
                            r.lastmodified = Some(text_buf.trim().to_owned())
                        }
                        "response" => {
                            if let Some(done) = current.take() {
                                out.push(done);
                            }
                        }
                        _ => {}
                    }
                }
                path_stack.pop();
                text_buf.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("propfind parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn local_name(qname: &[u8]) -> String {
    let s = std::str::from_utf8(qname).unwrap_or("");
    s.split(':').next_back().unwrap_or(s).to_ascii_lowercase()
}

/// Parse RFC 7231 IMF-fixdate ("Wed, 21 Oct 2015 07:28:00 GMT") to unix
/// seconds.  Returns None for unrecognised shapes.
fn parse_http_date(s: &str) -> Option<i64> {
    // Stdlib has no HTTP-date parser.  We accept just the IMF-fixdate form
    // (which all RFC 4918 servers use for getlastmodified) and silently
    // ignore the day-of-week prefix.
    let trimmed = s.trim();
    let after_comma = trimmed.split_once(", ").map(|(_, r)| r)?;
    let mut parts = after_comma.splitn(5, ' ');
    let d: i64 = parts.next()?.parse().ok()?;
    let mon: &str = parts.next()?;
    let y: i64 = parts.next()?.parse().ok()?;
    let time: &str = parts.next()?;
    // Last token may be "GMT" or "+0000"; we ignore the value, assume UTC.
    let _tz = parts.next();

    let mo = match mon {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };

    let mut tparts = time.split(':');
    let h: i64 = tparts.next()?.parse().ok()?;
    let mi: i64 = tparts.next()?.parse().ok()?;
    let s_int: i64 = tparts.next()?.parse().ok()?;

    // days-from-civil (Howard Hinnant) — same algorithm used elsewhere.
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = (yy - era * 400) as u64;
    let m_norm = if mo > 2 {
        (mo - 3) as u64
    } else {
        (mo + 9) as u64
    };
    let doy = (153 * m_norm + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe as i64 - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + s_int)
}

impl CloudDrive for WebDavDrive {
    fn label(&self) -> &str {
        &self.label
    }
    fn drive_type(&self) -> DriveType {
        DriveType::WebDav
    }

    fn capabilities(&self) -> DriveCapabilities {
        DriveCapabilities {
            create_dir: true,
            rename: true,
            move_path: true,
            copy: true,
            streaming: true,
            ..DriveCapabilities::basic()
        }
    }

    fn probed_capabilities(&self) -> DriveCapabilities {
        let mut capabilities = self.capabilities();
        let Some(url) = self.nextcloud_ocs_url() else {
            return capabilities;
        };

        let response = self
            .req(reqwest::Method::GET, &url)
            .timeout(Duration::from_secs(5))
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .send();
        let Ok(response) = response else {
            return capabilities;
        };
        if !response.status().is_success() {
            return capabilities;
        }
        let Ok(body) = response.json::<serde_json::Value>() else {
            return capabilities;
        };
        let status_code = body["ocs"]["meta"]["statuscode"].as_i64();
        if status_code == Some(100) || status_code == Some(200) {
            capabilities.share_links = true;
        }
        capabilities
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let url = self.url_for(path);
        let resp = self
            .req(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(PROPFIND_BODY)
            .send()
            .with_context(|| format!("PROPFIND {url}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .with_context(|| format!("reading PROPFIND body for {url}"))?;
        if !status.is_success() && status.as_u16() != 207 {
            return Err(anyhow!("WebDAV PROPFIND {url} → {status}: {body}"));
        }

        let mut responses = parse_propfind(&body)?;
        // The first response is usually the directory itself; drop entries
        // whose href matches the request URL.  We do an approximate match
        // (path component only) so quirks like trailing-slash differences
        // don't trip us up.
        let req_path = self.url_for(path);
        let req_normalised = strip_origin_and_decode(&req_path);
        responses.retain(|r| {
            let href = strip_origin_and_decode(&r.href);
            href.trim_end_matches('/') != req_normalised.trim_end_matches('/')
        });

        let mut out = Vec::with_capacity(responses.len());
        for r in &responses {
            let name = name_from_response(r);
            if name.is_empty() {
                continue;
            }
            out.push(DirEntry {
                name,
                is_dir: r.is_collection,
                size: if r.is_collection {
                    None
                } else {
                    r.contentlength
                },
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn stat(&self, path: &Path) -> Result<FileStat> {
        let url = self.url_for(path);
        let resp = self
            .req(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &url)
            .header("Depth", "0")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(PROPFIND_BODY)
            .send()
            .with_context(|| format!("PROPFIND (Depth 0) {url}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .with_context(|| format!("reading PROPFIND body for {url}"))?;
        if !status.is_success() && status.as_u16() != 207 {
            return Err(anyhow!("WebDAV PROPFIND {url} → {status}: {body}"));
        }
        let responses = parse_propfind(&body)?;
        let r = responses
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("PROPFIND returned no <response> for {url}"))?;
        Ok(FileStat {
            size: r.contentlength.unwrap_or(0),
            is_dir: r.is_collection,
            mtime_unix: r.lastmodified.as_deref().and_then(parse_http_date),
        })
    }

    fn share_link(&self, path: &Path) -> Result<Option<String>> {
        let Some(url) = self.nextcloud_ocs_url() else {
            return Ok(None);
        };
        let clean = path.to_string_lossy().trim_start_matches('/').to_owned();
        let form_body = format!(
            "path={}&shareType=3&permissions=1",
            percent_encode_segment(&format!("/{clean}"))
        );
        let response = self
            .req(reqwest::Method::POST, &url)
            .header("OCS-APIRequest", "true")
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(form_body)
            .send()
            .with_context(|| format!("Nextcloud share_link: POST {url}"))?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .context("Nextcloud share_link: parse OCS response")?;
        if !status.is_success() {
            return Err(anyhow!("Nextcloud share_link: HTTP {status}"));
        }
        let status_code = body["ocs"]["meta"]["statuscode"].as_i64();
        if status_code != Some(100) && status_code != Some(200) {
            return Err(anyhow!(
                "Nextcloud share_link rejected request: {}",
                body["ocs"]["meta"]["message"]
                    .as_str()
                    .unwrap_or("unknown error")
            ));
        }
        Ok(body["ocs"]["data"]["url"].as_str().map(str::to_owned))
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let url = self.url_for(path);
        let resp = self
            .req(reqwest::Method::GET, &url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("WebDAV GET {url} → {status}: {body}"));
        }
        Ok(resp.bytes()?.to_vec())
    }

    fn read_file_to_writer(&self, path: &Path, writer: &mut dyn Write) -> Result<u64> {
        let url = self.url_for(path);
        let mut response = self
            .req(reqwest::Method::GET, &url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("WebDAV GET {url} → {status}: {body}"));
        }
        std::io::copy(&mut response, writer).with_context(|| format!("streaming GET {url}"))
    }

    /// Read an inclusive byte range from a WebDAV resource.
    fn read_range(&self, path: &Path, start: u64, end: u64) -> Result<Vec<u8>> {
        if end < start {
            return Err(anyhow!("invalid WebDAV byte range {start}..={end}"));
        }
        let url = self.url_for(path);
        let range = format!("bytes={start}-{end}");
        let response = self
            .req(reqwest::Method::GET, &url)
            .header("Range", &range)
            .send()
            .with_context(|| format!("GET range {url} ({range})"))?;
        let status = response.status();
        if status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(anyhow!(
                "WebDAV range GET {url} → {status}; server did not honor Range"
            ));
        }
        Ok(response.bytes()?.to_vec())
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        // Servers like Nextcloud need the parent collection to exist, so
        // walk the prefix and MKCOL each missing segment.  Idempotent —
        // a 405 (Method Not Allowed) on MKCOL means it already exists.
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.ensure_collection(parent)?;
        }
        let url = self.url_for(path);
        let resp = self
            .req(reqwest::Method::PUT, &url)
            .body(data.to_vec())
            .send()
            .with_context(|| format!("PUT {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("WebDAV PUT {url} → {status}: {body}"));
        }
        Ok(())
    }

    fn write_file_from_reader(
        &self,
        path: &Path,
        reader: &mut dyn Read,
        size: u64,
    ) -> Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.ensure_collection(parent)?;
        }
        // reqwest's blocking streaming body requires an owned 'static reader.
        // Stage to an anonymous temporary file so memory stays bounded while
        // preserving the exact-size contract required by CloudDrive.
        let mut staged = tempfile::NamedTempFile::new().context("creating WebDAV staging file")?;
        // Do the bounded copy explicitly; the temporary file is the only
        // storage and is removed automatically on every return path.
        let mut source = reader.take(size);
        let copied = std::io::copy(&mut source, &mut staged)
            .context("staging WebDAV upload")?;
        anyhow::ensure!(copied == size, "reader ended before declared size");
        let mut extra = [0u8; 1];
        anyhow::ensure!(reader.read(&mut extra)? == 0, "reader has data beyond declared size");
        let file = File::open(staged.path()).context("opening WebDAV staging file")?;
        let url = self.url_for(path);
        let response = self
            .req(reqwest::Method::PUT, &url)
            .body(reqwest::blocking::Body::sized(file, size))
            .send()
            .with_context(|| format!("PUT {url}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("WebDAV PUT {url} → {status}: {body}"));
        }
        Ok(())
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        self.ensure_collection(path)
    }

    fn move_path(&self, source: &Path, destination: &Path) -> Result<()> {
        if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.ensure_collection(parent)?;
        }
        let source_url = self.url_for(source);
        let destination_url = self.url_for(destination);
        let response = self
            .req(reqwest::Method::from_bytes(b"MOVE").unwrap(), &source_url)
            .header("Destination", destination_url.as_str())
            .header("Overwrite", "T")
            .send()
            .with_context(|| format!("MOVE {source_url} -> {destination_url}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(anyhow!(
                "WebDAV MOVE {source_url} -> {destination_url} → {status}: {body}"
            ));
        }
        Ok(())
    }

    fn copy_path(&self, source: &Path, destination: &Path) -> Result<()> {
        if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.ensure_collection(parent)?;
        }
        let source_url = self.url_for(source);
        let destination_url = self.url_for(destination);
        let response = self
            .req(reqwest::Method::from_bytes(b"COPY").unwrap(), &source_url)
            .header("Destination", destination_url.as_str())
            .header("Overwrite", "F")
            .send()
            .with_context(|| format!("COPY {source_url} -> {destination_url}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(anyhow!(
                "WebDAV COPY {source_url} -> {destination_url} → {status}: {body}"
            ));
        }
        Ok(())
    }

    fn delete(&self, path: &Path) -> Result<()> {
        let url = self.url_for(path);
        let resp = self
            .req(reqwest::Method::DELETE, &url)
            .send()
            .with_context(|| format!("DELETE {url}"))?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 404 {
            let body = resp.text().unwrap_or_default();
            return Err(anyhow!("WebDAV DELETE {url} → {status}: {body}"));
        }
        Ok(())
    }
}

impl WebDavDrive {
    /// Walk the path's prefix and MKCOL each missing segment.
    fn ensure_collection(&self, dir: &Path) -> Result<()> {
        let mut acc = std::path::PathBuf::new();
        for comp in dir.components() {
            let s = comp.as_os_str().to_string_lossy();
            // Skip the leading "/" component when the path is absolute.
            if s == "/" || s.is_empty() {
                continue;
            }
            acc.push(s.as_ref());
            let url = self.url_for(&acc);
            let resp = self
                .req(reqwest::Method::from_bytes(b"MKCOL").unwrap(), &url)
                .send()
                .with_context(|| format!("MKCOL {url}"))?;
            let code = resp.status().as_u16();
            // 201 = created.  405/409 = already exists / parent missing.
            // Treat 405 as success; 409 means a previous segment didn't
            // come up — surface the error.
            if !(code == 201 || code == 405 || resp.status().is_success()) {
                let body = resp.text().unwrap_or_default();
                return Err(anyhow!("MKCOL {url} → {code}: {body}"));
            }
        }
        Ok(())
    }
}

/// Strip scheme+host from an href and percent-decode it for comparison.
fn strip_origin_and_decode(s: &str) -> String {
    // Find "://" and skip past the host.
    let after_scheme = if let Some(idx) = s.find("://") {
        let rest = &s[idx + 3..];
        rest.find('/').map(|p| &rest[p..]).unwrap_or("")
    } else {
        s
    };
    percent_decode(after_scheme)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let h = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16);
            if let Ok(b) = h {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Pick a display name for a `<D:response>`: prefer `<D:displayname>` if
/// present, else the last segment of the href.
fn name_from_response(r: &DavResponse) -> String {
    if let Some(d) = &r.displayname {
        return d.clone();
    }
    let href = strip_origin_and_decode(&r.href);
    let trimmed = href.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::{Matcher, Server};

    #[test]
    fn drive_metadata_correct() {
        let d = WebDavDrive::new("dav", "https://example.com/dav/", None, None, false);
        assert_eq!(d.label(), "dav");
        assert_eq!(d.drive_type(), DriveType::WebDav);
        let capabilities = d.capabilities();
        assert!(capabilities.create_dir);
        assert!(capabilities.rename);
        assert!(capabilities.move_path);
        assert!(capabilities.copy);
        assert!(capabilities.streaming);
    }

    #[test]
    fn streaming_read_and_bounded_write_use_webdav_wire() {
        let mut server = Server::new();
        let get = server
            .mock("GET", "/dav/read.txt")
            .with_status(200)
            .with_body("streamed response")
            .create();
        let put = server
            .mock("PUT", "/dav/write.txt")
            .match_body("streamed request")
            .with_status(201)
            .create();
        let drive = WebDavDrive::new("d", format!("{}/dav/", server.url()), None, None, false);
        let mut output = Vec::new();
        assert_eq!(
            drive.read_file_to_writer(Path::new("read.txt"), &mut output).unwrap(),
            17
        );
        assert_eq!(output, b"streamed response");
        let mut input = &b"streamed request"[..];
        drive
            .write_file_from_reader(Path::new("write.txt"), &mut input, 16)
            .unwrap();
        get.assert();
        put.assert();
    }

    #[test]
    fn range_read_requires_partial_content() {
        let mut server = Server::new();
        let mock = server
            .mock("GET", "/dav/file.bin")
            .match_header("Range", "bytes=4-7")
            .with_status(206)
            .with_body("part")
            .create();
        let drive = WebDavDrive::new("d", format!("{}/dav/", server.url()), None, None, false);
        assert_eq!(
            drive.read_range(Path::new("file.bin"), 4, 7).unwrap(),
            b"part"
        );
        mock.assert();
    }

    #[test]
    fn range_read_rejects_servers_that_ignore_range() {
        let mut server = Server::new();
        let mock = server
            .mock("GET", "/dav/file.bin")
            .with_status(200)
            .with_body("whole file")
            .create();
        let drive = WebDavDrive::new("d", format!("{}/dav/", server.url()), None, None, false);
        let error = drive.read_range(Path::new("file.bin"), 0, 3).unwrap_err();
        assert!(error.to_string().contains("did not honor Range"));
        mock.assert();
    }

    #[test]
    fn crispcloud_delta_app_is_detected_and_blockmap_is_decoded() {
        let mut server = Server::new();
        let status = server
            .mock("GET", "/index.php/apps/crispcloud_delta/api/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"app":"crispcloud_delta","version":"0.1.0","blockSize":4194304,"algorithm":"adler32+sha256"}"#,
            )
            .expect(2)
            .create();
        let blockmap = server
            .mock(
                "GET",
                "/index.php/apps/crispcloud_delta/api/blockmap/file.bin",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"filePath":"/file.bin","totalSize":4,"blockSize":4194304,"blockCount":1,"signatures":[{"blockIndex":0,"offset":0,"size":4,"weakHash":67371529,"strongHash":"9f64a747e1b97f131fabb6b447296c9b6f020c3f1e8f1c1d5d6f6b6f4e9f2f0a"}],"etag":"etag-1"}"#,
            )
            .create();
        let drive = WebDavDrive::new(
            "d",
            format!("{}/remote.php/dav/files/alice/", server.url()),
            Some("alice".into()),
            Some("pw".into()),
            false,
        );
        assert!(drive.delta_sync_available().unwrap());
        let map = drive.fetch_delta_map(Path::new("/file.bin")).unwrap();
        assert_eq!(map.unwrap().etag.as_deref(), Some("etag-1"));
        status.assert();
        blockmap.assert();
    }

    #[test]
    fn delta_upload_falls_back_for_plain_webdav() {
        let server = Server::new();
        let drive = WebDavDrive::new("d", format!("{}/dav/", server.url()), None, None, false);
        let result = drive
            .delta_upload_file(Path::new("missing-local.bin"), Path::new("remote.bin"))
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delta_upload_sends_blockmap_etag_as_if_match() {
        let mut server = Server::new();
        let block_size = crate::sync::delta::DEFAULT_BLOCK_SIZE;
        let old = vec![b'a'; block_size];
        let mut local = old.clone();
        local[0] = b'b';
        let map = crate::sync::delta::compute_blockmap_from_bytes(&old, block_size).unwrap();
        let signature = &map.blocks[0];
        let blockmap = serde_json::json!({
            "filePath": "/file.bin",
            "totalSize": block_size,
            "blockSize": block_size,
            "blockCount": 1,
            "signatures": [{
                "blockIndex": 0,
                "offset": signature.offset,
                "size": signature.size,
                "weakHash": signature.weak_hash,
                "strongHash": hex::encode(signature.strong_hash),
            }],
            "etag": "etag-1",
        });
        let status = server
            .mock("GET", "/index.php/apps/crispcloud_delta/api/status")
            .with_status(200)
            .with_body(
                r#"{"app":"crispcloud_delta","blockSize":4194304,"algorithm":"adler32+sha256"}"#,
            )
            .create();
        let map_mock = server
            .mock(
                "GET",
                "/index.php/apps/crispcloud_delta/api/blockmap/file.bin",
            )
            .with_status(200)
            .with_body(blockmap.to_string())
            .create();
        let block_mock = server
            .mock(
                "POST",
                "/index.php/apps/crispcloud_delta/api/blocks/file.bin",
            )
            .match_query(Matcher::Any)
            .match_header("If-Match", "\"etag-1\"")
            .with_status(200)
            .create();
        let finalize_mock = server
            .mock(
                "POST",
                "/index.php/apps/crispcloud_delta/api/finalize/file.bin",
            )
            .match_query(Matcher::Any)
            .match_header("If-Match", "\"etag-1\"")
            .with_status(200)
            .create();
        let local_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(local_file.path(), &local).unwrap();
        let drive = WebDavDrive::new(
            "d",
            format!("{}/remote.php/dav/files/alice/", server.url()),
            Some("alice".into()),
            Some("pw".into()),
            false,
        );

        let result = drive
            .delta_upload_file(local_file.path(), Path::new("file.bin"))
            .unwrap()
            .unwrap();
        assert_eq!(result.changed_blocks, 1);
        status.assert();
        map_mock.assert();
        block_mock.assert();
        finalize_mock.assert();
    }

    #[test]
    fn mutation_methods_use_webdav_destination_headers() {
        let mut server = Server::new();
        let moved_destination = format!("{}/dav/moved.txt", server.url());
        let copied_destination = format!("{}/dav/copied.txt", server.url());
        let move_mock = server
            .mock("MOVE", "/dav/source.txt")
            .match_header("Destination", moved_destination.as_str())
            .match_header("Overwrite", "T")
            .with_status(201)
            .create();
        let copy_mock = server
            .mock("COPY", "/dav/source.txt")
            .match_header("Destination", copied_destination.as_str())
            .match_header("Overwrite", "F")
            .with_status(201)
            .create();
        let drive = WebDavDrive::new(
            "d",
            format!("{}/dav/", server.url()),
            Some("alice".into()),
            Some("pw".into()),
            false,
        );

        drive
            .move_path(Path::new("source.txt"), Path::new("moved.txt"))
            .unwrap();
        drive
            .copy_path(Path::new("source.txt"), Path::new("copied.txt"))
            .unwrap();
        move_mock.assert();
        copy_mock.assert();
    }

    #[test]
    fn create_dir_walks_each_missing_collection() {
        let mut server = Server::new();
        let first = server.mock("MKCOL", "/dav/one").with_status(201).create();
        let second = server
            .mock("MKCOL", "/dav/one/two")
            .with_status(201)
            .create();
        let drive = WebDavDrive::new("d", format!("{}/dav/", server.url()), None, None, false);

        drive.create_dir(Path::new("one/two")).unwrap();
        first.assert();
        second.assert();
    }

    #[test]
    fn url_for_normalises_leading_slash_and_encodes() {
        let d = WebDavDrive::new("d", "https://h/dav/", None, None, false);
        assert_eq!(d.url_for(Path::new("/a/b.txt")), "https://h/dav/a/b.txt");
        assert_eq!(d.url_for(Path::new("a/b.txt")), "https://h/dav/a/b.txt");
        assert_eq!(d.url_for(Path::new("hi there")), "https://h/dav/hi%20there");
        assert_eq!(
            d.url_for(Path::new("a/b c/d.pdf")),
            "https://h/dav/a/b%20c/d.pdf"
        );
        assert_eq!(d.url_for(Path::new("a&b")), "https://h/dav/a%26b");
    }

    #[test]
    fn url_for_handles_unicode() {
        let d = WebDavDrive::new("d", "https://h/dav/", None, None, false);
        // ü = U+00FC = UTF-8 bytes 0xC3 0xBC
        assert_eq!(
            d.url_for(Path::new("\u{00FC}.txt")),
            "https://h/dav/%C3%BC.txt"
        );
    }

    #[test]
    fn nextcloud_share_detection_preserves_host_and_port() {
        let d = WebDavDrive::new(
            "d",
            "http://localhost:8080/remote.php/dav/files/alice/",
            Some("alice".into()),
            Some("pw".into()),
            true,
        );
        assert_eq!(
            d.nextcloud_ocs_url().as_deref(),
            Some("http://localhost:8080/ocs/v2.php/apps/files_sharing/api/v1/shares?format=json")
        );
    }

    #[test]
    fn non_nextcloud_webdav_does_not_probe_ocs() {
        let d = WebDavDrive::new("d", "https://dav.example.test/files/", None, None, false);
        assert!(d.nextcloud_ocs_url().is_none());
        assert!(d.share_link(Path::new("report.pdf")).unwrap().is_none());
    }

    #[test]
    fn constructor_uses_shared_proxy_validation() {
        let invalid = ProxyConfig {
            url: Some("not a proxy URL".into()),
            ..Default::default()
        };
        assert!(WebDavDrive::new_with_proxy(
            "d",
            "https://dav.example.test/dav/",
            None,
            None,
            false,
            &invalid,
        )
        .is_err());

        let valid = ProxyConfig {
            url: Some("socks5://127.0.0.1:9050".into()),
            ..Default::default()
        };
        assert!(WebDavDrive::new_with_proxy(
            "d",
            "https://dav.example.test/dav/",
            None,
            None,
            false,
            &valid,
        )
        .is_ok());
    }

    #[test]
    fn nextcloud_share_link_posts_ocs_form_and_returns_url() {
        let mut server = Server::new();
        let endpoint = "/ocs/v2.php/apps/files_sharing/api/v1/shares";
        let mock = server
            .mock("POST", endpoint)
            .match_query(Matcher::Any)
            .match_header("OCS-APIRequest", "true")
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded("path".into(), "/report.pdf".into()),
                Matcher::UrlEncoded("shareType".into(), "3".into()),
                Matcher::UrlEncoded("permissions".into(), "1".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"ocs":{"meta":{"status":"ok","statuscode":100},"data":{"url":"https://cloud.example/s/abc"}}}"#,
            )
            .create();
        let drive = WebDavDrive::new(
            "d",
            format!("{}/remote.php/dav/files/alice/", server.url()),
            Some("alice".into()),
            Some("pw".into()),
            false,
        );
        assert_eq!(
            drive
                .share_link(Path::new("report.pdf"))
                .unwrap()
                .as_deref(),
            Some("https://cloud.example/s/abc")
        );
        mock.assert();
    }

    #[test]
    fn probed_capabilities_enable_ocs_sharing_only_when_advertised() {
        let mut server = Server::new();
        let endpoint = "/ocs/v2.php/apps/files_sharing/api/v1/shares";
        let mock = server
            .mock("GET", endpoint)
            .match_query(Matcher::UrlEncoded("format".into(), "json".into()))
            .match_header("OCS-APIRequest", "true")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"ocs":{"meta":{"status":"ok","statuscode":100}}}"#)
            .create();
        let drive = WebDavDrive::new(
            "d",
            format!("{}/remote.php/dav/files/alice/", server.url()),
            Some("alice".into()),
            Some("pw".into()),
            false,
        );

        let capabilities = drive.probed_capabilities();
        assert!(capabilities.share_links);
        assert!(!capabilities.versions);
        mock.assert();
    }

    #[test]
    fn probed_capabilities_keep_sharing_disabled_when_ocs_rejects() {
        let mut server = Server::new();
        let endpoint = "/ocs/v2.php/apps/files_sharing/api/v1/shares";
        let mock = server
            .mock("GET", endpoint)
            .match_query(Matcher::UrlEncoded("format".into(), "json".into()))
            .match_header("OCS-APIRequest", "true")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"ocs":{"meta":{"status":"failure","statuscode":403}}}"#)
            .create();
        let drive = WebDavDrive::new(
            "d",
            format!("{}/remote.php/dav/files/alice/", server.url()),
            Some("alice".into()),
            Some("pw".into()),
            false,
        );

        let capabilities = drive.probed_capabilities();
        assert!(!capabilities.share_links);
        mock.assert();
    }

    #[test]
    fn percent_decode_round_trips_basic_chars() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(
            percent_decode("/Photos/2024%20holiday"),
            "/Photos/2024 holiday"
        );
        assert_eq!(percent_decode("noop"), "noop");
    }

    #[test]
    fn strip_origin_and_decode_extracts_path() {
        assert_eq!(
            strip_origin_and_decode("https://h/dav/foo%20bar"),
            "/dav/foo bar"
        );
        assert_eq!(strip_origin_and_decode("/dav/foo"), "/dav/foo");
    }

    #[test]
    fn parse_http_date_imf_fixdate() {
        // "Wed, 21 Oct 2015 07:28:00 GMT" → 1445412480
        assert_eq!(
            parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480)
        );
        // "Sun, 06 Nov 1994 08:49:37 GMT" → 784_111_777 (RFC 7231 example)
        assert_eq!(
            parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
    }

    #[test]
    fn parse_http_date_rejects_garbage() {
        assert!(parse_http_date("never").is_none());
        assert!(parse_http_date("").is_none());
        assert!(parse_http_date("Wed, 21 Foo 2015 07:28:00 GMT").is_none());
    }

    #[test]
    fn parse_propfind_handles_nextcloud_response() {
        // Real-world-ish multi-status from Nextcloud (trimmed).
        let xml = r#"<?xml version="1.0"?>
        <d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:oc="http://owncloud.org/ns">
          <d:response>
            <d:href>/remote.php/dav/files/alice/</d:href>
            <d:propstat>
              <d:prop>
                <d:displayname>alice</d:displayname>
                <d:getlastmodified>Wed, 21 Oct 2015 07:28:00 GMT</d:getlastmodified>
                <d:resourcetype><d:collection/></d:resourcetype>
              </d:prop>
              <d:status>HTTP/1.1 200 OK</d:status>
            </d:propstat>
          </d:response>
          <d:response>
            <d:href>/remote.php/dav/files/alice/Photos/</d:href>
            <d:propstat>
              <d:prop>
                <d:displayname>Photos</d:displayname>
                <d:resourcetype><d:collection/></d:resourcetype>
              </d:prop>
              <d:status>HTTP/1.1 200 OK</d:status>
            </d:propstat>
          </d:response>
          <d:response>
            <d:href>/remote.php/dav/files/alice/note.txt</d:href>
            <d:propstat>
              <d:prop>
                <d:displayname>note.txt</d:displayname>
                <d:getcontentlength>1234</d:getcontentlength>
                <d:getlastmodified>Sun, 06 Nov 1994 08:49:37 GMT</d:getlastmodified>
                <d:resourcetype/>
              </d:prop>
              <d:status>HTTP/1.1 200 OK</d:status>
            </d:propstat>
          </d:response>
        </d:multistatus>"#;
        let parsed = parse_propfind(xml).unwrap();
        assert_eq!(parsed.len(), 3);
        // The first response is the directory itself.
        assert!(parsed[0].is_collection);
        // Photos folder
        assert_eq!(parsed[1].displayname.as_deref(), Some("Photos"));
        assert!(parsed[1].is_collection);
        // File entry
        assert_eq!(parsed[2].displayname.as_deref(), Some("note.txt"));
        assert!(!parsed[2].is_collection);
        assert_eq!(parsed[2].contentlength, Some(1234));
        assert_eq!(
            parsed[2].lastmodified.as_deref(),
            Some("Sun, 06 Nov 1994 08:49:37 GMT")
        );
    }

    #[test]
    fn parse_propfind_handles_unprefixed_dav_namespace() {
        // Some servers (e.g. Synology) drop the `D:` prefix.  We match on
        // the local-name only so this should still parse.
        let xml = r#"<?xml version="1.0"?>
        <multistatus xmlns="DAV:">
          <response>
            <href>/dav/file.bin</href>
            <propstat>
              <prop>
                <getcontentlength>42</getcontentlength>
                <displayname>file.bin</displayname>
                <resourcetype/>
              </prop>
            </propstat>
          </response>
        </multistatus>"#;
        let parsed = parse_propfind(xml).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].displayname.as_deref(), Some("file.bin"));
        assert_eq!(parsed[0].contentlength, Some(42));
        assert!(!parsed[0].is_collection);
    }

    #[test]
    fn name_from_response_falls_back_to_href_basename() {
        // Servers that don't return <displayname>: derive name from href.
        let r = DavResponse {
            href: "/dav/path%20with%20spaces/file.pdf".into(),
            displayname: None,
            is_collection: false,
            contentlength: Some(1),
            lastmodified: None,
        };
        assert_eq!(name_from_response(&r), "file.pdf");
    }

    // ── Live integration tests ─────────────────────────────────────────────
    //
    // Gated by `#[ignore]` so the normal `cargo test` run stays offline.
    // Run with:
    //   WEBDAV_TEST_URL=http://localhost:8088/ \
    //   WEBDAV_TEST_USER=filen \
    //   WEBDAV_TEST_PASS=filen-webdav \
    //   cargo test -p crispsorter --lib -- --ignored webdav_live --nocapture
    //
    // For Internxt's HTTPS server with self-signed cert:
    //   WEBDAV_TEST_URL=https://127.0.0.1:9999/ \
    //   WEBDAV_TEST_USER=internxt \
    //   WEBDAV_TEST_PASS=<from `internxt webdav-config`> \
    //   WEBDAV_TEST_INSECURE=1 \
    //   cargo test -p crispsorter --lib -- --ignored webdav_live --nocapture
    //
    // Spin up the server first with the matching CLI's `webdav-start -b`.

    fn live_drive() -> Option<WebDavDrive> {
        let url = std::env::var("WEBDAV_TEST_URL").ok()?;
        let user = std::env::var("WEBDAV_TEST_USER").ok();
        let pass = std::env::var("WEBDAV_TEST_PASS").ok();
        let insecure = std::env::var("WEBDAV_TEST_INSECURE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        Some(WebDavDrive::new("live-test", url, user, pass, insecure))
    }

    fn live_delta_drive(prefix: &str) -> Option<WebDavDrive> {
        let url = std::env::var(format!("{prefix}_URL")).ok()?;
        let user = std::env::var(format!("{prefix}_USER")).ok()?;
        let pass = std::env::var(format!("{prefix}_PASS")).ok()?;
        Some(WebDavDrive::new(
            prefix,
            url,
            Some(user),
            Some(pass),
            std::env::var(format!("{prefix}_INSECURE"))
                .map(|v| !v.is_empty() && v != "0")
                .unwrap_or(false),
        ))
    }

    fn run_live_delta_roundtrip(prefix: &str) {
        let Some(drive) = live_delta_drive(prefix) else {
            eprintln!("skip: {prefix}_URL, {prefix}_USER, and {prefix}_PASS must be set");
            return;
        };

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let test_tag = prefix.to_ascii_lowercase();
        let remote_path = std::path::PathBuf::from(format!("/_{test_tag}_delta_{nonce}.bin"));
        let local_path = std::env::temp_dir().join(format!("{test_tag}-delta-{nonce}.bin"));
        let old_path = std::env::temp_dir().join(format!("{test_tag}-delta-old-{nonce}.bin"));

        let mut content = vec![b'a'; 2 * crate::sync::delta::DEFAULT_BLOCK_SIZE];
        content[crate::sync::delta::DEFAULT_BLOCK_SIZE + 17] = b'b';
        std::fs::write(&local_path, &content).expect("write live delta fixture");
        drive
            .write_file(&remote_path, &content)
            .expect("initial live delta upload failed");
        std::fs::write(&old_path, &content).expect("write old live delta fixture");

        content[17] = b'c';
        std::fs::write(&local_path, &content).expect("write changed live delta fixture");
        let upload = drive
            .delta_upload_file(&local_path, &remote_path)
            .expect("live delta upload failed")
            .expect("crispcloud_delta app was not detected");
        assert_eq!(upload.total_blocks, 2);
        assert_eq!(upload.changed_blocks, 1);
        assert_eq!(
            upload.transferred_bytes,
            crate::sync::delta::DEFAULT_BLOCK_SIZE as u64
        );

        let remote = drive
            .read_file(&remote_path)
            .expect("read live delta result");
        assert_eq!(remote, content);

        let download = drive
            .delta_download_file(&remote_path, &old_path)
            .expect("live delta download failed")
            .expect("delta download unexpectedly fell back");
        assert_eq!(download.changed_blocks, 1);
        assert_eq!(
            std::fs::read(&old_path).expect("read patched live file"),
            content
        );

        // Exercise server-side finalize for a shrinking file, then grow it
        // again so the remote-only blocks are also covered.
        content = vec![b's'; crate::sync::delta::DEFAULT_BLOCK_SIZE / 2];
        std::fs::write(&local_path, &content).expect("write shrinking delta fixture");
        let shrink = drive
            .delta_upload_file(&local_path, &remote_path)
            .expect("live shrink upload failed")
            .expect("delta shrink unexpectedly fell back");
        assert_eq!(
            shrink.total_bytes,
            (crate::sync::delta::DEFAULT_BLOCK_SIZE / 2) as u64
        );
        assert_eq!(drive.read_file(&remote_path).unwrap(), content);

        content = vec![b'g'; 2 * crate::sync::delta::DEFAULT_BLOCK_SIZE + 123];
        std::fs::write(&local_path, &content).expect("write growing delta fixture");
        let grow = drive
            .delta_upload_file(&local_path, &remote_path)
            .expect("live grow upload failed")
            .expect("delta grow unexpectedly fell back");
        assert_eq!(grow.total_bytes, content.len() as u64);
        assert_eq!(drive.read_file(&remote_path).unwrap(), content);

        let _ = drive.delete(&remote_path);
        let _ = std::fs::remove_file(local_path);
        let _ = std::fs::remove_file(old_path);
    }

    fn run_live_share_link(prefix: &str) {
        let Some(drive) = live_delta_drive(prefix) else {
            eprintln!("skip: {prefix}_URL, {prefix}_USER, and {prefix}_PASS must be set");
            return;
        };
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let remote_path = std::path::PathBuf::from(format!("/_{prefix}_share_{nonce}.txt"));
        drive
            .write_file(&remote_path, b"CrispSorter OCS share-link live test")
            .expect("write live share fixture failed");
        let result = drive
            .share_link(&remote_path)
            .expect("live OCS share request failed")
            .expect("Nextcloud/ownCloud did not return a public share URL");
        assert!(result.starts_with("http://") || result.starts_with("https://"));
        let _ = drive.delete(&remote_path);
    }

    #[test]
    #[ignore]
    fn webdav_live_delta_nextcloud() {
        run_live_delta_roundtrip("CRISPSORTER_NEXTCLOUD_DELTA");
    }

    #[test]
    #[ignore]
    fn webdav_live_delta_owncloud() {
        run_live_delta_roundtrip("CRISPSORTER_OWNCLOUD_DELTA");
    }

    #[test]
    #[ignore]
    fn webdav_live_share_link_nextcloud() {
        run_live_share_link("CRISPSORTER_NEXTCLOUD_DELTA");
    }

    #[test]
    #[ignore]
    fn webdav_live_share_link_owncloud() {
        run_live_share_link("CRISPSORTER_OWNCLOUD_DELTA");
    }

    #[test]
    #[ignore]
    fn webdav_live_list_root() {
        let Some(drive) = live_drive() else {
            eprintln!("skip: WEBDAV_TEST_URL not set");
            return;
        };
        let entries = drive
            .list_dir(Path::new("/"))
            .expect("PROPFIND root failed");
        eprintln!("--- root listing ({} entries) ---", entries.len());
        for e in entries.iter().take(20) {
            eprintln!(
                "  {} {} {}",
                if e.is_dir { "DIR " } else { "FILE" },
                e.size
                    .map(|s| format!("{:>10}", s))
                    .unwrap_or_else(|| " ".repeat(10)),
                e.name
            );
        }
        // We don't assert anything about contents (varies by account); we
        // just want to know PROPFIND parses without panicking.
        assert!(!entries.is_empty() || true, "tolerate empty drive");
    }

    #[test]
    #[ignore]
    fn webdav_live_write_read_delete_roundtrip() {
        let Some(drive) = live_drive() else {
            eprintln!("skip: WEBDAV_TEST_URL not set");
            return;
        };

        // Use a fresh path each time so reruns don't collide.
        let nonce: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        let test_path = format!("/_crispsorter_webdav_test_{nonce}.txt");
        let content = format!("hello from CrispSorter test at {nonce}").into_bytes();

        eprintln!("PUT  {test_path}");
        drive
            .write_file(Path::new(&test_path), &content)
            .expect("write_file failed");

        eprintln!("STAT {test_path}");
        let stat = drive.stat(Path::new(&test_path)).expect("stat failed");
        assert!(!stat.is_dir, "test file must not be reported as a dir");
        assert_eq!(
            stat.size,
            content.len() as u64,
            "stat size mismatch (got {}, want {})",
            stat.size,
            content.len()
        );

        eprintln!("GET  {test_path}");
        let bytes = drive
            .read_file(Path::new(&test_path))
            .expect("read_file failed");
        assert_eq!(bytes, content, "round-trip content mismatch");

        eprintln!("DEL  {test_path}");
        match drive.delete(Path::new(&test_path)) {
            Ok(_) => {
                // Standards-compliant server: stat after delete must 404.
                let post = drive.stat(Path::new(&test_path));
                assert!(
                    post.is_err(),
                    "stat should fail after DELETE; got {:?}",
                    post
                );
                eprintln!("OK: full write→stat→read→delete round-trip succeeded");
            }
            Err(e) => {
                // Filen's wsgidav-backed server (4.3.3) returns
                // 500 "Resource could not be deleted" on DELETE even via
                // curl, despite PUT/PROPFIND/GET working perfectly.  Treat
                // as a known server-side limitation: warn but don't fail.
                // The user can clean up via `filen rm` if needed.
                eprintln!(
                    "warning: DELETE failed (server quirk?) — leaving {} on the server: {:#}",
                    test_path, e
                );
                eprintln!("OK: write→stat→read succeeded; delete is server-dependent");
            }
        }
    }
}
