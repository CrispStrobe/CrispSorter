//! Pure Internxt file crypto used by the native drive client (P33.1).
//!
//! The formulas mirror the protocol implementation in Internxt's clients:
//! BIP-39 seed derivation, SHA-512 key derivation, and AES-256-CTR with the
//! first 16 bytes of the random file index as the IV.  This module deliberately
//! has no HTTP or credential state, which makes the wire-critical part easy to
//! test before the authenticated drive wrapper is added.

use aes::Aes256;
use anyhow::{anyhow, Context, Result};
use cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use md5::{Digest as Md5Digest, Md5};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Sha512};
use unicode_normalization::UnicodeNormalization;

type Aes256Ctr = Ctr128BE<Aes256>;
type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

const OPENSSL_MAGIC: &[u8; 8] = b"Salted__";
const INTERNXT_APP_SECRET: &str = "6KYQBP847D4ATSFA";
pub const DEFAULT_DRIVE_API_URL: &str = "https://gateway.internxt.com/drive";
const INTERNXT_NETWORK_URL: &str = "https://gateway.internxt.com/network";

/// OpenSSL's legacy EVP_BytesToKey derivation with MD5, as used by the
/// Internxt CLI for the `/auth/login` salt and password-hash envelope.
fn evp_bytes_to_key(secret: &[u8], salt: &[u8; 8]) -> ([u8; 32], [u8; 16]) {
    let mut material = Vec::with_capacity(48);
    let mut previous = Vec::new();
    while material.len() < 48 {
        let mut digest = Md5::new();
        digest.update(&previous);
        digest.update(secret);
        digest.update(salt);
        previous = digest.finalize().to_vec();
        material.extend_from_slice(&previous);
    }
    let mut key = [0u8; 32];
    let mut iv = [0u8; 16];
    key.copy_from_slice(&material[..32]);
    iv.copy_from_slice(&material[32..48]);
    (key, iv)
}

/// Encrypt text in the hex-encoded `Salted__` envelope used by Internxt.
pub fn encrypt_text(text: &[u8], secret: &str) -> Result<String> {
    let mut salt = [0u8; 8];
    getrandom::getrandom(&mut salt)
        .map_err(|error| anyhow!("generating Internxt crypto salt: {error}"))?;
    let (key, iv) = evp_bytes_to_key(secret.as_bytes(), &salt);
    let mut ciphertext = vec![0u8; text.len() + 16];
    let encrypted = Aes256CbcEnc::new((&key).into(), (&iv).into())
        .encrypt_padded_b2b_mut::<Pkcs7>(text, &mut ciphertext)
        .map_err(|_| anyhow!("Internxt AES-CBC encryption failed"))?;
    let mut envelope = Vec::with_capacity(16 + encrypted.len());
    envelope.extend_from_slice(OPENSSL_MAGIC);
    envelope.extend_from_slice(&salt);
    envelope.extend_from_slice(encrypted);
    Ok(hex::encode(envelope))
}

/// Decrypt an Internxt `Salted__` envelope.
pub fn decrypt_text(encoded: &str, secret: &str) -> Result<Vec<u8>> {
    let envelope = hex::decode(encoded).context("decoding Internxt encrypted text")?;
    if envelope.len() < 16 || &envelope[..8] != OPENSSL_MAGIC {
        return Err(anyhow!("invalid Internxt encrypted text envelope"));
    }
    let salt: [u8; 8] = envelope[8..16]
        .try_into()
        .expect("validated eight-byte salt");
    let (key, iv) = evp_bytes_to_key(secret.as_bytes(), &salt);
    let mut plaintext = envelope[16..].to_vec();
    let decrypted = Aes256CbcDec::new((&key).into(), (&iv).into())
        .decrypt_padded_mut::<Pkcs7>(&mut plaintext)
        .map_err(|_| anyhow!("Internxt AES-CBC decryption failed"))?;
    Ok(decrypted.to_vec())
}

/// Complete password transport step after `/auth/login` returns `sKey`.
pub fn login_password_payload(
    password: &str,
    encrypted_salt: &str,
    app_secret: &str,
) -> Result<String> {
    let salt = String::from_utf8(decrypt_text(encrypted_salt, app_secret)?)
        .context("Internxt login salt is not UTF-8")?;
    let hash = password_hash(password, &salt)?;
    encrypt_text(hash.as_bytes(), app_secret)
}

