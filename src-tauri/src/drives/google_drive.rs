//! Google Drive cloud drive via Drive API v3 (P27.11).
//!
//! Implements `CloudDrive` using `reqwest::blocking` HTTP calls to the
//! Google Drive v3 REST API.  Auth: OAuth2 access token passed in at
//! construction.
//!
//! Path semantics: Google Drive uses file IDs, not paths.  This
//! implementation maps paths to IDs by walking the folder hierarchy
//! from root.  Slow for deep paths but correct.

use anyhow::{anyhow, Context, Result};
use std::path::Path;

use crate::sync::proxy::{build_blocking_client, ProxyConfig};

use super::{CloudDrive, DirEntry, DriveCapabilities, DriveType, FileStat, FileVersion};

const API_BASE: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";

pub struct GoogleDriveDrive {
    label: String,
    api_base: String,
    upload_base: String,
    access_token: String,
    _refresh_token: Option<String>,
    _client_id: Option<String>,
    _client_secret: Option<String>,
    client: reqwest::blocking::Client,
}

impl GoogleDriveDrive {
    pub fn new(
        label: String,
        access_token: String,
        refresh_token: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
    ) -> Self {
        Self::new_with_proxy(
            label,
            access_token,
            refresh_token,
            client_id,
            client_secret,
            &ProxyConfig::default(),
        )
        .expect("default Google Drive HTTP client must build")
    }

    /// Construct a Google Drive client with the shared HTTP/SOCKS5 policy.
    pub fn new_with_proxy(
        label: String,
        access_token: String,
        refresh_token: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        proxy: &ProxyConfig,
    ) -> Result<Self> {
        let client = build_blocking_client(proxy)?;
        Ok(Self::with_api_base_and_client(
            label,
            access_token,
            refresh_token,
            client_id,
            client_secret,
            API_BASE,
            client,
        ))
    }

    fn with_api_base(
        label: String,
        access_token: String,
        refresh_token: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        api_base: impl Into<String>,
    ) -> Self {
        Self::with_api_base_and_client(
            label,
            access_token,
            refresh_token,
            client_id,
            client_secret,
            api_base,
            reqwest::blocking::Client::new(),
        )
    }

    fn with_api_base_and_client(
        label: String,
        access_token: String,
        refresh_token: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        api_base: impl Into<String>,
        client: reqwest::blocking::Client,
    ) -> Self {
        let api_base = api_base.into();
        let upload_base = api_base
            .strip_suffix("/drive/v3")
            .map(|origin| format!("{origin}/upload/drive/v3"))
            .unwrap_or_else(|| UPLOAD_BASE.to_owned());
        Self {
            label,
            api_base,
            upload_base,
            access_token,
            _refresh_token: refresh_token,
            _client_id: client_id,
            _client_secret: client_secret,
            client,
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.access_token)
    }

    /// Resolve a path like "Documents/Work/report.pdf" to a Google Drive
    /// file ID by walking the folder hierarchy from root.
    fn resolve_id(&self, path: &Path) -> Result<String> {
        let rel = path.to_string_lossy();
        let rel = rel.trim_start_matches('/');
        if rel.is_empty() || rel == "." {
            return Ok("root".to_string());
        }

        let mut parent_id = "root".to_string();
        let components: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();

        for (i, name) in components.iter().enumerate() {
            let escaped = name.replace('\'', "\\'");
            let q = format!(
                "'{}' in parents and name = '{}' and trashed = false",
                parent_id, escaped
            );
            let url = format!(
                "{}/files?q={}&fields=files(id,name)&pageSize=1",
                self.api_base,
                encode_query_param(&q)
            );
            let resp = self
                .client
                .get(&url)
                .header("Authorization", self.auth_header())
                .send()
                .with_context(|| format!("Google Drive resolve: {name}"))?;

            if !resp.status().is_success() {
                return Err(anyhow!(
                    "Google Drive resolve '{}': HTTP {}",
                    name,
                    resp.status()
                ));
            }

            let body: serde_json::Value = resp.json()?;
            let files = body["files"]
                .as_array()
                .ok_or_else(|| anyhow!("Google Drive: no files array for '{name}'"))?;

            if files.is_empty() {
                // If this is the last component, the file doesn't exist
                if i == components.len() - 1 {
                    return Err(anyhow!("not found: {}", path.display()));
                }
                return Err(anyhow!("folder not found: {name}"));
            }

            parent_id = files[0]["id"]
                .as_str()
                .ok_or_else(|| anyhow!("no id for '{name}'"))?
                .to_string();
        }

        Ok(parent_id)
    }
}

