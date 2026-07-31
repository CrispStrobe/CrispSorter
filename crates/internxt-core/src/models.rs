use serde::{Deserialize, Serialize};

/// Persisted credentials (our own format; stored AES-encrypted like the node CLI).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Credentials {
    /// JWT used as Bearer for the drive API (the node CLI's `newToken`).
    pub token: String,
    pub user: UserInfo,
    /// Active workspace context (set by `workspaces use`), if any. When present,
    /// all drive/network operations are scoped to this workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceContext>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UserInfo {
    pub email: String,
    /// Plain (decrypted) mnemonic.
    pub mnemonic: String,
    pub bucket: String,
    pub bridge_user: String,
    pub user_id: String,
    pub root_folder_id: String,
    /// Decrypted ecc (OpenPGP) private key, base64(armored). Needed to decrypt
    /// workspace mnemonics. Optional: only present for key-aware logins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ecc_private_key: Option<String>,
    /// Decrypted kyber private key, base64(raw). Optional (hybrid workspaces only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kyber_private_key: Option<String>,
}

/// Persisted active-workspace context. Mirrors the node CLI's stored `workspace`
/// (credentials + decrypted workspace mnemonic).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WorkspaceContext {
    /// Workspace uuid (used in `/workspaces/{id}/...` routes and the web link).
    pub id: String,
    pub name: String,
    /// `x-internxt-workspace` header value (WorkspaceCredentialsDetails.tokenHeader).
    pub token: String,
    pub bucket: String,
    /// Network (bridge) basic-auth user/pass for workspace transfers.
    pub network_user: String,
    pub network_pass: String,
    /// Decrypted workspace mnemonic (WorkspaceUser.key after hybrid decrypt).
    pub mnemonic: String,
    /// Workspace root folder uuid (default browse/upload target).
    pub root_folder_id: String,
}

impl Credentials {
    /// Network basic-auth user: workspace network user when a workspace is active,
    /// else the personal bridge user.
    pub fn net_user(&self) -> &str {
        match &self.workspace {
            Some(w) => &w.network_user,
            None => &self.user.bridge_user,
        }
    }

    /// Network basic-auth password source (sha256'd downstream): workspace network
    /// pass when active, else the personal userId.
    pub fn net_pass(&self) -> &str {
        match &self.workspace {
            Some(w) => &w.network_pass,
            None => &self.user.user_id,
        }
    }

    /// Active bucket: workspace bucket when active, else personal bucket.
    pub fn bucket(&self) -> &str {
        match &self.workspace {
            Some(w) => &w.bucket,
            None => &self.user.bucket,
        }
    }

    /// Active mnemonic for file-key derivation: workspace mnemonic when active.
    pub fn mnemonic(&self) -> &str {
        match &self.workspace {
            Some(w) => &w.mnemonic,
            None => &self.user.mnemonic,
        }
    }

    /// Default root folder: workspace root when active, else personal root.
    pub fn root_folder(&self) -> &str {
        match &self.workspace {
            Some(w) => &w.root_folder_id,
            None => &self.user.root_folder_id,
        }
    }

    /// Active workspace uuid, if any.
    pub fn workspace_id(&self) -> Option<&str> {
        self.workspace.as_ref().map(|w| w.id.as_str())
    }
}

/// Space usage breakdown (`GET /users/usage`). Bytes.
#[derive(Deserialize, Debug, Clone, Default)]
pub struct SpaceUsage {
    #[serde(default)]
    pub drive: u64,
    #[serde(default)]
    pub backups: u64,
    #[serde(default)]
    pub total: u64,
}

// ---- Network (bridge) DTOs ----

#[derive(Deserialize, Debug)]
pub struct StartUploadResponse {
    pub uploads: Vec<UploadSlot>,
}

#[derive(Deserialize, Debug)]
pub struct UploadSlot {
    pub uuid: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub urls: Option<Vec<String>>,
    #[serde(rename = "UploadId", default)]
    pub upload_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct FinishUploadResponse {
    pub id: String,
}

#[derive(Deserialize, Debug)]
pub struct DownloadLinksResponse {
    pub index: String,
    pub shards: Vec<DownloadShard>,
    #[serde(default)]
    pub version: Option<u32>,
    pub size: u64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct DownloadShard {
    pub index: i64,
    pub url: String,
    /// Ciphertext byte length of this shard. Shards concatenate (ordered by
    /// `index`) into one continuous CTR stream, so this lets a range request
    /// skip whole shards and byte-range the boundary ones.
    #[serde(default)]
    pub size: u64,
}

// ---- Drive DTOs ----

#[derive(Deserialize, Debug)]
pub struct DriveFileData {
    pub uuid: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(rename = "fileId", default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub size: SizeField,
    #[serde(rename = "plainName", default)]
    pub plain_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type", default)]
    pub file_type: Option<String>,
}

/// A thumbnail as returned inside a folder-content file listing (`files[].thumbnails[]`
/// of `/folders/content/{uuid}/files`; the `/meta` endpoint omits it). Enough to
/// download it (bucket + network file id) and describe it.
#[derive(Deserialize, Debug, Clone)]
pub struct ThumbnailMeta {
    #[serde(default)]
    pub id: u64,
    #[serde(rename = "bucket_id", default)]
    pub bucket_id: String,
    #[serde(rename = "bucket_file", default)]
    pub bucket_file: String,
    #[serde(rename = "type", default)]
    pub thumbnail_type: String,
    #[serde(default)]
    pub size: SizeField,
    #[serde(rename = "max_width", default)]
    pub max_width: u32,
    #[serde(rename = "max_height", default)]
    pub max_height: u32,
}

/// Response of `POST /files/thumbnail` (og `Thumbnail`). We only need it to
/// deserialize successfully; the useful bits are the network `bucket_file` and id.
#[derive(Deserialize, Debug)]
pub struct Thumbnail {
    #[serde(default)]
    pub id: u64,
    #[serde(rename = "file_id", default)]
    pub file_id: u64,
    #[serde(rename = "bucket_file", default)]
    pub bucket_file: String,
    #[serde(rename = "type", default)]
    pub thumbnail_type: String,
    #[serde(default)]
    pub size: SizeField,
}

/// Size comes back as a number or a numeric string depending on endpoint.
#[derive(Debug, Default, Clone)]
pub struct SizeField(pub u64);

impl<'de> Deserialize<'de> for SizeField {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        let n = match v {
            serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
            serde_json::Value::String(s) => s.parse().map_err(D::Error::custom)?,
            serde_json::Value::Null => 0,
            _ => return Err(D::Error::custom("invalid size")),
        };
        Ok(SizeField(n))
    }
}