/// All state needed after a successful Internxt login. This is serialized
/// into a caller-owned secret store (CrispSorter uses the OS keychain), never
/// into the drive configuration file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternxtSession {
    pub drive_api_url: String,
    pub network_url: String,
    pub email: String,
    pub token: String,
    pub new_token: String,
    pub mnemonic: String,
    pub user_id: String,
    pub root_folder_id: String,
    pub bridge_user: String,
    pub bucket_id: String,
}

impl InternxtSession {
    pub fn encode(&self) -> Result<String> {
        serde_json::to_string(self).context("serializing Internxt session")
    }

    pub fn decode(serialized: &str) -> Result<Self> {
        serde_json::from_str(serialized).context("parsing Internxt session")
    }

    /// Password for the S3-compatible bridge service, derived from the
    /// account id rather than stored as a second secret.
    pub fn bridge_pass(&self) -> String {
        hex::encode(Sha256::digest(self.user_id.as_bytes()))
    }

    pub fn bucket_bytes(&self) -> Result<[u8; 12]> {
        let bytes = hex::decode(&self.bucket_id).context("decoding Internxt bucket id")?;
        bytes
            .try_into()
            .map_err(|_| anyhow!("Internxt bucket id must contain 12 bytes"))
    }
}

/// Derive the 64-byte BIP-39 seed for a mnemonic and optional passphrase.
///
/// BIP-39 specifies PBKDF2-HMAC-SHA512 with 2048 rounds and the salt prefix
/// `mnemonic`. The clients pass an empty passphrase for Internxt accounts.
pub fn mnemonic_seed(mnemonic: &str, passphrase: &str) -> [u8; 64] {
    let mnemonic = mnemonic.nfkd().collect::<String>();
    let passphrase = passphrase.nfkd().collect::<String>();
    let salt = format!("mnemonic{passphrase}");
    let mut seed = [0u8; 64];
    pbkdf2::pbkdf2_hmac::<Sha512>(mnemonic.as_bytes(), salt.as_bytes(), 2048, &mut seed);
    seed
}

/// SHA-512 over two byte strings, matching the Dart/Python clients.
pub fn deterministic_key(left: &[u8], right: &[u8]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// Derive the 32-byte AES key for a file.
pub fn file_key(mnemonic: &str, bucket_id: &[u8; 12], index: &[u8; 32]) -> [u8; 32] {
    let seed = mnemonic_seed(mnemonic, "");
    let bucket_key = deterministic_key(&seed, bucket_id);
    let file_key = deterministic_key(&bucket_key[..32], index);
    file_key[..32]
        .try_into()
        .expect("SHA-512 has at least 32 bytes")
}

/// Encrypt or decrypt a complete file payload. AES-CTR is symmetric.
pub fn crypt(data: &mut [u8], mnemonic: &str, bucket_id: &[u8; 12], index: &[u8; 32]) {
    let key = file_key(mnemonic, bucket_id, index);
    let mut cipher = Aes256Ctr::new((&key).into(), (&index[..16]).into());
    cipher.apply_keystream(data);
}

/// Encrypt a payload and return the 32-byte file index plus ciphertext.
pub fn encrypt(data: &[u8], mnemonic: &str, bucket_id: &[u8; 12]) -> ([u8; 32], Vec<u8>) {
    let mut index = [0u8; 32];
    getrandom::getrandom(&mut index).expect("OS randomness unavailable");
    let mut encrypted = data.to_vec();
    crypt(&mut encrypted, mnemonic, bucket_id, &index);
    (index, encrypted)
}

/// PBKDF2-HMAC-SHA1 password hash used by `/auth/login/access`.
pub fn password_hash(password: &str, salt_hex: &str) -> anyhow::Result<String> {
    let salt = hex::decode(salt_hex)?;
    let mut hash = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), &salt, 10_000, &mut hash);
    Ok(hex::encode(hash))
}

/// A drive item returned by Internxt's folder-content endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeItem {
    pub name: String,
    pub uuid: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Debug, Deserialize)]
