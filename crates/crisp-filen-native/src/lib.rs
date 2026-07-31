//! Native, synchronous Filen protocol client.
//!
//! The crypto follows FilenCloudDienste's MIT Go SDK.  The public session is
//! intentionally serializable as one opaque value so callers can put it in an
//! OS keychain rather than in a plaintext drive configuration.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use md5::{Digest as Md5Digest, Md5};
use pbkdf2::pbkdf2_hmac;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Sha256, Sha512};

pub const DEFAULT_GATEWAY_URL: &str = "https://gateway.filen.io";
pub const DEFAULT_INGEST_URL: &str = "https://ingest.filen.io";
pub const DEFAULT_EGEST_URL: &str = "https://egest.filen.io";
pub const CHUNK_SIZE: usize = 1024 * 1024;

pub type AuthVersion = u8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataVersion {
    V1,
    V2,
    V3,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FilenSession {
    pub gateway_url: String,
    pub ingest_url: String,
    pub egest_url: String,
    pub email: String,
    pub api_key: String,
    pub auth_version: AuthVersion,
    pub file_encryption_version: u8,
    pub metadata_encryption_version: u8,
    pub root_folder_uuid: String,
    pub master_keys: Vec<Vec<u8>>,
    pub dek: Option<[u8; 32]>,
    pub kek: Option<[u8; 32]>,
    pub private_key: Option<Vec<u8>>,
    pub hmac_key: Option<[u8; 32]>,
}

impl FilenSession {
    pub fn encode(&self) -> Result<String> {
        serde_json::to_string(self).context("serializing Filen session")
    }
    pub fn decode(value: &str) -> Result<Self> {
        serde_json::from_str(value).context("parsing Filen session")
    }
}

fn evp_bytes_to_key(key: &[u8], salt: &[u8]) -> ([u8; 32], [u8; 16]) {
    let mut material = Vec::with_capacity(48);
    let mut previous = Vec::new();
    while material.len() < 48 {
        let mut h = Md5::new();
        h.update(&previous);
        h.update(key);
        h.update(salt);
        previous = h.finalize().to_vec();
        material.extend_from_slice(&previous);
    }
    (
        material[..32].try_into().unwrap(),
        material[32..48].try_into().unwrap(),
    )
}

/// Legacy v1 metadata decryptor for `U2FsdGVk...` OpenSSL envelopes.
pub fn decrypt_v1_metadata(encoded: &str, key: &[u8]) -> Result<String> {
    use cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    let raw = STANDARD.decode(encoded).context("decoding v1 metadata")?;
    anyhow::ensure!(
        raw.len() >= 16 && &raw[..8] == b"Salted__",
        "invalid v1 metadata envelope"
    );
    type Dec = cbc::Decryptor<aes::Aes256>;
    let (k, iv) = evp_bytes_to_key(key, &raw[8..16]);
    let mut data = raw[16..].to_vec();
    let plain = Dec::new((&k).into(), (&iv).into())
        .decrypt_padded_mut::<Pkcs7>(&mut data)
        .map_err(|_| anyhow!("invalid v1 metadata padding"))?;
    String::from_utf8(plain.to_vec()).context("v1 metadata is not UTF-8")
}

fn gcm(key: &[u8; 32], nonce: &[u8; 12], data: &[u8]) -> Result<Vec<u8>> {
    Aes256Gcm::new_from_slice(key)
        .map_err(|_| anyhow!("invalid AES-256 key"))?
        .decrypt(Nonce::from_slice(nonce), data)
        .map_err(|_| anyhow!("Filen AES-GCM authentication failed"))
}

fn gcm_encrypt(key: &[u8; 32], nonce: &[u8; 12], data: &[u8]) -> Result<Vec<u8>> {
    Aes256Gcm::new_from_slice(key)
        .map_err(|_| anyhow!("invalid AES-256 key"))?
        .encrypt(Nonce::from_slice(nonce), data)
        .map_err(|_| anyhow!("Filen AES-GCM encryption failed"))
}

pub fn pbkdf2_login(password: &str, salt: &str) -> ([u8; 64], String) {
    let mut raw = [0u8; 64];
    pbkdf2_hmac::<Sha512>(password.as_bytes(), salt.as_bytes(), 200_000, &mut raw);
    let derived = hex::encode(raw);
    let mut h = Sha512::new();
    h.update(derived[64..].as_bytes());
    (raw, hex::encode(h.finalize()))
}

pub fn argon2id_login(password: &str, salt_hex: &str) -> Result<([u8; 32], String)> {
    let salt = hex::decode(salt_hex).context("decoding Argon2 salt")?;
    let params =
        Params::new(65_536, 3, 4, Some(64)).map_err(|e| anyhow!("Argon2 parameters: {e:?}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut raw = [0u8; 64];
    argon
        .hash_password_into(password.as_bytes(), &salt, &mut raw)
        .map_err(|e| anyhow!("deriving Argon2 login key: {e:?}"))?;
    let derived = hex::encode(raw);
    let key = hex::decode(&derived[..64])?
        .try_into()
        .map_err(|_| anyhow!("invalid Argon2 KEK"))?;
    Ok((key, derived[64..].to_owned()))
}

pub fn v2_master_key(raw: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2_hmac::<Sha512>(raw, raw, 1, &mut out);
    out
}

pub fn v2_decrypt_metadata(encoded: &str, raw_key: &[u8]) -> Result<String> {
    anyhow::ensure!(
        encoded.starts_with("002") && encoded.len() >= 15,
        "invalid v2 metadata"
    );
    let key = v2_master_key(raw_key);
    let nonce: [u8; 12] = encoded.as_bytes()[3..15].try_into().unwrap();
    let data = STANDARD
        .decode(&encoded[15..])
        .context("decoding v2 metadata")?;
    String::from_utf8(gcm(&key, &nonce, &data)?).context("v2 metadata is not UTF-8")
}

pub fn v2_encrypt_metadata(plain: &str, raw_key: &[u8], nonce: [u8; 12]) -> Result<String> {
    let key = v2_master_key(raw_key);
    Ok(format!(
        "002{}{}",
        String::from_utf8_lossy(&nonce),
        STANDARD.encode(gcm_encrypt(&key, &nonce, plain.as_bytes())?)
    ))
}

pub fn v3_decrypt_metadata(encoded: &str, key: &[u8; 32]) -> Result<String> {
    anyhow::ensure!(
        encoded.starts_with("003") && encoded.len() >= 27,
        "invalid v3 metadata"
    );
    let nonce: [u8; 12] = hex::decode(&encoded[3..27])?
        .try_into()
        .map_err(|_| anyhow!("invalid v3 nonce"))?;
    String::from_utf8(gcm(key, &nonce, &STANDARD.decode(&encoded[27..])?)?)
        .context("v3 metadata is not UTF-8")
}

pub fn encrypt_v3_metadata(plain: &str, key: &[u8; 32], nonce: [u8; 12]) -> Result<String> {
    Ok(format!(
        "003{}{}",
        hex::encode(nonce),
        STANDARD.encode(gcm_encrypt(key, &nonce, plain.as_bytes())?)
    ))
}

pub fn decrypt_metadata(
    encoded: &str,
    master_key: Option<&[u8]>,
    dek: Option<&[u8; 32]>,
) -> Result<String> {
    if encoded.starts_with("U2FsdGVk") {
        return decrypt_v1_metadata(
            encoded,
            master_key.ok_or_else(|| anyhow!("missing v1 master key"))?,
        );
    }
    if encoded.starts_with("002") {
        return v2_decrypt_metadata(
            encoded,
            master_key.ok_or_else(|| anyhow!("missing v2 master key"))?,
        );
    }
    if encoded.starts_with("003") {
        return v3_decrypt_metadata(encoded, dek.ok_or_else(|| anyhow!("missing v3 DEK"))?);
    }
    Err(anyhow!("unknown Filen metadata format"))
}

pub fn encrypt_file_chunk(data: &[u8], key: &[u8; 32], nonce: [u8; 12]) -> Result<Vec<u8>> {
    let mut out = nonce.to_vec();
    out.extend(gcm_encrypt(key, &nonce, data)?);
    Ok(out)
}

pub fn decrypt_file_chunk(data: &[u8], key: &[u8; 32]) -> Result<Vec<u8>> {
    anyhow::ensure!(data.len() >= 12, "encrypted Filen chunk is too short");
    let nonce: [u8; 12] = data[..12].try_into().unwrap();
    gcm(key, &nonce, &data[12..])
}

pub fn v2_hash(data: &[u8]) -> String {
    let mut inner = Sha512::new();
    inner.update(data);
    let mut outer = Sha1::new();
    outer.update(hex::encode(inner.finalize()).as_bytes());
    hex::encode(outer.finalize())
}

pub fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeItem {
    pub uuid: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub parent: String,
    pub file_key: Option<[u8; 32]>,
    pub bucket: String,
    pub region: String,
    pub chunks: u64,
    pub version: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CryptoMode {
    V2,
    V3,
}

pub struct FilenNativeClient {
    http: reqwest::blocking::Client,
    gateway_url: String,
    ingest_url: String,
    egest_url: String,
    api_key: String,
    mode: CryptoMode,
    master_key: Option<Vec<u8>>,
    dek: Option<[u8; 32]>,
    hmac_key: Option<[u8; 32]>,
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    status: bool,
    #[serde(default)]
    message: String,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct AuthInfo {
    #[serde(rename = "authVersion")]
    auth_version: u8,
    salt: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    #[serde(rename = "apiKey")]
    api_key: String,
}

#[derive(Debug, Deserialize)]
struct RootResponse {
    uuid: String,
}

#[derive(Debug, Deserialize)]
struct DirContent {
    #[serde(default)]
    uploads: Vec<RemoteUpload>,
    #[serde(default)]
    folders: Vec<RemoteFolder>,
}

#[derive(Debug, Deserialize)]
struct RemoteUpload {
    uuid: String,
    metadata: String,
    parent: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    bucket: String,
    #[serde(default)]
    region: String,
    #[serde(default)]
    chunks: u64,
    #[serde(default)]
    version: u8,
}

#[derive(Debug, Deserialize)]
struct RemoteFolder {
    uuid: String,
    #[serde(rename = "name")]
    metadata: String,
    parent: String,
}

impl FilenNativeClient {
    fn new_inner(session: &FilenSession) -> Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder().build()?,
            gateway_url: session.gateway_url.trim_end_matches('/').into(),
            ingest_url: session.ingest_url.trim_end_matches('/').into(),
            egest_url: session.egest_url.trim_end_matches('/').into(),
            api_key: session.api_key.clone(),
            mode: if session.auth_version >= 3 {
                CryptoMode::V3
            } else {
                CryptoMode::V2
            },
            master_key: session.master_keys.first().cloned(),
            dek: session.dek,
            hmac_key: session.hmac_key,
        })
    }

    pub fn from_session(session: &FilenSession) -> Result<Self> {
        Self::new_inner(session)
    }

    fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        url: String,
        body: Option<serde_json::Value>,
    ) -> Result<T> {
        let mut request = self.http.request(method, &url).bearer_auth(&self.api_key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .with_context(|| format!("requesting Filen {url}"))?;
        let status = response.status();
        let text = response.text()?;
        anyhow::ensure!(status.is_success(), "Filen HTTP {status}: {text}");
        let envelope: ApiEnvelope<T> = serde_json::from_str(&text)
            .with_context(|| format!("decoding Filen response: {text}"))?;
        anyhow::ensure!(envelope.status, "Filen API error: {}", envelope.message);
        envelope
            .data
            .ok_or_else(|| anyhow!("Filen response has no data"))
    }

    fn crypto_metadata(&self, value: &str) -> Result<String> {
        match self.mode {
            CryptoMode::V2 => v2_decrypt_metadata(
                value,
                self.master_key
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing master key"))?,
            ),
            CryptoMode::V3 => v3_decrypt_metadata(
                value,
                self.dek.as_ref().ok_or_else(|| anyhow!("missing DEK"))?,
            ),
        }
    }

    fn hash_name(&self, name: &str) -> Result<String> {
        if let Some(key) = self.hmac_key {
            use hmac::{Hmac, Mac};
            let mut h = <Hmac<Sha256> as Mac>::new_from_slice(&key)
                .map_err(|_| anyhow!("invalid HMAC key"))?;
            h.update(name.to_lowercase().as_bytes());
            return Ok(hex::encode(h.finalize().into_bytes()));
        }
        Ok(v2_hash(name.to_lowercase().as_bytes()))
    }

    pub fn list_folder(&self, uuid: &str) -> Result<Vec<NativeItem>> {
        let content: DirContent = self.request(
            reqwest::Method::POST,
            format!("{}/v3/dir/content", self.gateway_url),
            Some(serde_json::json!({"uuid": uuid})),
        )?;
        let mut items = Vec::with_capacity(content.uploads.len() + content.folders.len());
        for folder in content.folders {
            let name = self.crypto_metadata(&folder.metadata)?;
            let name = serde_json::from_str::<serde_json::Value>(&name)
                .ok()
                .and_then(|v| v.get("name").and_then(|n| n.as_str().map(str::to_owned)))
                .unwrap_or(name);
            items.push(NativeItem {
                uuid: folder.uuid,
                name,
                is_dir: true,
                size: 0,
                parent: folder.parent,
                file_key: None,
                bucket: String::new(),
                region: String::new(),
                chunks: 0,
                version: 0,
            });
        }
        for upload in content.uploads {
            let metadata = self.crypto_metadata(&upload.metadata)?;
            let value: serde_json::Value = serde_json::from_str(&metadata)?;
            let name = value
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let key = value
                .get("key")
                .and_then(|v| v.as_str())
                .and_then(|s| decode_file_key(s).ok());
            items.push(NativeItem {
                uuid: upload.uuid,
                name,
                is_dir: false,
                size: value
                    .get("size")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(upload.size),
                parent: upload.parent,
                file_key: key,
                bucket: upload.bucket,
                region: upload.region,
                chunks: upload.chunks,
                version: upload.version,
            });
        }
        Ok(items)
    }

    pub fn resolve_path(
        &self,
        session: &FilenSession,
        path: &std::path::Path,
    ) -> Result<NativeItem> {
        let root = NativeItem {
            uuid: session.root_folder_uuid.clone(),
            name: String::new(),
            is_dir: true,
            size: 0,
            parent: String::new(),
            file_key: None,
            bucket: String::new(),
            region: String::new(),
            chunks: 0,
            version: 0,
        };
        let mut current = root;
        for component in path.components() {
            let part = component.as_os_str().to_string_lossy();
            if part.is_empty() || part == "." || part == "/" {
                continue;
            }
            current = self
                .list_folder(&current.uuid)?
                .into_iter()
                .find(|item| item.name == part)
                .ok_or_else(|| anyhow!("Filen path not found: {path:?}"))?;
        }
        Ok(current)
    }

    pub fn login(
        gateway_url: &str,
        email: &str,
        password: &str,
        tfa: Option<&str>,
    ) -> Result<FilenSession> {
        let http = reqwest::blocking::Client::new();
        let base = gateway_url.trim_end_matches('/');
        let auth: ApiEnvelope<AuthInfo> = http
            .post(format!("{base}/v3/auth/info"))
            .json(&serde_json::json!({"email": email}))
            .send()?
            .json()?;
        let auth = auth
            .data
            .ok_or_else(|| anyhow!("Filen auth info missing"))?;
        let (auth_password, master, dek, hmac) = if auth.auth_version >= 3 {
            let (k, p) = argon2id_login(password, &auth.salt)?;
            (p, None, Some(k), None)
        } else {
            let (raw, p) = pbkdf2_login(password, &auth.salt);
            let derived = hex::encode(raw);
            (p, Some(derived[..64].as_bytes().to_vec()), None, None)
        };
        let login: ApiEnvelope<LoginResponse> = http.post(format!("{base}/v3/login")).json(&serde_json::json!({"email": email, "password": auth_password, "authVersion": auth.auth_version, "twoFactorCode": tfa.unwrap_or("")})).send()?.json()?;
        let api = login
            .data
            .ok_or_else(|| anyhow!("Filen login response missing"))?;
        let mut session = FilenSession {
            gateway_url: base.into(),
            ingest_url: DEFAULT_INGEST_URL.into(),
            egest_url: DEFAULT_EGEST_URL.into(),
            email: email.into(),
            api_key: api.api_key,
            auth_version: auth.auth_version,
            file_encryption_version: if auth.auth_version >= 3 { 3 } else { 2 },
            metadata_encryption_version: if auth.auth_version >= 3 { 3 } else { 2 },
            root_folder_uuid: String::new(),
            master_keys: master.into_iter().collect(),
            dek,
            kek: None,
            private_key: None,
            hmac_key: hmac,
        };
        let client = Self::from_session(&session)?;
        if auth.auth_version >= 3 {
            #[derive(Deserialize)]
            struct DekResponse {
                dek: String,
            }
            let encrypted: DekResponse =
                client.request(reqwest::Method::GET, format!("{base}/v3/user/dek"), None)?;
            let kek = session
                .dek
                .take()
                .ok_or_else(|| anyhow!("missing v3 KEK"))?;
            let dek_hex = v3_decrypt_metadata(&encrypted.dek, &kek)?;
            session.dek = Some(
                hex::decode(dek_hex)?
                    .try_into()
                    .map_err(|_| anyhow!("invalid v3 DEK"))?,
            );
        }
        let client = Self::from_session(&session)?;
        let root: RootResponse = client.request(
            reqwest::Method::GET,
            format!("{base}/v3/user/baseFolder"),
            None,
        )?;
        session.root_folder_uuid = root.uuid;
        Ok(session)
    }

    fn encrypt_metadata(&self, plain: &str) -> Result<String> {
        let mut random = [0u8; 12];
        getrandom::getrandom(&mut random)?;
        let mut nonce = [0u8; 12];
        for (slot, value) in nonce.iter_mut().zip(random) {
            *slot = b'A' + (value % 26);
        }
        match self.mode {
            CryptoMode::V2 => v2_encrypt_metadata(
                plain,
                self.master_key
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing master key"))?,
                nonce,
            ),
            CryptoMode::V3 => encrypt_v3_metadata(
                plain,
                self.dek.as_ref().ok_or_else(|| anyhow!("missing DEK"))?,
                nonce,
            ),
        }
    }

    fn new_file_key(&self) -> Result<([u8; 32], String)> {
        let mut random = [0u8; 32];
        getrandom::getrandom(&mut random)?;
        let mut key = [0u8; 32];
        if self.mode == CryptoMode::V2 {
            for (slot, value) in key.iter_mut().zip(random) {
                *slot = b'A' + (value % 26);
            }
        } else {
            key = random;
        }
        Ok((
            key,
            match self.mode {
                CryptoMode::V2 => String::from_utf8(key.to_vec())
                    .unwrap_or_else(|_| "FilenNativeFileKey000000000000000".into()),
                CryptoMode::V3 => hex::encode(key),
            },
        ))
    }

    pub fn create_folder(&self, parent: &str, name: &str) -> Result<String> {
        let metadata =
            serde_json::json!({"name": name, "creation": chrono::Utc::now().timestamp_millis()})
                .to_string();
        #[derive(Deserialize)]
        struct Created {
            uuid: String,
        }
        let value: Created = self.request(reqwest::Method::POST, format!("{}/v3/dir/create", self.gateway_url), Some(serde_json::json!({"uuid": random_uuid(), "name": self.encrypt_metadata(&metadata)?, "nameHashed": self.hash_name(name)?, "parent": parent})))?;
        Ok(value.uuid)
    }

    pub fn trash(&self, uuid: &str, kind: &str) -> Result<()> {
        let endpoint = if kind == "folder" {
            "v3/dir/trash"
        } else {
            "v3/file/trash"
        };
        let _: serde_json::Value = self.request(
            reqwest::Method::POST,
            format!("{}/{endpoint}", self.gateway_url),
            Some(serde_json::json!({"uuid": uuid})),
        )?;
        Ok(())
    }

    pub fn upload_file(&self, parent: &str, name: &str, mime: &str, data: &[u8]) -> Result<()> {
        let (key, key_string) = self.new_file_key()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);
        let metadata = serde_json::json!({"name": name, "size": data.len(), "mime": mime, "key": key_string, "creation": chrono::Utc::now().timestamp_millis(), "lastModified": chrono::Utc::now().timestamp_millis(), "blake3": hasher.finalize().to_hex().to_string()}).to_string();
        let uuid = random_uuid();
        let upload_key = random_uuid().replace('-', "");
        let chunks = data.len().div_ceil(CHUNK_SIZE);
        if data.is_empty() {
            let _: serde_json::Value = self.request(reqwest::Method::POST, format!("{}/v3/upload/empty", self.gateway_url), Some(serde_json::json!({"uuid": uuid, "name": self.encrypt_metadata(name)?, "nameHashed": self.hash_name(name)?, "size": self.encrypt_metadata("0")?, "parent": parent, "mime": self.encrypt_metadata(mime)?, "metadata": self.encrypt_metadata(&metadata)?, "version": if self.mode == CryptoMode::V3 {3} else {2}})))?;
            return Ok(());
        }
        let mut bucket = String::new();
        let mut region = String::new();
        for (index, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
            let mut nonce = [0u8; 12];
            getrandom::getrandom(&mut nonce)?;
            let encrypted = encrypt_file_chunk(chunk, &key, nonce)?;
            let hash = hex::encode(Sha512::digest(&encrypted));
            #[derive(Deserialize)]
            struct Uploaded {
                bucket: String,
                region: String,
            }
            let value: Uploaded = self.request_raw(reqwest::Method::POST, format!("{}/v3/upload?uuid={uuid}&index={index}&parent={parent}&uploadKey={upload_key}&hash={hash}", self.ingest_url), &encrypted)?;
            bucket = value.bucket;
            region = value.region;
        }
        let _: serde_json::Value = self.request(reqwest::Method::POST, format!("{}/v3/upload/done", self.gateway_url), Some(serde_json::json!({"uuid": uuid, "name": self.encrypt_metadata(name)?, "nameHashed": self.hash_name(name)?, "size": self.encrypt_metadata(&data.len().to_string())?, "parent": parent, "mime": self.encrypt_metadata(mime)?, "metadata": self.encrypt_metadata(&metadata)?, "version": if self.mode == CryptoMode::V3 {3} else {2}, "chunks": chunks, "rm": "0", "uploadKey": upload_key, "bucket": bucket, "region": region})))?;
        Ok(())
    }

    fn request_raw<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        url: String,
        bytes: &[u8],
    ) -> Result<T> {
        let response = self
            .http
            .request(method, &url)
            .bearer_auth(&self.api_key)
            .body(bytes.to_vec())
            .send()?;
        let status = response.status();
        let text = response.text()?;
        anyhow::ensure!(status.is_success(), "Filen upload HTTP {status}: {text}");
        let envelope: ApiEnvelope<T> = serde_json::from_str(&text)?;
        anyhow::ensure!(
            envelope.status,
            "Filen upload API error: {}",
            envelope.message
        );
        envelope
            .data
            .ok_or_else(|| anyhow!("Filen upload response has no data"))
    }

    pub fn download_file(&self, item: &NativeItem) -> Result<Vec<u8>> {
        let key = item
            .file_key
            .ok_or_else(|| anyhow!("Filen item has no file key"))?;
        let mut plain = Vec::new();
        for index in 0..item.chunks.max(1) {
            let response = self
                .http
                .get(format!(
                    "{}/{}/{}/{}/{}",
                    self.egest_url, item.region, item.bucket, item.uuid, index
                ))
                .bearer_auth(&self.api_key)
                .send()?;
            anyhow::ensure!(
                response.status().is_success(),
                "Filen download HTTP {}",
                response.status()
            );
            let encrypted = response.bytes()?;
            plain.extend(decrypt_file_chunk(&encrypted, &key)?);
        }
        plain.truncate(item.size as usize);
        Ok(plain)
    }
}