impl CloudDrive for GoogleDriveDrive {
    fn label(&self) -> &str {
        &self.label
    }
    fn drive_type(&self) -> DriveType {
        DriveType::GoogleDrive
    }

    fn capabilities(&self) -> DriveCapabilities {
        DriveCapabilities {
            create_dir: true,
            rename: true,
            move_path: true,
            copy: true,
            share_links: true,
            versions: true,
            ..DriveCapabilities::basic()
        }
    }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let folder_id = self.resolve_id(path)?;
        let q = format!("'{}' in parents and trashed = false", folder_id);
        let url = format!(
            "{}/files?q={}&fields=files(id,name,mimeType,size)&pageSize=1000",
            self.api_base,
            encode_query_param(&q)
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive list_dir")?;

        if !resp.status().is_success() {
            return Err(anyhow!("Google Drive list_dir: HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp.json()?;
        let files = body["files"]
            .as_array()
            .ok_or_else(|| anyhow!("Google Drive: no files array"))?;

        let mut entries = Vec::with_capacity(files.len());
        for f in files {
            let name = f["name"].as_str().unwrap_or("").to_string();
            let is_dir = f["mimeType"].as_str() == Some("application/vnd.google-apps.folder");
            let size = f["size"].as_str().and_then(|s| s.parse().ok());
            entries.push(DirEntry { name, is_dir, size });
        }
        Ok(entries)
    }

    fn read_file(&self, path: &Path) -> Result<Vec<u8>> {
        let file_id = self.resolve_id(path)?;
        let url = format!("{}/files/{}?alt=media", self.api_base, file_id);

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive read_file")?;

        if !resp.status().is_success() {
            return Err(anyhow!("Google Drive read_file: HTTP {}", resp.status()));
        }

        resp.bytes()
            .map(|b| b.to_vec())
            .context("Google Drive read_file: reading bytes")
    }

    fn write_file(&self, path: &Path, data: &[u8]) -> Result<()> {
        let rel = path.to_string_lossy();
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| rel.to_string());

        // Resolve parent folder
        let parent_path = path.parent().unwrap_or_else(|| Path::new(""));
        let parent_id = self.resolve_id(parent_path)?;

        let metadata = serde_json::json!({
            "name": filename,
            "parents": [parent_id]
        });

        // Simple upload (< 5MB); resumable upload is a follow-up.
        let url = format!("{}/files?uploadType=multipart&fields=id", self.upload_base);

        let boundary = "crispsorter_boundary";
        let mut body = Vec::new();
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Type: application/json\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(metadata.to_string().as_bytes());
        body.extend_from_slice(
            format!("\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header(
                "Content-Type",
                format!("multipart/related; boundary={boundary}"),
            )
            .body(body)
            .send()
            .context("Google Drive write_file")?;

        if !resp.status().is_success() {
            return Err(anyhow!("Google Drive write_file: HTTP {}", resp.status()));
        }
        Ok(())
    }

    fn create_dir(&self, path: &Path) -> Result<()> {
        let name = path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("Google Drive create_dir requires a directory name"))?;
        let parent_id = self.resolve_id(path.parent().unwrap_or_else(|| Path::new("")))?;
        let url = format!("{}/files?fields=id", self.api_base);
        let response = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({
                "name": name,
                "mimeType": "application/vnd.google-apps.folder",
                "parents": [parent_id]
            }))
            .send()
            .with_context(|| format!("Google Drive create_dir: {}", path.display()))?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Google Drive create_dir: HTTP {}",
                response.status()
            ));
        }
        Ok(())
    }

    fn move_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let file_id = self.resolve_id(source)?;
        let old_parent = self.resolve_id(source.parent().unwrap_or_else(|| Path::new("")))?;
        let new_parent = self.resolve_id(destination.parent().unwrap_or_else(|| Path::new("")))?;
        let name = destination
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("Google Drive move requires a destination name"))?;
        let url = format!(
            "{}/files/{}?addParents={}&removeParents={}&fields=id",
            self.api_base,
            file_id,
            encode_query_param(&new_parent),
            encode_query_param(&old_parent)
        );
        let response = self
            .client
            .patch(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "name": name }))
            .send()
            .with_context(|| {
                format!(
                    "Google Drive move: {} -> {}",
                    source.display(),
                    destination.display()
                )
            })?;
        if !response.status().is_success() {
            return Err(anyhow!("Google Drive move: HTTP {}", response.status()));
        }
        Ok(())
    }

    fn copy_path(&self, source: &Path, destination: &Path) -> Result<()> {
        let file_id = self.resolve_id(source)?;
        let parent_id = self.resolve_id(destination.parent().unwrap_or_else(|| Path::new("")))?;
        let name = destination
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("Google Drive copy requires a destination name"))?;
        let url = format!("{}/files/{}/copy?fields=id", self.api_base, file_id);
        let response = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({ "name": name, "parents": [parent_id] }))
            .send()
            .with_context(|| {
                format!(
                    "Google Drive copy: {} -> {}",
                    source.display(),
                    destination.display()
                )
            })?;
        if !response.status().is_success() {
            return Err(anyhow!("Google Drive copy: HTTP {}", response.status()));
        }
        Ok(())
    }

    fn delete(&self, path: &Path) -> Result<()> {
        let file_id = self.resolve_id(path)?;
        let url = format!("{}/files/{}", self.api_base, file_id);

        let resp = self
            .client
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive delete")?;

        if !resp.status().is_success() && resp.status().as_u16() != 404 {
            return Err(anyhow!("Google Drive delete: HTTP {}", resp.status()));
        }
        Ok(())
    }

    fn stat(&self, path: &Path) -> Result<FileStat> {
        let file_id = self.resolve_id(path)?;
        let url = format!(
            "{}/files/{}?fields=size,mimeType,modifiedTime",
            self.api_base, file_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive stat")?;

        if !resp.status().is_success() {
            return Err(anyhow!("Google Drive stat: HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp.json()?;
        let size = body["size"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let is_dir = body["mimeType"].as_str() == Some("application/vnd.google-apps.folder");
        let mtime_unix = body["modifiedTime"]
            .as_str()
            .and_then(super::onedrive::chrono_parse_iso8601);

        Ok(FileStat {
            size,
            is_dir,
            mtime_unix,
        })
    }

    fn share_link(&self, path: &Path) -> Result<Option<String>> {
        let file_id = self.resolve_id(path)?;
        let permission_url = format!("{}/files/{}/permissions?fields=id", self.api_base, file_id);
        let resp = self
            .client
            .post(&permission_url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({"type": "anyone", "role": "reader"}))
            .send()
            .context("Google Drive share_link: create permission")?;

        if !resp.status().is_success() {
            return Err(anyhow!("Google Drive share_link: HTTP {}", resp.status()));
        }

        let metadata_url = format!("{}/files/{}?fields=webViewLink", self.api_base, file_id);
        let resp = self
            .client
            .get(&metadata_url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive share_link: fetch URL")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "Google Drive share_link metadata: HTTP {}",
                resp.status()
            ));
        }

        let body: serde_json::Value = resp.json().context("Google Drive share_link: parse JSON")?;
        Ok(body["webViewLink"].as_str().map(str::to_owned))
    }

    fn list_versions(&self, path: &Path) -> Result<Vec<FileVersion>> {
        let file_id = self.resolve_id(path)?;
        let url = format!(
            "{}/files/{}/revisions?fields=revisions(id,modifiedTime,size,lastModifyingUser/displayName)",
            self.api_base, file_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive list_versions")?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "Google Drive list_versions: HTTP {}",
                resp.status()
            ));
        }

        let body: serde_json::Value = resp.json()?;
        let revisions = body["revisions"]
            .as_array()
            .ok_or_else(|| anyhow!("Google Drive: no revisions array"))?;

        let mut versions = Vec::with_capacity(revisions.len());
        for rev in revisions {
            let id = rev["id"].as_str().unwrap_or("").to_string();
            let modified_at = rev["modifiedTime"]
                .as_str()
                .and_then(super::onedrive::chrono_parse_iso8601);
            let size = rev["size"].as_str().and_then(|s| s.parse().ok());
            let modifier_name = rev["lastModifyingUser"]["displayName"]
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
        // Google Drive: copy a revision's content back to the current version.
        // There's no direct "restore" API like OneDrive — the approach is to
        // download the revision content and re-upload it.
        let file_id = self.resolve_id(path)?;

        // Download the specific revision
        let download_url = format!(
            "{}/files/{}/revisions/{}?alt=media",
            self.api_base, file_id, version_id
        );
        let resp = self
            .client
            .get(&download_url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive restore_version: download")?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "Google Drive restore_version download: HTTP {}",
                resp.status()
            ));
        }

        let data = resp.bytes()?.to_vec();

        // Re-upload as the current version via PATCH (update, not create)
        let update_url = format!("{}/files/{}?uploadType=media", self.upload_base, file_id);
        let resp = self
            .client
            .patch(&update_url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/octet-stream")
            .body(data)
            .send()
            .context("Google Drive restore_version: upload")?;

        if !resp.status().is_success() {
            return Err(anyhow!(
                "Google Drive restore_version upload: HTTP {}",
                resp.status()
            ));
        }
        Ok(())
    }
}