struct ContentPage {
    #[serde(default)]
    result: Vec<serde_json::Value>,
    #[serde(default)]
    folders: Vec<serde_json::Value>,
    #[serde(default)]
    files: Vec<serde_json::Value>,
}

/// Minimal authenticated Internxt gateway client. Authentication/session
/// creation is deliberately separate: the native drive will obtain a token
/// from the keychain-backed login flow, then use this client for ordinary
/// drive operations.
pub struct InternxtNativeClient {
    base_url: String,
    bearer_token: String,
    http: Client,
}

impl InternxtNativeClient {
    pub fn new(base_url: impl Into<String>, bearer_token: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        reqwest::Url::parse(&base_url)
            .with_context(|| format!("invalid Internxt URL: {base_url}"))?;
        Ok(Self {
            base_url,
            bearer_token: bearer_token.into(),
            http: Client::new(),
        })
    }

    /// Authenticate using Internxt's compatibility flow that does not upload
    /// fresh OpenPGP keys. Existing accounts accept this path and the server
    /// still returns the complete drive session, including the encrypted
    /// mnemonic. Accounts that require key registration receive the gateway's
    /// error instead of silently creating a partial session.
    pub fn login_without_keys(
        drive_api_url: &str,
        email: &str,
        password: &str,
        tfa_code: Option<&str>,
    ) -> Result<InternxtSession> {
        let http = Client::new();
        let drive_api_url = drive_api_url.trim_end_matches('/');
        let email = email.trim().to_lowercase();
        let security_url = format!("{drive_api_url}/auth/login");
        let security = http
            .post(&security_url)
            .header("content-type", "application/json")
            .header("internxt-client", "cli")
            .json(&serde_json::json!({"email": email}))
            .send()
            .context("requesting Internxt login security details")?;
        let security_status = security.status();
        let security_body = security
            .text()
            .context("reading Internxt login security details")?;
        if !security_status.is_success() {
            return Err(anyhow!(
                "Internxt login security returned {security_status}: {security_body}"
            ));
        }
        let encrypted_salt = serde_json::from_str::<serde_json::Value>(&security_body)?
            .get("sKey")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt login response has no sKey"))?
            .to_owned();
        let encrypted_password =
            login_password_payload(password, &encrypted_salt, INTERNXT_APP_SECRET)?;

        let access_url = format!("{drive_api_url}/auth/login/access");
        let access = http
            .post(&access_url)
            .header("content-type", "application/json")
            .header("internxt-client", "cli")
            .json(&serde_json::json!({
                "email": email,
                "password": encrypted_password,
                "tfa": tfa_code
            }))
            .send()
            .context("requesting Internxt login access")?;
        let access_status = access.status();
        let access_body = access.text().context("reading Internxt login access")?;
        if !access_status.is_success() {
            return Err(anyhow!(
                "Internxt login access returned {access_status}: {access_body}"
            ));
        }
        let access_json: serde_json::Value = serde_json::from_str(&access_body)?;
        let temporary_token = access_json
            .get("newToken")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt login access response has no newToken"))?;

        let refresh_url = format!("{drive_api_url}/users/refresh");
        let refresh = http
            .get(&refresh_url)
            .bearer_auth(temporary_token)
            .header("content-type", "application/json")
            .header("internxt-client", "cli")
            .send()
            .context("hydrating Internxt login session")?;
        let refresh_status = refresh.status();
        let refresh_body = refresh.text().context("reading Internxt login hydration")?;
        if !refresh_status.is_success() {
            return Err(anyhow!(
                "Internxt login hydration returned {refresh_status}: {refresh_body}"
            ));
        }
        let hydrated: serde_json::Value = serde_json::from_str(&refresh_body)?;
        let user = hydrated
            .get("user")
            .ok_or_else(|| anyhow!("Internxt login hydration has no user"))?;
        let text_field = |name: &str| -> Result<String> {
            user.get(name)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("Internxt login hydration has no {name}"))
        };
        let encrypted_mnemonic = text_field("mnemonic")?;
        let mnemonic = String::from_utf8(decrypt_text(&encrypted_mnemonic, password)?)
            .context("decrypting Internxt mnemonic")?;
        let user_id = text_field("userId")?;
        Ok(InternxtSession {
            drive_api_url: drive_api_url.to_owned(),
            network_url: INTERNXT_NETWORK_URL.to_owned(),
            email: text_field("email")?,
            token: hydrated
                .get("token")
                .and_then(|value| value.as_str())
                .unwrap_or(temporary_token)
                .to_owned(),
            new_token: hydrated
                .get("newToken")
                .and_then(|value| value.as_str())
                .unwrap_or(temporary_token)
                .to_owned(),
            mnemonic,
            user_id,
            root_folder_id: text_field("rootFolderId")?,
            bridge_user: text_field("bridgeUser")?,
            bucket_id: text_field("bucket")?,
        })
    }

    fn list_page(&self, folder_uuid: &str, kind: &str, offset: usize) -> Result<Vec<NativeItem>> {
        let url = format!("{}/folders/content/{}/{}", self.base_url, folder_uuid, kind);
        let mut url = reqwest::Url::parse(&url).context("building Internxt listing URL")?;
        url.query_pairs_mut()
            .append_pair("offset", &offset.to_string())
            .append_pair("limit", "50")
            .append_pair("sort", "plainName")
            .append_pair("direction", "ASC");
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.bearer_token)
            .send()
            .context("requesting Internxt folder contents")?;
        let status = response.status();
        let body = response
            .text()
            .context("reading Internxt folder response")?;
        if !status.is_success() {
            return Err(anyhow!("Internxt gateway returned {status}: {body}"));
        }
        let page: ContentPage = serde_json::from_str(&body)
            .with_context(|| format!("parsing Internxt folder response: {body}"))?;
        let values = if !page.result.is_empty() {
            page.result
        } else if kind == "folders" {
            page.folders
        } else {
            page.files
        };
        Ok(values
            .into_iter()
            .map(|item| NativeItem {
                name: item
                    .get("plainName")
                    .or_else(|| item.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_owned(),
                uuid: item
                    .get("uuid")
                    .or_else(|| item.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                is_dir: kind == "folders",
                size: item
                    .get("size")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
                    .unwrap_or(0),
            })
            .collect())
    }

    fn bearer_response(&self, method: reqwest::Method, url: &str) -> Result<Response> {
        self.http
            .request(method, url)
            .bearer_auth(&self.bearer_token)
            .header("internxt-client", "cli")
            .send()
            .with_context(|| format!("requesting Internxt drive endpoint: {url}"))
    }

    fn bridge_response(
        &self,
        method: reqwest::Method,
        url: &str,
        session: &InternxtSession,
        body: Option<Vec<u8>>,
    ) -> Result<Response> {
        let mut request = self
            .http
            .request(method, url)
            .basic_auth(&session.bridge_user, Some(session.bridge_pass()))
            .header("x-api-version", "2");
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body);
        }
        request
            .send()
            .with_context(|| format!("requesting Internxt network endpoint: {url}"))
    }

    pub fn file_metadata(&self, file_uuid: &str) -> Result<serde_json::Value> {
        let url = format!("{}/files/{file_uuid}/meta", self.base_url);
        let response = self.bearer_response(reqwest::Method::GET, &url)?;
        self.json_response(response, &url)
    }

    fn json_response(&self, response: Response, url: &str) -> Result<serde_json::Value> {
        let status = response.status();
        let body = response
            .text()
            .with_context(|| format!("reading Internxt response: {url}"))?;
        if !status.is_success() {
            return Err(anyhow!("Internxt endpoint returned {status}: {body}"));
        }
        serde_json::from_str(&body).with_context(|| format!("parsing Internxt response: {body}"))
    }

    fn download_links(
        &self,
        session: &InternxtSession,
        bucket_id: &str,
        network_file_id: &str,
    ) -> Result<(String, String)> {
        let url = format!(
            "{}/buckets/{bucket_id}/files/{network_file_id}/info",
            session.network_url.trim_end_matches('/')
        );
        let value = self.json_response(
            self.bridge_response(reqwest::Method::GET, &url, session, None)?,
            &url,
        )?;
        let download_url = value
            .get("shards")
            .and_then(|v| v.as_array())
            .and_then(|v| v.first())
            .and_then(|v| v.get("url"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Internxt response has no download shard URL"))?;
        let index = value
            .get("index")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Internxt response has no file index"))?;
        Ok((download_url.to_owned(), index.to_owned()))
    }

    pub fn download_file(&self, session: &InternxtSession, file_uuid: &str) -> Result<Vec<u8>> {
        let metadata = self.file_metadata(file_uuid)?;
        let bucket = metadata
            .get("bucket")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no bucket"))?;
        let network_id = metadata
            .get("fileId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no network file id"))?;
        let (url, index_hex) = self.download_links(session, bucket, network_id)?;
        let response = self
            .http
            .get(url)
            .send()
            .context("downloading encrypted Internxt file")?;
        let status = response.status();
        let encrypted = response
            .bytes()
            .context("reading encrypted Internxt file")?;
        if !status.is_success() {
            return Err(anyhow!("Internxt shard download returned {status}"));
        }
        let index = hex::decode(index_hex).context("decoding Internxt file index")?;
        let index: [u8; 32] = index
            .try_into()
            .map_err(|_| anyhow!("Internxt file index must contain 32 bytes"))?;
        let bucket = hex::decode(bucket).context("decoding Internxt bucket")?;
        let bucket: [u8; 12] = bucket
            .try_into()
            .map_err(|_| anyhow!("Internxt bucket must contain 12 bytes"))?;
        let mut plain = encrypted.to_vec();
        crypt(&mut plain, &session.mnemonic, &bucket, &index);
        let expected = metadata
            .get("size")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .ok_or_else(|| anyhow!("Internxt file metadata has no size"))?
            as usize;
        if plain.len() < expected {
            return Err(anyhow!("Internxt download is shorter than its metadata"));
        }
        plain.truncate(expected);
        Ok(plain)
    }

    pub fn upload_file(
        &self,
        session: &InternxtSession,
        parent_folder_uuid: &str,
        plain_name: &str,
        file_type: &str,
        data: &[u8],
    ) -> Result<()> {
        const SINGLE_UPLOAD_LIMIT: usize = 100 * 1024 * 1024;
        if data.len() >= SINGLE_UPLOAD_LIMIT {
            return Err(anyhow!(
                "native Internxt upload currently supports files below 100 MiB"
            ));
        }
        let bucket = session.bucket_bytes()?;
        let (index, encrypted) = encrypt(data, &session.mnemonic, &bucket);
        let index_hex = hex::encode(index);
        let start_url = format!(
            "{}/v2/buckets/{}/files/start?multiparts=1",
            session.network_url.trim_end_matches('/'),
            session.bucket_id
        );
        let start_body = serde_json::to_vec(&serde_json::json!({
            "uploads": [{"index": 0, "size": encrypted.len()}]
        }))?;
        let started = self.json_response(
            self.bridge_response(reqwest::Method::POST, &start_url, session, Some(start_body))?,
            &start_url,
        )?;
        let upload = started
            .get("uploads")
            .and_then(|v| v.as_array())
            .and_then(|v| v.first())
            .ok_or_else(|| anyhow!("Internxt upload start returned no upload"))?;
        let upload_url = upload
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Internxt upload start returned no upload URL"))?;
        let shard_uuid = upload
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Internxt upload start returned no shard UUID"))?;
        let response = self
            .http
            .put(upload_url)
            .header("content-type", "application/octet-stream")
            .body(encrypted.clone())
            .send()
            .context("uploading encrypted Internxt shard")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Internxt shard upload returned {}",
                response.status()
            ));
        }
        let hash = hex::encode(sha2::Sha256::digest(&encrypted));
        let finish_url = format!(
            "{}/v2/buckets/{}/files/finish",
            session.network_url.trim_end_matches('/'),
            session.bucket_id
        );
        let finish_body = serde_json::to_vec(&serde_json::json!({
            "index": index_hex,
            "shards": [{"hash": hash, "uuid": shard_uuid}]
        }))?;
        let finished = self.json_response(
            self.bridge_response(
                reqwest::Method::POST,
                &finish_url,
                session,
                Some(finish_body),
            )?,
            &finish_url,
        )?;
        let network_file_id = finished
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Internxt upload finish returned no file id"))?;
        let create_url = format!("{}/files", self.base_url);
        let create_body = serde_json::to_vec(&serde_json::json!({
            "folderUuid": parent_folder_uuid,
            "plainName": plain_name,
            "type": file_type,
            "size": data.len(),
            "bucket": session.bucket_id,
            "fileId": network_file_id,
            "encryptVersion": "Aes03",
            "name": ""
        }))?;
        self.json_response(
            self.bearer_request(reqwest::Method::POST, &create_url, create_body)?,
            &create_url,
        )?;
        Ok(())
    }

    fn bearer_request(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Vec<u8>,
    ) -> Result<Response> {
        self.http
            .request(method, url)
            .bearer_auth(&self.bearer_token)
            .header("internxt-client", "cli")
            .header("content-type", "application/json")
            .body(body)
            .send()
            .with_context(|| format!("requesting Internxt drive endpoint: {url}"))
    }

    /// List all files and folders directly below [folder_uuid].
    pub fn list_folder(&self, folder_uuid: &str) -> Result<Vec<NativeItem>> {
        let mut entries = Vec::new();
        for kind in ["folders", "files"] {
            let mut offset = 0;
            loop {
                let page = self.list_page(folder_uuid, kind, offset)?;
                let count = page.len();
                entries.extend(page);
                if count < 50 {
                    break;
                }
                offset += count;
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    pub fn resolve_path(
        &self,
        session: &InternxtSession,
        path: &std::path::Path,
    ) -> Result<NativeItem> {
        let mut current = NativeItem {
            name: "Root".to_owned(),
            uuid: session.root_folder_id.clone(),
            is_dir: true,
            size: 0,
        };
        for component in path.components() {
            let component = component.as_os_str().to_string_lossy();
            if component.is_empty() || component == "." || component == "/" {
                continue;
            }
            if !current.is_dir {
                return Err(anyhow!("Internxt path traverses through a file"));
            }
            current = self
                .list_folder(&current.uuid)?
                .into_iter()
                .find(|item| item.name == component)
                .ok_or_else(|| anyhow!("Internxt path component not found: {component}"))?;
        }
        Ok(current)
    }

    pub fn create_folder(&self, parent_uuid: &str, name: &str) -> Result<String> {
        let url = format!("{}/folders", self.base_url);
        let body = serde_json::to_vec(&serde_json::json!({
            "plainName": name,
            "parentFolderUuid": parent_uuid
        }))?;
        let value = self.json_response(
            self.bearer_request(reqwest::Method::POST, &url, body)?,
            &url,
        )?;
        value
            .get("uuid")
            .or_else(|| value.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Internxt folder creation returned no UUID"))
    }

    pub fn trash(&self, uuid: &str, kind: &str) -> Result<()> {
        let url = format!("{}/storage/trash/add", self.base_url);
        let body = serde_json::to_vec(&serde_json::json!({
            "items": [{"uuid": uuid, "type": kind}]
        }))?;
        let response = self.bearer_request(reqwest::Method::POST, &url, body)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("Internxt trash endpoint returned {status}: {body}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    const BUCKET: [u8; 12] = [0; 12];
    const INDEX: [u8; 32] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
        0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
        0xee, 0xff,
    ];

    #[test]
    fn password_hash_matches_reference_vector() {
        assert_eq!(
            password_hash("password123", "00112233445566778899aabbccddeeff").unwrap(),
            "c1248c09f33f02499054008e59e28207367eae453a09b4c49a1df4c2d1b516c8"
        );
    }

    #[test]
    fn openssl_envelope_round_trips_and_has_magic_header() {
        let encrypted = encrypt_text("unicode ✓".as_bytes(), "6KYQBP847D4ATSFA").unwrap();
        assert!(encrypted.starts_with("53616c7465645f5f"));
        assert_eq!(
            decrypt_text(&encrypted, "6KYQBP847D4ATSFA").unwrap(),
            "unicode ✓".as_bytes()
        );
    }

    #[test]
    fn login_password_payload_decrypts_to_derived_hash() {
        let secret = "6KYQBP847D4ATSFA";
        let encrypted_salt = encrypt_text(b"00112233445566778899aabbccddeeff", secret).unwrap();
        let payload = login_password_payload("password123", &encrypted_salt, secret).unwrap();
        let hash = decrypt_text(&payload, secret).unwrap();
        assert_eq!(
            hash,
            b"c1248c09f33f02499054008e59e28207367eae453a09b4c49a1df4c2d1b516c8"
        );
    }

    #[test]
    fn file_key_matches_reference_vector() {
        assert_eq!(
            hex::encode(file_key(MNEMONIC, &BUCKET, &INDEX)),
            "89c56e8b825396d9e2d5b047843b42fe3269bacaf6e6fddb4f6c9a0bf3f9cfc1"
        );
    }

    #[test]
    fn aes_ctr_matches_reference_vector_and_round_trips() {
        let mut data = b"hello internxt".to_vec();
        crypt(&mut data, MNEMONIC, &BUCKET, &INDEX);
        assert_eq!(hex::encode(&data), "4a68f2da3e622b5fe6acc7758724");
        crypt(&mut data, MNEMONIC, &BUCKET, &INDEX);
        assert_eq!(data, b"hello internxt");
    }

    #[test]
    fn empty_payload_preserves_length() {
        let (index, encrypted) = encrypt(&[], MNEMONIC, &BUCKET);
        assert_eq!(index.len(), 32);
        assert!(encrypted.is_empty());
    }

    #[test]
    fn content_page_accepts_result_and_legacy_keys() {
        let page: ContentPage =
            serde_json::from_str(r#"{"result":[{"plainName":"a.txt","uuid":"f1","size":"12"}]}"#)
                .unwrap();
        assert_eq!(page.result.len(), 1);
        let legacy: ContentPage =
            serde_json::from_str(r#"{"folders":[{"name":"Docs","id":"d1"}],"files":[]}"#).unwrap();
        assert_eq!(legacy.folders.len(), 1);
    }

    #[test]
    fn session_serialization_round_trips_all_auth_state() {
        let session = InternxtSession {
            drive_api_url: "https://drive.example".into(),
            network_url: "https://network.example".into(),
            email: "user@example.com".into(),
            token: "token".into(),
            new_token: "new-token".into(),
            mnemonic: "test mnemonic".into(),
            user_id: "user-id".into(),
            root_folder_id: "root-id".into(),
            bridge_user: "bridge-user".into(),
            bucket_id: "00112233445566778899aabb".into(),
        };
        assert_eq!(
            InternxtSession::decode(&session.encode().unwrap()).unwrap(),
            session
        );
    }

    #[test]
    fn session_derives_bridge_password_and_bucket_bytes() {
        let session = InternxtSession {
            drive_api_url: String::new(),
            network_url: String::new(),
            email: String::new(),
            token: String::new(),
            new_token: String::new(),
            mnemonic: String::new(),
            user_id: "user-id".into(),
            root_folder_id: String::new(),
            bridge_user: String::new(),
            bucket_id: "00112233445566778899aabb".into(),
        };
        assert_eq!(
            session.bridge_pass(),
            "a7571ddec1df43045ac667d7c976bd1149fe9a2dbb3fb55357beed582e11538d"
        );
        assert_eq!(
            session.bucket_bytes().unwrap(),
            [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb]
        );
    }

    #[test]
    fn mnemonic_seed_normalizes_unicode() {
        assert_eq!(
            mnemonic_seed("cafe\u{301}", "pass"),
            mnemonic_seed("caf\u{e9}", "pass")
        );
    }

    #[test]
    fn crypto_matches_the_vendored_internxt_core_engine() {
        let bucket_hex = hex::encode(BUCKET);
        assert_eq!(
            file_key(MNEMONIC, &BUCKET, &INDEX),
            internxt_core::crypto::generate_file_key(MNEMONIC, &bucket_hex, &INDEX).unwrap()
        );
        assert_eq!(
            InternxtSession {
                drive_api_url: String::new(),
                network_url: String::new(),
                email: String::new(),
                token: String::new(),
                new_token: String::new(),
                mnemonic: String::new(),
                user_id: "user-id".into(),
                root_folder_id: String::new(),
                bridge_user: String::new(),
                bucket_id: bucket_hex,
            }
            .bridge_pass(),
            internxt_core::crypto::network_password("user-id")
        );
        assert_eq!(
            password_hash("password123", "00112233445566778899aabbccddeeff").unwrap(),
            internxt_core::crypto::pass_to_hash(
                "password123",
                Some("00112233445566778899aabbccddeeff")
            )
            .unwrap()
            .1
        );
    }
}
