//! Drive REST API client (DRIVE_NEW_API_URL). Mirrors og/sdk auth + storage.

use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

use crate::config;
use crate::models::{Credentials, DriveFileData};

/// Connecting to a reachable API host should be fast; anything slower almost
/// certainly means a dead peer or a firewalled black hole rather than a slow
/// server, so we don't let it hang indefinitely.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Every [`DriveApi`] call is a small metadata/JSON request (auth, folder
/// listings, file entry CRUD, ...) — never a large file body (those go
/// through [`crate::network::NetworkApi`] to presigned storage URLs). So it's
/// safe, and desirable, to cap the *total* request duration here — unlike the
/// network client, which streams large bodies and must not be capped this way.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeouts for [`DriveApi`]'s HTTP client.
#[derive(Clone, Copy, Debug)]
pub struct DriveTimeouts {
    pub connect: Duration,
    pub request: Duration,
}

impl Default for DriveTimeouts {
    fn default() -> Self {
        DriveTimeouts {
            connect: DEFAULT_CONNECT_TIMEOUT,
            request: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

pub struct DriveApi {
    client: Client,
    base: String,
    /// Active workspace as (uuid, token). When set, requests carry the
    /// `x-internxt-workspace` header and folder/trash/file-entry calls route to
    /// the `/workspaces/{id}/...` endpoints.
    workspace: Option<(String, String)>,
}

fn base_headers() -> HeaderMap {
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
    if let Ok(v) = HeaderValue::from_str(&config::desktop_header()) {
        h.insert("x-internxt-desktop-header", v);
    }
    h
}

impl DriveApi {
    pub fn new() -> Self {
        Self::with_timeouts(DriveTimeouts::default())
    }

    /// Same as [`Self::new`], with caller-adjustable timeouts (e.g. so
    /// `internxt-cli-rust` can widen them via a flag/env var).
    pub fn with_timeouts(timeouts: DriveTimeouts) -> Self {
        let client = Client::builder()
            .connect_timeout(timeouts.connect)
            .timeout(timeouts.request)
            .build()
            .unwrap_or_default();
        DriveApi {
            client,
            base: config::drive_api_url(),
            workspace: None,
        }
    }

    /// Build a client scoped to the credentials' active workspace (if any), so
    /// every request carries the workspace header and routes appropriately.
    pub fn for_credentials(creds: &Credentials) -> Self {
        let mut api = Self::new();
        if let Some(w) = &creds.workspace {
            api.workspace = Some((w.id.clone(), w.token.clone()));
        }
        api
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Authenticated headers, including `x-internxt-workspace` when a workspace
    /// is active (mirrors node SdkManager.init with a workspaceToken).
    fn auth_headers(&self, token: &str) -> Result<HeaderMap> {
        let mut h = base_headers();
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))?);
        if let Some((_, ws_token)) = &self.workspace {
            h.insert("x-internxt-workspace", HeaderValue::from_str(ws_token)?);
        }
        Ok(h)
    }

    async fn check(resp: reqwest::Response, ctx: &str) -> Result<Value> {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("{ctx} failed: HTTP {status}: {text}"));
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// POST /auth/login -> (encrypted_salt (sKey), tfa_enabled)
    pub async fn security_details(&self, email: &str) -> Result<(String, bool)> {
        let resp = self
            .client
            .post(self.url("/auth/login"))
            .headers(base_headers())
            .json(&json!({ "email": email }))
            .send()
            .await?;
        let v = Self::check(resp, "securityDetails").await?;
        let skey = v["sKey"]
            .as_str()
            .ok_or_else(|| anyhow!("no sKey in response: {v}"))?
            .to_string();
        let tfa = v["tfa"].as_bool().unwrap_or(false) || v["tfa"].is_string();
        Ok((skey, tfa))
    }

    /// POST /auth/login/access (no keys) -> full response json (newToken, user, ...)
    pub async fn login_access(
        &self,
        email: &str,
        encrypted_password_hash: &str,
        tfa: Option<&str>,
    ) -> Result<Value> {
        let body = json!({
            "email": email,
            "password": encrypted_password_hash,
            "tfa": tfa,
        });
        let resp = self
            .client
            .post(self.url("/auth/login/access"))
            .headers(base_headers())
            .json(&body)
            .send()
            .await?;
        Self::check(resp, "loginAccess").await
    }

    /// GET /users/refresh -> new session token (refreshUserCredentials).
    /// Returns the `newToken`; the rest of the user identity is unchanged.
    pub async fn refresh_user_token(&self, token: &str) -> Result<String> {
        let v = self.refresh_user_credentials(token).await?;
        v["newToken"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("no newToken in refresh response: {v}"))
    }

    /// GET /users/refresh -> full `{ user, token, newToken }` (RefreshUserTokensDto).
    /// Used by the SSO login flow to fetch the user identity, since the
    /// universal link only carries the mnemonic, token and ecc private key.
    ///
    /// The node CLI hits `/users/cli/refresh`, but that path is tier-gated by the
    /// backend (`402 "CLI access not allowed for this user tier"` on non-Ultimate
    /// plans). `/users/refresh` is the first-party GUI endpoint (drive-web /
    /// drive-desktop) and returns the identical `RefreshUserTokensDto`, so we use
    /// it to work on every plan. See config `client_name` note on the gate.
    pub async fn refresh_user_credentials(&self, token: &str) -> Result<serde_json::Value> {
        let resp = self
            .client
            .get(self.url("/users/refresh"))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "refreshUserCredentials").await
    }

