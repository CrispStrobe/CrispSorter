//! OneDrive / SharePoint cloud drive via Microsoft Graph API (P27.11).
//!
//! Implements `CloudDrive` using `reqwest::blocking` HTTP calls to the
//! Microsoft Graph v1.0 REST API.  Auth: OAuth2 access token passed in
//! at construction (the token refresh flow is handled by the frontend
//! or CLI before instantiation).
//!
//! Paths are relative to the user's OneDrive root (`/me/drive/root`).
//! For SharePoint, the caller configures a different Graph drive URL.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use super::{CloudDrive, DirEntry, DriveCapabilities, DriveType, FileStat, FileVersion};

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

pub struct OneDriveDrive {
    label: String,
    graph_base: String,
    access_token: String,
    _refresh_token: Option<String>,
    _client_id: Option<String>,
    _client_secret: Option<String>,
    client: reqwest::blocking::Client,
}

impl OneDriveDrive {
    pub fn new(
        label: String,
        access_token: String,
        refresh_token: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Self {
        Self::with_graph_base(
            label,
            access_token,
            refresh_token,
            client_id,
            client_secret,
            GRAPH_BASE,
        )
    }

    fn with_graph_base(
        label: String,
        access_token: String,
        refresh_token: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        graph_base: impl Into<String>,
    ) -> Self {
        Self {
            label,
            graph_base: graph_base.into(),
            access_token,
            _refresh_token: refresh_token,
            _client_id: client_id,
            _client_secret: client_secret,
            client: reqwest::blocking::Client::new(),
        }
    }

    fn graph_url(&self, path: &Path) -> String {
        let rel = path.to_string_lossy();
        if rel.is_empty() || rel == "." || rel == "/" {
            format!("{}/me/drive/root/children", self.graph_base)
        } else {
            // Encode path for Graph API — colons delimit the path segment
            let clean = rel.trim_start_matches('/');
            format!("{}/me/drive/root:/{}:/children", self.graph_base, clean)
        }
    }

    fn item_url(&self, path: &Path) -> String {
        let rel = path.to_string_lossy();
        let clean = rel.trim_start_matches('/');
        if clean.is_empty() || clean == "." {
            format!("{}/me/drive/root", self.graph_base)
        } else {
            format!("{}/me/drive/root:/{}", self.graph_base, clean)
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.access_token)
    }

    fn share_link_url(&self, path: &Path) -> String {
        format!("{}/createLink", self.item_url(path))
    }
}

impl CloudDrive for OneDriveDrive {
    fn label(&self) -> &str {
        &self.label
    }
    fn drive_type(&self) -> DriveType {
        DriveType::OneDrive
    }

    fn capabilities(&self) -> DriveCapabilities {
        DriveCapabilities {
            create_dir: true,
            rename: true,
            move_path: true,
            share_links: true,
            versions: true,
            ..DriveCapabilities::basic()
        }
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let url = self.graph_url(path);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .with_context(|| format!("OneDrive list_dir: {url}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!("OneDrive list_dir: HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp.json().context("OneDrive list_dir: parse JSON")?;
        let items = body["value"]
            .as_array()
            .ok_or_else(|| anyhow!("OneDrive: no 'value' array in response"))?;

        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            let name = item["name"].as_str().unwrap_or("").to_string();
            let is_dir = item.get("folder").is_some();
            let size = item["size"].as_u64();
            entries.push(DirEntry { name, is_dir, size });
        }
        Ok(entries)
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let url = format!("{}:/content", self.item_url(path));
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .with_context(|| format!("OneDrive read_file: {url}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!("OneDrive read_file: HTTP {}", resp.status()));
        }

        resp.bytes()
            .map(|b| b.to_vec())
            .context("OneDrive read_file: reading bytes")
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        let rel = path.to_string_lossy();
        let clean = rel.trim_start_matches('/');
        let url = format!("{}/me/drive/root:/{}:/content", self.graph_base, clean);

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/octet-stream")
            .body(data.to_vec())
            .send()
            .with_context(|| format!("OneDrive write_file: {url}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!("OneDrive write_file: HTTP {}", resp.status()));
        }
        Ok(())
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("OneDrive create_dir requires a directory name"))?;
        let parent_url = self.graph_url(path.parent().unwrap_or_else(|| Path::new("")));
        let response = self
            .client
            .post(&parent_url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({
                "name": name,
                "folder": {},
                "@microsoft.graph.conflictBehavior": "fail"
            }))
            .send()
            .with_context(|| format!("OneDrive create_dir: {}", path.display()))?;
        if !response.status().is_success() {
            return Err(anyhow!("OneDrive create_dir: HTTP {}", response.status()));
        }
        Ok(())
    }

    fn move_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let name = destination
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("OneDrive move requires a destination name"))?;
        let source_parent = source.parent().unwrap_or_else(|| Path::new(""));
        let destination_parent = destination.parent().unwrap_or_else(|| Path::new(""));
        let mut update = serde_json::json!({ "name": name });
        if source_parent != destination_parent {
            let clean = destination_parent
                .to_string_lossy()
                .trim_start_matches('/')
                .to_owned();
            let parent_path = if clean.is_empty() {
                "/drive/root:".to_owned()
            } else {
                format!("/drive/root:/{clean}")
            };
            update["parentReference"] = serde_json::json!({ "path": parent_path });
        }
        let url = self.item_url(source);
        let response = self
            .client
            .patch(&url)
            .header("Authorization", self.auth_header())
            .json(&update)
            .send()
            .with_context(|| {
                format!(
                    "OneDrive move: {} -> {}",
                    source.display(),
                    destination.display()
                )
            })?;
        if !response.status().is_success() {
            return Err(anyhow!("OneDrive move: HTTP {}", response.status()));
        }
        Ok(())
    }