fn decode_file_key(value: &str) -> Result<[u8; 32]> {
    if value.len() == 64 {
        return Ok(hex::decode(value)?
            .try_into()
            .map_err(|_| anyhow!("invalid file key"))?);
    }
    Ok(value
        .as_bytes()
        .try_into()
        .map_err(|_| anyhow!("invalid v2 file key"))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_login_matches_reference_formula() {
        let (_, password) = pbkdf2_login("password", "salt");
        assert_eq!(password.len(), 128);
        assert_eq!(password, "65773430407d1049af0d42763b5bc2bc8f60ab7f4143d98f7f57a877a951801d38054187db31989a02e83e7a0f5f1a9085a85197d2846b7df28053b46aed4790");
    }

    #[test]
    fn v3_metadata_round_trips_reference_shape() {
        let key = [7u8; 32];
        let nonce = [9u8; 12];
        let encoded = encrypt_v3_metadata("hello", &key, nonce).unwrap();
        assert!(encoded.starts_with("003"));
        assert_eq!(v3_decrypt_metadata(&encoded, &key).unwrap(), "hello");
    }

    #[test]
    fn v2_metadata_round_trips_reference_shape() {
        let raw_key = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let encoded = v2_encrypt_metadata("hello", raw_key, *b"abcdefghijkl").unwrap();
        assert!(encoded.starts_with("002"));
        assert_eq!(v2_decrypt_metadata(&encoded, raw_key).unwrap(), "hello");
    }

    #[test]
    fn session_serializes_as_one_blob() {
        let session = FilenSession {
            gateway_url: "https://gateway.filen.io".into(),
            ingest_url: "https://ingest.filen.io".into(),
            egest_url: "https://egest.filen.io".into(),
            email: "user@example.test".into(),
            api_key: "secret".into(),
            auth_version: 2,
            file_encryption_version: 2,
            metadata_encryption_version: 2,
            root_folder_uuid: "root".into(),
            master_keys: vec![b"key".to_vec()],
            dek: None,
            kek: None,
            private_key: None,
            hmac_key: None,
        };
        assert_eq!(
            FilenSession::decode(&session.encode().unwrap()).unwrap(),
            session
        );
    }

    #[test]
    fn file_chunk_round_trips_with_nonce_prefix() {
        let key = [3u8; 32];
        let encrypted = encrypt_file_chunk(b"hello", &key, [1u8; 12]).unwrap();
        assert_eq!(decrypt_file_chunk(&encrypted, &key).unwrap(), b"hello");
    }
}