    /// GET /users/usage -> space used, split drive / backups / total (bytes).
    /// Mirrors og `storageClient.spaceUsageV2()`.
    pub async fn space_usage(&self, token: &str) -> Result<crate::models::SpaceUsage> {
        let resp = self
            .client
            .get(self.url("/users/usage"))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        let v = Self::check(resp, "spaceUsage").await?;
        Ok(serde_json::from_value(v)?)
    }

    /// GET /users/limit -> the plan's total space limit in bytes (`maxSpaceBytes`).
    /// Mirrors og `storageClient.spaceLimitV2()`.
    pub async fn space_limit(&self, token: &str) -> Result<u64> {
        let resp = self
            .client
            .get(self.url("/users/limit"))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        let v = Self::check(resp, "spaceLimit").await?;
        Ok(v.get("maxSpaceBytes").and_then(|m| m.as_u64()).unwrap_or(0))
    }

    /// GET /files/limits -> the plan's `maxUploadFileSize` in bytes, or `None`
    /// when the plan sets no per-file cap (field null/absent). Mirrors og
    /// `storageClient.getFileVersionLimits()`.
    pub async fn get_file_limits(&self, token: &str) -> Result<Option<u64>> {
        let resp = self
            .client
            .get(self.url("/files/limits"))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        let v = Self::check(resp, "getFileLimits").await?;
        Ok(v.get("maxUploadFileSize").and_then(|m| m.as_u64()))
    }