    fn delete(&self, path: &Path) -> Result<()> {
        let url = self.item_url(path);
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .with_context(|| format!("OneDrive delete: {url}"))?;

        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            return Err(anyhow!("OneDrive delete: HTTP {}", resp.status()));
        }
        Ok(())
    }

    fn stat(&self, path: &Path) -> Result<FileStat> {
        let url = self.item_url(path);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .with_context(|| format!("OneDrive stat: {url}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!("OneDrive stat: HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp.json().context("OneDrive stat: parse JSON")?;

        let size = body["size"].as_u64().unwrap_or(0);
        let is_dir = body.get("folder").is_some();
        let mtime_unix = body["lastModifiedDateTime"].as_str().and_then(|s| {
            // ISO 8601 → epoch seconds (rough parse)
            chrono_parse_iso8601(s)
        });

        Ok(FileStat {
            size,
            is_dir,
            mtime_unix,
        })
    }

    fn share_link(&self, path: &Path) -> Result<Option<String>> {
        // Microsoft Graph creates an anonymous read-only sharing link in one
        // call.  The tenant may disallow anonymous links; Graph then returns
        // a precise 4xx error which we preserve for the caller.
        let url = self.share_link_url(path);
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({
                "type": "view",
                "scope": "anonymous"
            }))
            .send()
            .with_context(|| format!("OneDrive share_link: {url}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!("OneDrive share_link: HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp.json().context("OneDrive share_link: parse JSON")?;
        Ok(body["link"]["webUrl"].as_str().map(str::to_owned))
    }

    fn list_versions(&self, path: &Path) -> Result<Vec<FileVersion>> {
        // Graph API: GET /me/drive/root:/{path}:/versions
        let url = format!("{}:/versions", self.item_url(path));
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .with_context(|| format!("OneDrive list_versions: {url}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!("OneDrive list_versions: HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp.json().context("OneDrive list_versions: parse JSON")?;
        let items = body["value"]
            .as_array()
            .ok_or_else(|| anyhow!("OneDrive: no 'value' array in versions response"))?;

        let mut versions = Vec::with_capacity(items.len());
        for item in items {
            let id = item["id"].as_str().unwrap_or("").to_string();
            let modified_at = item["lastModifiedDateTime"]
                .as_str()
                .and_then(chrono_parse_iso8601);
            let size = item["size"].as_u64();
            let modifier_name = item["lastModifiedBy"]["user"]["displayName"]
                .as_str()
                .map(|s| s.to_string());
            versions.push(FileVersion {
                id,
                modified_at,
                size,
                modifier_name,
            });
        }
        Ok(versions)
    }

    fn restore_version(&self, path: &Path, version_id: &str) -> Result<()> {
        // Graph API: POST /me/drive/root:/{path}:/versions/{id}/restoreVersion
        let url = format!(
            "{}:/versions/{}/restoreVersion",
            self.item_url(path),
            version_id
        );
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .send()
            .with_context(|| format!("OneDrive restore_version: {url}"))?;

        if !resp.status().is_success() {
            return Err(anyhow!("OneDrive restore_version: HTTP {}", resp.status()));
        }
        Ok(())
    }
}

/// Rough ISO 8601 → epoch seconds parser for Graph API timestamps
/// like "2024-01-15T10:30:00Z".  No chrono dep — just manual parsing.
/// Public so `google_drive.rs` can reuse it.
pub fn chrono_parse_iso8601(s: &str) -> Option<i64> {
    // Format: YYYY-MM-DDThh:mm:ssZ (or with fractional seconds)
    let s = s.trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;

    let time = time.split('.').next()?; // strip fractional seconds
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let min: i64 = time_parts.next()?.parse().ok()?;
    let sec: i64 = time_parts.next()?.parse().ok()?;

    // Approximate days from epoch (no leap second precision needed)
    let mut days = 0i64;
    for y in 1970..year {
        days += if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
    }
    let month_days = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    for m in 1..month {
        days += month_days[m as usize];
        if m == 2 && is_leap {
            days += 1;
        }
    }
    days += day - 1;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    #[test]
    fn iso8601_parse() {
        let ts = chrono_parse_iso8601("2024-01-15T10:30:00Z").unwrap();
        // 2024-01-15 10:30:00 UTC should be around 1705312200
        assert!(ts > 1705000000 && ts < 1706000000, "got {ts}");
    }

    #[test]
    fn iso8601_with_fractional() {
        let ts = chrono_parse_iso8601("2024-01-15T10:30:00.123Z").unwrap();
        assert!(ts > 1705000000);
    }

    #[test]
    fn iso8601_invalid() {
        assert!(chrono_parse_iso8601("not-a-date").is_none());
        assert!(chrono_parse_iso8601("").is_none());
    }

    #[test]
    fn graph_url_root() {
        let d = OneDriveDrive::new("test".into(), "tok".into(), None, None, None);
        let url = d.graph_url(Path::new(""));
        assert!(url.contains("/root/children"));
    }

    #[test]
    fn graph_url_subfolder() {
        let d = OneDriveDrive::new("test".into(), "tok".into(), None, None, None);
        let url = d.graph_url(Path::new("Documents/Work"));
        assert!(url.contains("root:/Documents/Work:/children"));
    }

    #[test]
    fn item_url_file() {
        let d = OneDriveDrive::new("test".into(), "tok".into(), None, None, None);
        let url = d.item_url(Path::new("Documents/report.pdf"));
        assert!(url.contains("root:/Documents/report.pdf"));
        assert!(!url.contains("children"));
    }

    #[test]
    fn capabilities_include_graph_mutations_but_not_copy() {
        let d = OneDriveDrive::new("test".into(), "tok".into(), None, None, None);
        let capabilities = d.capabilities();
        assert!(capabilities.create_dir);
        assert!(capabilities.rename);
        assert!(capabilities.move_path);
        assert!(!capabilities.copy);
        assert!(capabilities.share_links);
        assert!(capabilities.versions);
    }

    #[test]
    fn graph_mutations_use_expected_endpoints() {
        let mut server = Server::new();
        let create = server
            .mock("POST", "/v1.0/me/drive/root/children")
            .match_header("authorization", "Bearer tok")
            .match_body(mockito::Matcher::JsonString(
                r#"{"@microsoft.graph.conflictBehavior":"fail","folder":{},"name":"Archive"}"#
                    .into(),
            ))
            .with_status(201)
            .create();
        let move_mock = server
            .mock("PATCH", "/v1.0/me/drive/root:/old.txt")
            .match_header("authorization", "Bearer tok")
            .match_body(mockito::Matcher::JsonString(r#"{"name":"new.txt"}"#.into()))
            .with_status(200)
            .create();
        let drive = OneDriveDrive::with_graph_base(
            "test".into(),
            "tok".into(),
            None,
            None,
            None,
            format!("{}/v1.0", server.url()),
        );
        drive.create_dir(Path::new("Archive")).unwrap();
        drive
            .move_path(Path::new("old.txt"), Path::new("new.txt"))
            .unwrap();
        create.assert();
        move_mock.assert();
    }

    #[test]
    fn share_link_url_targets_graph_create_link() {
        let d = OneDriveDrive::new("test".into(), "tok".into(), None, None, None);
        assert_eq!(
            d.share_link_url(Path::new("Documents/report.pdf")),
            "https://graph.microsoft.com/v1.0/me/drive/root:/Documents/report.pdf/createLink"
        );
    }

    #[test]
    fn share_link_posts_graph_create_link_and_returns_web_url() {
        let mut server = Server::new();
        let mock = server
            .mock("POST", "/v1.0/me/drive/root:/report.pdf/createLink")
            .match_header("authorization", "Bearer tok")
            .with_status(200)
            .with_body(r#"{"link":{"webUrl":"https://onedrive.example/s/abc"}}"#)
            .create();
        let drive = OneDriveDrive::with_graph_base(
            "test".into(),
            "tok".into(),
            None,
            None,
            None,
            format!("{}/v1.0", server.url()),
        );
        assert_eq!(
            drive
                .share_link(Path::new("report.pdf"))
                .unwrap()
                .as_deref(),
            Some("https://onedrive.example/s/abc")
        );
        mock.assert();
    }

    #[test]
    fn share_link_surfaces_expired_token_response() {
        let mut server = Server::new();
        let mock = server
            .mock("POST", "/v1.0/me/drive/root:/report.pdf/createLink")
            .match_header("authorization", "Bearer expired")
            .with_status(401)
            .with_body(r#"{"error":{"code":"InvalidAuthenticationToken"}}"#)
            .create();
        let drive = OneDriveDrive::with_graph_base(
            "test".into(),
            "expired".into(),
            None,
            None,
            None,
            format!("{}/v1.0", server.url()),
        );
        let error = drive.share_link(Path::new("report.pdf")).unwrap_err();
        assert!(error.to_string().contains("HTTP 401"));
        mock.assert();
    }

    #[test]
    fn versions_list_and_restore_use_graph_endpoints() {
        let mut server = Server::new();
        let versions = server
            .mock("GET", "/v1.0/me/drive/root:/report.pdf:/versions")
            .match_header("authorization", "Bearer tok")
            .with_status(200)
            .with_body(r#"{"value":[{"id":"v1","lastModifiedDateTime":"2024-01-15T10:30:00Z","size":12,"lastModifiedBy":{"user":{"displayName":"Alice"}}}]}"#)
            .create();
        let restore = server
            .mock("POST", "/v1.0/me/drive/root:/report.pdf:/versions/v1/restoreVersion")
            .match_header("authorization", "Bearer tok")
            .with_status(204)
            .create();
        let drive = OneDriveDrive::with_graph_base(
            "test".into(),
            "tok".into(),
            None,
            None,
            None,
            format!("{}/v1.0", server.url()),
        );

        let result = drive.list_versions(Path::new("report.pdf")).unwrap();
        assert_eq!(result[0].id, "v1");
        assert_eq!(result[0].size, Some(12));
        assert_eq!(result[0].modifier_name.as_deref(), Some("Alice"));
        assert!(result[0].modified_at.is_some());
        drive.restore_version(Path::new("report.pdf"), "v1").unwrap();
        versions.assert();
        restore.assert();
    }

    #[test]
    #[ignore = "mutates a real OneDrive; requires CRISPSORTER_ONEDRIVE_ACCESS_TOKEN"]
    fn onedrive_live_file_round_trip() {
        let Some(token) = std::env::var("CRISPSORTER_ONEDRIVE_ACCESS_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            eprintln!("skipping OneDrive live test: access token not configured");
            return;
        };
        let drive = OneDriveDrive::new("live-test".into(), token, None, None, None);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let path = Path::new("_crispsorter_live").join(format!("onedrive-{nonce}.txt"));
        let content = format!("CrispSorter OneDrive live test {nonce}").into_bytes();
        let result = (|| -> Result<()> {
            drive.create_dir(Path::new("_crispsorter_live"))
                .or_else(|error| {
                    anyhow::ensure!(error.to_string().contains("HTTP 409"));
                    Ok(())
                })?;
            drive.write_file(&path, &content)?;
            let stat = drive.stat(&path)?;
            anyhow::ensure!(!stat.is_dir && stat.size == content.len() as u64);
            anyhow::ensure!(drive.read_file(&path)? == content);
            let versions = drive.list_versions(&path)?;
            anyhow::ensure!(!versions.is_empty(), "OneDrive returned no versions");
            Ok(())
        })();
        let _ = drive.delete(&path);
        let _ = drive.delete(Path::new("_crispsorter_live"));
        result.expect("OneDrive live round trip failed");
    }
}