/// Minimal percent-encoding for query parameter values.
fn encode_query_param(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    #[test]
    fn proxy_constructor_rejects_invalid_proxy_before_requests() {
        let proxy = ProxyConfig {
            url: Some("not a proxy URL".into()),
            ..Default::default()
        };
        assert!(GoogleDriveDrive::new_with_proxy(
            "test".into(),
            "tok".into(),
            None,
            None,
            None,
            &proxy,
        )
        .is_err());
    }

    #[test]
    fn resolve_root() {
        let d = GoogleDriveDrive::new("test".into(), "tok".into(), None, None, None);
        let id = d.resolve_id(Path::new("")).unwrap();
        assert_eq!(id, "root");
    }

    #[test]
    fn resolve_dot() {
        let d = GoogleDriveDrive::new("test".into(), "tok".into(), None, None, None);
        let id = d.resolve_id(Path::new(".")).unwrap();
        assert_eq!(id, "root");
    }

    #[test]
    fn capabilities_include_drive_mutations_and_versions() {
        let d = GoogleDriveDrive::new("test".into(), "tok".into(), None, None, None);
        let capabilities = d.capabilities();
        assert!(capabilities.create_dir);
        assert!(capabilities.rename);
        assert!(capabilities.move_path);
        assert!(capabilities.copy);
        assert!(capabilities.share_links);
        assert!(capabilities.versions);
        assert!(!capabilities.streaming);
    }

    #[test]
    fn drive_mutations_use_injectable_api_endpoints() {
        let mut server = Server::new();
        let list = server
            .mock("GET", "/drive/v3/files")
            .match_query(mockito::Matcher::Any)
            .expect(2)
            .with_status(200)
            .with_body(r#"{"files":[{"id":"file-1","name":"old.txt"}]}"#)
            .create();
        let create = server
            .mock("POST", "/drive/v3/files")
            .match_query(mockito::Matcher::Any)
            .match_body(mockito::Matcher::JsonString(
                r#"{"mimeType":"application/vnd.google-apps.folder","name":"Archive","parents":["root"]}"#.into(),
            ))
            .with_status(200)
            .create();
        let move_mock = server
            .mock("PATCH", "/drive/v3/files/file-1")
            .match_query(mockito::Matcher::Any)
            .match_body(mockito::Matcher::JsonString(r#"{"name":"new.txt"}"#.into()))
            .with_status(200)
            .create();
        let copy = server
            .mock("POST", "/drive/v3/files/file-1/copy")
            .match_query(mockito::Matcher::Any)
            .match_body(mockito::Matcher::JsonString(
                r#"{"name":"copy.txt","parents":["root"]}"#.into(),
            ))
            .with_status(200)
            .create();
        let drive = GoogleDriveDrive::with_api_base(
            "test".into(),
            "tok".into(),
            None,
            None,
            None,
            format!("{}/drive/v3", server.url()),
        );

        drive.create_dir(Path::new("Archive")).unwrap();
        drive
            .move_path(Path::new("old.txt"), Path::new("new.txt"))
            .unwrap();
        drive
            .copy_path(Path::new("old.txt"), Path::new("copy.txt"))
            .unwrap();

        list.assert();
        create.assert();
        move_mock.assert();
        copy.assert();
    }

    #[test]
    fn share_permission_contract_uses_anonymous_reader() {
        let body = serde_json::json!({"type": "anyone", "role": "reader"});
        assert_eq!(body["type"], "anyone");
        assert_eq!(body["role"], "reader");
        assert_eq!(
            format!("{}/files/id/permissions?fields=id", API_BASE),
            "https://www.googleapis.com/drive/v3/files/id/permissions?fields=id"
        );
    }

    #[test]
    fn share_link_uses_permission_and_metadata_endpoints() {
        let mut server = Server::new();
        let list = server
            .mock("GET", "/drive/v3/files")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"files":[{"id":"file-1","name":"report.pdf"}]}"#)
            .create();
        let permission = server
            .mock("POST", "/drive/v3/files/file-1/permissions")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"id":"anyoneWithLink"}"#)
            .create();
        let metadata = server
            .mock("GET", "/drive/v3/files/file-1")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"webViewLink":"https://drive.example/file-1"}"#)
            .create();
        let drive = GoogleDriveDrive::with_api_base(
            "test".into(),
            "tok".into(),
            None,
            None,
            None,
            format!("{}/drive/v3", server.url()),
        );
        assert_eq!(
            drive
                .share_link(Path::new("report.pdf"))
                .unwrap()
                .as_deref(),
            Some("https://drive.example/file-1")
        );
        list.assert();
        permission.assert();
        metadata.assert();
    }

    #[test]
    fn share_link_surfaces_expired_token_response() {
        let mut server = Server::new();
        let list = server
            .mock("GET", "/drive/v3/files")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"files":[{"id":"file-1","name":"report.pdf"}]}"#)
            .create();
        let permission = server
            .mock("POST", "/drive/v3/files/file-1/permissions")
            .match_query(mockito::Matcher::Any)
            .with_status(401)
            .with_body(r#"{"error":{"code":401,"message":"Invalid Credentials"}}"#)
            .create();
        let drive = GoogleDriveDrive::with_api_base(
            "test".into(),
            "expired".into(),
            None,
            None,
            None,
            format!("{}/drive/v3", server.url()),
        );
        let error = drive.share_link(Path::new("report.pdf")).unwrap_err();
        assert!(error.to_string().contains("HTTP 401"));
        list.assert();
        permission.assert();
    }

    #[test]
    fn versions_list_and_restore_use_expected_endpoints() {
        let mut server = Server::new();
        let resolve = server
            .mock("GET", "/drive/v3/files")
            .match_query(mockito::Matcher::Any)
            .expect(2)
            .with_status(200)
            .with_body(r#"{"files":[{"id":"file-1","name":"report.pdf"}]}"#)
            .create();
        let revisions = server
            .mock("GET", "/drive/v3/files/file-1/revisions")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"revisions":[{"id":"rev-1","modifiedTime":"2024-01-15T10:30:00Z","size":"12","lastModifyingUser":{"displayName":"Alice"}}]}"#)
            .create();
        let download = server
            .mock("GET", "/drive/v3/files/file-1/revisions/rev-1")
            .match_query(mockito::Matcher::UrlEncoded("alt".into(), "media".into()))
            .with_status(200)
            .with_body("restored bytes")
            .create();
        let upload = server
            .mock("PATCH", "/upload/drive/v3/files/file-1")
            .match_query(mockito::Matcher::UrlEncoded(
                "uploadType".into(),
                "media".into(),
            ))
            .match_header("content-type", "application/octet-stream")
            .match_body("restored bytes")
            .with_status(200)
            .create();
        let drive = GoogleDriveDrive::with_api_base(
            "test".into(),
            "tok".into(),
            None,
            None,
            None,
            format!("{}/drive/v3", server.url()),
        );

        let versions = drive.list_versions(Path::new("report.pdf")).unwrap();
        assert_eq!(versions[0].id, "rev-1");
        assert_eq!(versions[0].size, Some(12));
        assert_eq!(versions[0].modifier_name.as_deref(), Some("Alice"));
        assert!(versions[0].modified_at.is_some());
        drive
            .restore_version(Path::new("report.pdf"), "rev-1")
            .unwrap();
        resolve.assert();
        revisions.assert();
        download.assert();
        upload.assert();
    }

    #[test]
    #[ignore = "mutates a real Google Drive; requires CRISPSORTER_GOOGLE_ACCESS_TOKEN"]
    fn google_drive_live_file_round_trip() {
        let Some(token) = std::env::var("CRISPSORTER_GOOGLE_ACCESS_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            eprintln!("skipping Google Drive live test: access token not configured");
            return;
        };
        let drive = GoogleDriveDrive::new("live-test".into(), token, None, None, None);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let path = Path::new("_crispsorter_live").join(format!("google-{nonce}.txt"));
        let content = format!("CrispSorter Google Drive live test {nonce}").into_bytes();
        let result = (|| -> Result<()> {
            drive
                .create_dir(Path::new("_crispsorter_live"))
                .or_else(|error| {
                    anyhow::ensure!(error.to_string().contains("HTTP 409"));
                    Ok(())
                })?;
            drive.write_file(&path, &content)?;
            let stat = drive.stat(&path)?;
            anyhow::ensure!(!stat.is_dir && stat.size == content.len() as u64);
            anyhow::ensure!(drive.read_file(&path)? == content);
            let versions = drive.list_versions(&path)?;
            anyhow::ensure!(!versions.is_empty(), "Google Drive returned no versions");
            Ok(())
        })();
        let _ = drive.delete(&path);
        let _ = drive.delete(Path::new("_crispsorter_live"));
        result.expect("Google Drive live round trip failed");
    }
}