    /// GET {payments}/products/tier -> the plan's human `label` (e.g. "Pro").
    /// Best-effort (separate API, og cli never calls it). **Unreliable for
    /// legacy plans**: those come back `label:"free"` regardless, so callers
    /// must corroborate with [`Self::user_subscription`] before trusting it.
    pub async fn user_tier(&self, token: &str) -> Result<Option<String>> {
        let url = format!("{}/products/tier", config::payments_api_url());
        let mut headers = base_headers();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))?);
        let resp = self.client.get(url).headers(headers).send().await?;
        let v = Self::check(resp, "userTier").await?;
        Ok(v.get("label")
            .and_then(|l| l.as_str())
            .map(|s| s.to_string()))
    }

    /// GET {payments}/subscriptions -> the billing `type`: `free`, `lifetime`,
    /// or `subscription`. This is the authoritative plan signal (legacy lifetime
    /// accounts report `lifetime` here even while the tier endpoint mislabels
    /// them `free`). Best-effort; `None` on error/absent. Not workspace-scoped.
    pub async fn user_subscription(&self, token: &str) -> Result<Option<String>> {
        let url = format!("{}/subscriptions", config::payments_api_url());
        let mut headers = base_headers();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))?);
        let resp = self.client.get(url).headers(headers).send().await?;
        let v = Self::check(resp, "userSubscription").await?;
        Ok(v.get("type").and_then(|t| t.as_str()).map(|s| s.to_string()))
    }

    /// GET /files/{uuid}/meta
    pub async fn get_file_meta(&self, token: &str, uuid: &str) -> Result<DriveFileData> {
        let resp = self
            .client
            .get(self.url(&format!("/files/{uuid}/meta")))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        let v = Self::check(resp, "getFileMeta").await?;
        Ok(serde_json::from_value(v)?)
    }

    /// GET /files/{uuid}/meta — raw JSON (keeps fields absent from
    /// [`DriveFileData`], e.g. `folderUuid`, needed to reconstruct a file's path).
    pub async fn get_file_meta_value(&self, token: &str, uuid: &str) -> Result<Value> {
        let resp = self
            .client
            .get(self.url(&format!("/files/{uuid}/meta")))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "getFileMeta").await
    }

    /// GET folder ancestors — the chain from the folder itself (first element) up
    /// to the account/workspace root (last element, `parentUuid: null`). Each entry
    /// carries `uuid`/`plainName`/`parentUuid`. Workspace-aware
    /// (`/workspaces/{id}/folders/{uuid}/ancestors`).
    pub async fn get_folder_ancestors(&self, token: &str, uuid: &str) -> Result<Value> {
        let path = match &self.workspace {
            Some((id, _)) => format!("/workspaces/{id}/folders/{uuid}/ancestors"),
            None => format!("/folders/{uuid}/ancestors"),
        };
        let resp = self
            .client
            .get(self.url(&path))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "getFolderAncestors").await
    }

    /// POST /files (createFileEntryByUuid), or POST /workspaces/{id}/files when a
    /// workspace is active. The workspace variant omits `creationTime` and adds a
    /// `date` field (mirrors og workspaceClient.createFileEntry).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_file_entry(
        &self,
        token: &str,
        plain_name: &str,
        file_type: &str,
        size: u64,
        folder_uuid: &str,
        file_id: &str,
        bucket: &str,
        creation_time: &str,
        modification_time: &str,
    ) -> Result<DriveFileData> {
        let (path, body) = match &self.workspace {
            Some((id, _)) => (
                format!("/workspaces/{id}/files"),
                json!({
                    "name": plain_name,
                    "plainName": plain_name,
                    "type": file_type,
                    "size": size,
                    "folderUuid": folder_uuid,
                    "fileId": file_id,
                    "bucket": bucket,
                    "encryptVersion": "03-aes",
                    "modificationTime": modification_time,
                    "date": modification_time,
                }),
            ),
            None => (
                "/files".to_string(),
                json!({
                    "plainName": plain_name,
                    "type": file_type,
                    "size": size,
                    "folderUuid": folder_uuid,
                    "fileId": file_id,
                    "bucket": bucket,
                    "encryptVersion": "03-aes",
                    "creationTime": creation_time,
                    "modificationTime": modification_time,
                }),
            ),
        };
        let resp = self
            .client
            .post(self.url(&path))
            .headers(self.auth_headers(token)?)
            .json(&body)
            .send()
            .await?;
        let v = Self::check(resp, "createFileEntry").await?;
        Ok(serde_json::from_value(v)?)
    }

    /// PUT /files/{uuid} — replace an existing file's content in place (keeps the
    /// same uuid/name/folder, swaps `fileId` + `size`). Mirrors og
    /// storage.replaceFile; avoids the 409 that createFileEntry raises for a
    /// duplicate name in the same folder.
    pub async fn replace_file(
        &self,
        token: &str,
        uuid: &str,
        file_id: &str,
        size: u64,
    ) -> Result<DriveFileData> {
        let resp = self
            .client
            .put(self.url(&format!("/files/{uuid}")))
            .headers(self.auth_headers(token)?)
            .json(&json!({ "fileId": file_id, "size": size }))
            .send()
            .await?;
        let v = Self::check(resp, "replaceFile").await?;
        Ok(serde_json::from_value(v)?)
    }

    /// POST /files/thumbnail — register a thumbnail for a file (mirrors og
    /// storage.createThumbnailEntryWithUUID). The thumbnail bytes must already be
    /// uploaded to the network; `bucket_file` is that network file id.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_thumbnail_entry(
        &self,
        token: &str,
        file_uuid: &str,
        thumbnail_type: &str,
        size: u64,
        max_width: u32,
        max_height: u32,
        bucket: &str,
        bucket_file: &str,
    ) -> Result<crate::models::Thumbnail> {
        let body = json!({
            "fileUuid": file_uuid,
            "type": thumbnail_type,
            "size": size,
            "maxWidth": max_width,
            "maxHeight": max_height,
            "bucketId": bucket,
            "bucketFile": bucket_file,
            "encryptVersion": "03-aes",
        });
        let resp = self
            .client
            .post(self.url("/files/thumbnail"))
            .headers(self.auth_headers(token)?)
            .json(&body)
            .send()
            .await?;
        let v = Self::check(resp, "createThumbnailEntry").await?;
        Ok(serde_json::from_value(v)?)
    }

    /// GET /workspaces/ — available + pending workspaces (WorkspacesResponse).
    pub async fn get_workspaces(&self, token: &str) -> Result<Value> {
        let resp = self
            .client
            .get(self.url("/workspaces/"))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "getWorkspaces").await
    }

    /// GET /workspaces/{id}/credentials — network creds + token header for a workspace.
    pub async fn get_workspace_credentials(&self, token: &str, workspace_id: &str) -> Result<Value> {
        let resp = self
            .client
            .get(self.url(&format!("/workspaces/{workspace_id}/credentials")))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "getWorkspaceCredentials").await
    }

    /// GET /auth/logout (best effort; invalidates the session token server-side).
    pub async fn logout(&self, token: &str) -> Result<()> {
        let resp = self
            .client
            .get(self.url("/auth/logout"))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "logout").await?;
        Ok(())
    }

    /// GET /folders/{uuid}/meta
    pub async fn get_folder_meta(&self, token: &str, uuid: &str) -> Result<Value> {
        let resp = self
            .client
            .get(self.url(&format!("/folders/{uuid}/meta")))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "getFolderMeta").await
    }

    /// One page of subfolders (returns `.folders`/`.result`). Personal endpoint is
    /// `/folders/content/{uuid}/folders/`; workspace is `/workspaces/{id}/folders/{uuid}/folders/`.
    pub async fn get_folder_subfolders(
        &self,
        token: &str,
        uuid: &str,
        offset: u32,
    ) -> Result<Value> {
        let path = match &self.workspace {
            Some((id, _)) => {
                format!("/workspaces/{id}/folders/{uuid}/folders/?offset={offset}&limit=50")
            }
            None => format!(
                "/folders/content/{uuid}/folders/?offset={offset}&limit=50&sort=plainName&order=ASC"
            ),
        };
        let resp = self
            .client
            .get(self.url(&path))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "getFolderFolders").await
    }

    /// One page of files (returns `.files`/`.result`). Personal endpoint is
    /// `/folders/content/{uuid}/files/`; workspace is `/workspaces/{id}/folders/{uuid}/files/`.
    pub async fn get_folder_subfiles(
        &self,
        token: &str,
        uuid: &str,
        offset: u32,
    ) -> Result<Value> {
        let path = match &self.workspace {
            Some((id, _)) => {
                format!("/workspaces/{id}/folders/{uuid}/files/?offset={offset}&limit=50")
            }
            None => format!(
                "/folders/content/{uuid}/files/?offset={offset}&limit=50&sort=plainName&order=ASC"
            ),
        };
        let resp = self
            .client
            .get(self.url(&path))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "getFolderFiles").await
    }

    /// Create a folder by parent uuid. Routes to `/workspaces/{id}/folders` when
    /// a workspace is active (payload uses `name` instead of `plainName`).
    pub async fn create_folder(
        &self,
        token: &str,
        plain_name: &str,
        parent_folder_uuid: &str,
    ) -> Result<Value> {
        let (path, body) = match &self.workspace {
            Some((id, _)) => (
                format!("/workspaces/{id}/folders"),
                json!({ "name": plain_name, "parentFolderUuid": parent_folder_uuid }),
            ),
            None => (
                "/folders".to_string(),
                json!({ "plainName": plain_name, "parentFolderUuid": parent_folder_uuid }),
            ),
        };
        let resp = self
            .client
            .post(self.url(&path))
            .headers(self.auth_headers(token)?)
            .json(&body)
            .send()
            .await?;
        Self::check(resp, "createFolder").await
    }

    /// PATCH /folders/{uuid} — move folder into a destination folder.
    pub async fn move_folder(&self, token: &str, uuid: &str, destination: &str) -> Result<Value> {
        let resp = self
            .client
            .patch(self.url(&format!("/folders/{uuid}")))
            .headers(self.auth_headers(token)?)
            .json(&json!({ "destinationFolder": destination }))
            .send()
            .await?;
        Self::check(resp, "moveFolder").await
    }

    /// PATCH /files/{uuid} — move file into a destination folder.
    pub async fn move_file(&self, token: &str, uuid: &str, destination: &str) -> Result<Value> {
        let resp = self
            .client
            .patch(self.url(&format!("/files/{uuid}")))
            .headers(self.auth_headers(token)?)
            .json(&json!({ "destinationFolder": destination }))
            .send()
            .await?;
        Self::check(resp, "moveFile").await
    }

    /// PUT /folders/{uuid}/meta — rename folder.
    pub async fn rename_folder(&self, token: &str, uuid: &str, plain_name: &str) -> Result<()> {
        let resp = self
            .client
            .put(self.url(&format!("/folders/{uuid}/meta")))
            .headers(self.auth_headers(token)?)
            .json(&json!({ "plainName": plain_name }))
            .send()
            .await?;
        Self::check(resp, "renameFolder").await?;
        Ok(())
    }

    /// PUT /files/{uuid}/meta — rename file (plainName + type).
    pub async fn rename_file(
        &self,
        token: &str,
        uuid: &str,
        plain_name: &str,
        file_type: &str,
    ) -> Result<()> {
        let resp = self
            .client
            .put(self.url(&format!("/files/{uuid}/meta")))
            .headers(self.auth_headers(token)?)
            .json(&json!({ "plainName": plain_name, "type": file_type }))
            .send()
            .await?;
        Self::check(resp, "renameFile").await?;
        Ok(())
    }

    /// POST /storage/trash/add — move items to trash. `items` = [{uuid,type}].
    pub async fn trash_items(&self, token: &str, items: Value) -> Result<()> {
        let resp = self
            .client
            .post(self.url("/storage/trash/add"))
            .headers(self.auth_headers(token)?)
            .json(&json!({ "items": items }))
            .send()
            .await?;
        Self::check(resp, "trashItems").await?;
        Ok(())
    }

    /// One page of trash; `kind` is "files" or "folders". Personal uses
    /// `/storage/trash/paginated`; workspace uses `/workspaces/{id}/trash` with a
    /// singular `type` (`file`/`folder`).
    pub async fn trash_paginated(&self, token: &str, kind: &str, offset: u32) -> Result<Value> {
        let path = match &self.workspace {
            Some((id, _)) => {
                let ws_type = if kind == "folders" { "folder" } else { "file" };
                format!("/workspaces/{id}/trash?offset={offset}&limit=50&type={ws_type}")
            }
            None => {
                format!("/storage/trash/paginated?limit=50&offset={offset}&type={kind}&root=true")
            }
        };
        let resp = self
            .client
            .get(self.url(&path))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "getTrashPaginated").await
    }

    /// Empty the trash permanently. Personal: DELETE /storage/trash/all;
    /// workspace: DELETE /workspaces/{id}/trash.
    pub async fn clear_trash(&self, token: &str) -> Result<()> {
        let path = match &self.workspace {
            Some((id, _)) => format!("/workspaces/{id}/trash"),
            None => "/storage/trash/all".to_string(),
        };
        let resp = self
            .client
            .delete(self.url(&path))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "clearTrash").await?;
        Ok(())
    }

    /// DELETE /files/{uuid} — permanently delete a file.
    pub async fn delete_file(&self, token: &str, uuid: &str) -> Result<()> {
        let resp = self
            .client
            .delete(self.url(&format!("/files/{uuid}")))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "deleteFile").await?;
        Ok(())
    }

    /// DELETE /folders/{uuid} — permanently delete a folder.
    pub async fn delete_folder(&self, token: &str, uuid: &str) -> Result<()> {
        let resp = self
            .client
            .delete(self.url(&format!("/folders/{uuid}")))
            .headers(self.auth_headers(token)?)
            .send()
            .await?;
        Self::check(resp, "deleteFolder").await?;
        Ok(())
    }
}

impl Default for DriveApi {
    fn default() -> Self {
        Self::new()
    }
}
