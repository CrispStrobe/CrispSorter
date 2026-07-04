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

use super::{CloudDrive, DirEntry, DriveType, FileStat};

const API_BASE: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/drive/v3";

pub struct GoogleDriveDrive {
    label: String,
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
        Self {
            label,
            access_token,
            _refresh_token: refresh_token,
            _client_id: client_id,
            _client_secret: client_secret,
            client: reqwest::blocking::Client::new(),
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
                API_BASE, encode_query_param(&q)
            );
            let resp = self.client
                .get(&url)
                .header("Authorization", self.auth_header())
                .send()
                .with_context(|| format!("Google Drive resolve: {name}"))?;

            if !resp.status().is_success() {
                return Err(anyhow!("Google Drive resolve '{}': HTTP {}", name, resp.status()));
            }

            let body: serde_json::Value = resp.json()?;
            let files = body["files"].as_array()
                .ok_or_else(|| anyhow!("Google Drive: no files array for '{name}'"))?;

            if files.is_empty() {
                // If this is the last component, the file doesn't exist
                if i == components.len() - 1 {
                    return Err(anyhow!("not found: {}", path.display()));
                }
                return Err(anyhow!("folder not found: {name}"));
            }

            parent_id = files[0]["id"].as_str()
                .ok_or_else(|| anyhow!("no id for '{name}'"))?
                .to_string();
        }

        Ok(parent_id)
    }
}

impl CloudDrive for GoogleDriveDrive {
    fn label(&self) -> &str { &self.label }
    fn drive_type(&self) -> DriveType { DriveType::GoogleDrive }

    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>> {
        let folder_id = self.resolve_id(path)?;
        let q = format!("'{}' in parents and trashed = false", folder_id);
        let url = format!(
            "{}/files?q={}&fields=files(id,name,mimeType,size)&pageSize=1000",
            API_BASE, encode_query_param(&q)
        );

        let resp = self.client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive list_dir")?;

        if !resp.status().is_success() {
            return Err(anyhow!("Google Drive list_dir: HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp.json()?;
        let files = body["files"].as_array()
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
        let url = format!("{}/files/{}?alt=media", API_BASE, file_id);

        let resp = self.client
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
        let filename = path.file_name()
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
        let url = format!(
            "{}/files?uploadType=multipart&fields=id",
            UPLOAD_BASE
        );

        let boundary = "crispsorter_boundary";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Type: application/json\r\n\r\n").as_bytes());
        body.extend_from_slice(metadata.to_string().as_bytes());
        body.extend_from_slice(format!("\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let resp = self.client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", format!("multipart/related; boundary={boundary}"))
            .body(body)
            .send()
            .context("Google Drive write_file")?;

        if !resp.status().is_success() {
            return Err(anyhow!("Google Drive write_file: HTTP {}", resp.status()));
        }
        Ok(())
    }

    fn delete(&self, path: &Path) -> Result<()> {
        let file_id = self.resolve_id(path)?;
        let url = format!("{}/files/{}", API_BASE, file_id);

        let resp = self.client
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
            API_BASE, file_id
        );

        let resp = self.client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .context("Google Drive stat")?;

        if !resp.status().is_success() {
            return Err(anyhow!("Google Drive stat: HTTP {}", resp.status()));
        }

        let body: serde_json::Value = resp.json()?;
        let size = body["size"].as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let is_dir = body["mimeType"].as_str() == Some("application/vnd.google-apps.folder");
        let mtime_unix = body["modifiedTime"].as_str()
            .and_then(super::onedrive::chrono_parse_iso8601);

        Ok(FileStat { size, is_dir, mtime_unix })
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
}
