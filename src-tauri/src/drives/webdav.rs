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
use std::path::Path;
use std::time::Duration;

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

impl WebDavDrive {
    pub fn new(
        label: impl Into<String>,
        base_url: impl Into<String>,
        username: Option<String>,
        password: Option<String>,
        insecure_tls: bool,
    ) -> Self {
        let mut url = base_url.into();
        if !url.ends_with('/') {
            url.push('/');
        }
        let mut builder = reqwest::blocking::Client::builder()
            // Generous default — WebDAV servers can be slow on cold dirs.
            .timeout(Duration::from_secs(60));
        if insecure_tls {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let client = builder.build().expect("reqwest blocking client must build");
        Self {
            label: label.into(),
            base_url: url,
            username,
            password,
            client,
        }
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
        url.set_query(None);
        url.set_fragment(None);
        Some(url.to_string())
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
            ..DriveCapabilities::basic()
        }
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
        if body["ocs"]["meta"]["statuscode"].as_i64() != Some(100) {
            return Err(anyhow!(
                "Nextcloud share_link rejected request: {}",
                body["ocs"]["meta"]["message"].as_str().unwrap_or("unknown error")
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
        assert!(!capabilities.streaming);
    }

    #[test]
    fn mutation_methods_use_webdav_destination_headers() {
        let mut server = Server::new();
        let move_mock = server
            .mock("MOVE", "/dav/source.txt")
            .match_header("Destination", &format!("{}/dav/moved.txt", server.url()))
            .match_header("Overwrite", "T")
            .with_status(201)
            .create();
        let copy_mock = server
            .mock("COPY", "/dav/source.txt")
            .match_header("Destination", &format!("{}/dav/copied.txt", server.url()))
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
            Some("http://localhost:8080/ocs/v2.php/apps/files_sharing/api/v1/shares")
        );
    }

    #[test]
    fn non_nextcloud_webdav_does_not_probe_ocs() {
        let d = WebDavDrive::new("d", "https://dav.example.test/files/", None, None, false);
        assert!(d.nextcloud_ocs_url().is_none());
        assert!(d.share_link(Path::new("report.pdf")).unwrap().is_none());
    }

    #[test]
    fn nextcloud_share_link_posts_ocs_form_and_returns_url() {
        let mut server = Server::new();
        let endpoint = "/ocs/v2.php/apps/files_sharing/api/v1/shares";
        let mock = server
            .mock("POST", endpoint)
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
            drive.share_link(Path::new("report.pdf")).unwrap().as_deref(),
            Some("https://cloud.example/s/abc")
        );
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
