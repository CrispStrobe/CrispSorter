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
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_GATEWAY_URL: &str = "https://gateway.filen.io";
pub const DEFAULT_INGEST_URL: &str = "https://ingest.filen.io";
pub const DEFAULT_EGEST_URL: &str = "https://egest.filen.io";
pub const CHUNK_SIZE: usize = 1024 * 1024;
/// Serial is the gateway-safe default; callers may opt into concurrency via
/// [`FilenNativeClient::set_transfer_config`].
pub const TRANSFER_CONCURRENCY: usize = 1;
pub const LISTING_CACHE_TTL: Duration = Duration::from_secs(600);

fn local_timestamps(metadata: &std::fs::Metadata) -> (i64, i64) {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let created = metadata
        .created()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(modified);
    (created, modified)
}

fn set_local_modified_best_effort(path: &Path, modified: i64) {
    if modified <= 0 {
        return;
    }
    let value = std::time::UNIX_EPOCH.checked_add(Duration::from_millis(modified as u64));
    if let Some(value) = value {
        let _ = std::fs::File::open(path).and_then(|file| file.set_modified(value));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferConfig {
    pub chunk_size: usize,
    pub workers: usize,
    pub file_workers: usize,
    pub retries: usize,
    pub retry_backoff_ms: u64,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            chunk_size: CHUNK_SIZE,
            workers: TRANSFER_CONCURRENCY,
            file_workers: 1,
            retries: 3,
            retry_backoff_ms: 250,
        }
    }
}

impl TransferConfig {
    pub fn validate(self) -> Result<Self> {
        anyhow::ensure!(self.chunk_size > 0, "Filen chunk size must be positive");
        anyhow::ensure!(self.workers > 0, "Filen transfer workers must be positive");
        anyhow::ensure!(self.file_workers > 0, "Filen file workers must be positive");
        Ok(self)
    }
}

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
    h.update(&derived.as_bytes()[64..]);
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

fn post_json_retry<T: serde::de::DeserializeOwned>(
    http: &reqwest::blocking::Client,
    url: String,
    body: serde_json::Value,
    config: TransferConfig,
) -> Result<ApiEnvelope<T>> {
    let mut last_error = None;
    for attempt in 0..config.retries.max(1) {
        match http.post(&url).json(&body).send() {
            Ok(response) => {
                let status = response.status();
                let text = response.text()?;
                if status.is_success() {
                    return Ok(serde_json::from_str(&text)?);
                }
                last_error = Some(anyhow!("Filen HTTP {status}: {text}"));
                if !status.is_server_error() {
                    break;
                }
            }
            Err(error) => last_error = Some(error.into()),
        }
        if attempt + 1 < config.retries.max(1) {
            std::thread::sleep(Duration::from_millis(
                config.retry_backoff_ms.saturating_mul(1 << attempt),
            ));
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("Filen request failed")))
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
    pub mime: String,
    pub created: i64,
    pub modified: i64,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePathListing {
    pub path: PathBuf,
    pub item: NativeItem,
}

/// Compatibility name shared with the native Internxt client.
pub type SearchResult = NativePathListing;

#[derive(Debug, Clone)]
pub struct UploadJob {
    pub parent: String,
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct DownloadPathJob {
    pub item: NativeItem,
    pub local_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchConflictPolicy {
    Fail,
    Skip,
    Replace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchTransferState {
    pub version: u8,
    pub completed: Vec<String>,
}

enum ResumableUploadEvent {
    Progress { index: usize, bytes: u64 },
    Done { index: usize, result: Result<()> },
}

enum ResumableDownloadEvent {
    Progress { index: usize, bytes: u64 },
    Done { index: usize, result: Result<()> },
}

impl Default for BatchTransferState {
    fn default() -> Self {
        Self {
            version: 1,
            completed: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadResumeState {
    pub uuid: String,
    pub upload_key: String,
    pub file_key: [u8; 32],
    pub parent: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub chunk_size: usize,
    pub completed_chunks: Vec<usize>,
    pub bucket: String,
    pub region: String,
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
    listing_cache: Mutex<HashMap<String, CachedListing>>,
    listing_cache_ttl: Duration,
    transfer_config: TransferConfig,
}

#[derive(Debug, Clone)]
struct CachedListing {
    loaded_at: Instant,
    items: Vec<NativeItem>,
}

#[derive(Debug, Deserialize)]
struct ApiEnvelope<T> {
    status: bool,
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: String,
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
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
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
            listing_cache: Mutex::new(HashMap::new()),
            listing_cache_ttl: LISTING_CACHE_TTL,
            transfer_config: TransferConfig::default(),
        })
    }

    pub fn from_session(session: &FilenSession) -> Result<Self> {
        Self::new_inner(session)
    }

    pub fn from_session_with_config(
        session: &FilenSession,
        transfer_config: TransferConfig,
    ) -> Result<Self> {
        let mut client = Self::new_inner(session)?;
        client.transfer_config = transfer_config.validate()?;
        Ok(client)
    }

    pub fn set_transfer_config(&mut self, transfer_config: TransferConfig) -> Result<()> {
        self.transfer_config = transfer_config.validate()?;
        Ok(())
    }

    /// Configure listing freshness independently from transfer tuning.
    /// `Duration::ZERO` disables cache reuse while retaining invalidation
    /// behavior for callers that need a fresh read on every request.
    pub fn set_listing_cache_ttl(&mut self, ttl: Duration) {
        self.listing_cache_ttl = ttl;
        self.invalidate_listings();
    }

    pub fn upload_files(&self, jobs: Vec<UploadJob>) -> Result<()> {
        self.upload_files_with_progress(jobs, |_, _| {})
    }

    /// Batch upload with `(completed_files, total_files)` progress. Files
    /// still complete out of order, while callbacks are serialized by the
    /// receiver and therefore safe for ordinary UI state.
    pub fn upload_files_with_progress<F: FnMut(usize, usize)>(
        &self,
        jobs: Vec<UploadJob>,
        mut progress: F,
    ) -> Result<()> {
        let mut last_completed = 0;
        self.upload_files_with_byte_progress(jobs, |completed, total, _, _| {
            if completed > last_completed {
                last_completed = completed;
                progress(completed, total);
            }
        })
    }

    /// Batch upload progress with both completed-file and completed-byte
    /// totals. Callbacks are serialized and emitted when each file finishes.
    pub fn upload_files_with_byte_progress<F: FnMut(usize, usize, u64, u64)>(
        &self,
        jobs: Vec<UploadJob>,
        mut progress: F,
    ) -> Result<()> {
        let workers = self.transfer_config.file_workers.min(jobs.len()).max(1);
        let total = jobs.len();
        let total_bytes = jobs.iter().map(|job| job.data.len() as u64).sum();
        let jobs = jobs.as_slice();
        let next = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel();
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let next = Arc::clone(&next);
                let sender = sender.clone();
                scope.spawn(move || loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= jobs.len() {
                        break;
                    }
                    let result = self.upload_file(
                        &jobs[index].parent,
                        &jobs[index].name,
                        &jobs[index].mime,
                        &jobs[index].data,
                    );
                    if sender.send((index, result)).is_err() {
                        break;
                    }
                });
            }
            drop(sender);
            let mut completed = 0;
            let mut completed_bytes = 0;
            for (index, result) in receiver {
                result?;
                completed += 1;
                completed_bytes += jobs[index].data.len() as u64;
                progress(completed, total, completed_bytes, total_bytes);
            }
            Ok(())
        })
    }

    /// Batch upload with durable per-file completion state. A failed batch
    /// leaves completed jobs checkpointed so the same input can be retried.
    /// Conflict handling is explicit and compatible with the Python/Dart
    /// clients: `Skip`, `Replace`, or `Fail`.
    pub fn upload_files_resumable<F: FnMut(usize, usize)>(
        &self,
        jobs: Vec<UploadJob>,
        state_path: &Path,
        conflict: BatchConflictPolicy,
        mut progress: F,
    ) -> Result<()> {
        let mut last_completed = 0;
        self.upload_files_resumable_with_byte_progress(
            jobs,
            state_path,
            conflict,
            |completed, total, _, _| {
                if completed > last_completed {
                    last_completed = completed;
                    progress(completed, total);
                }
            },
        )
    }

    /// Resumable batch upload with serialized file and aggregate byte
    /// progress. Byte callbacks are emitted as encrypted chunks complete,
    /// even when multiple files are uploading concurrently.
    pub fn upload_files_resumable_with_byte_progress<F: FnMut(usize, usize, u64, u64)>(
        &self,
        jobs: Vec<UploadJob>,
        state_path: &Path,
        conflict: BatchConflictPolicy,
        mut progress: F,
    ) -> Result<()> {
        let mut state = Self::load_batch_transfer_state(state_path)?.unwrap_or_default();
        anyhow::ensure!(state.version == 1, "unsupported Filen batch state version");
        state.completed.sort_unstable();
        Self::save_batch_transfer_state(state_path, &state)?;
        let shared_state = Arc::new(Mutex::new(state));
        let workers = self.transfer_config.file_workers.min(jobs.len()).max(1);
        let total = jobs.len();
        let jobs = jobs.as_slice();
        let next = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel::<ResumableUploadEvent>();
        let state_path = state_path.to_owned();
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let next = Arc::clone(&next);
                let sender = sender.clone();
                let progress_sender = sender.clone();
                let shared_state = Arc::clone(&shared_state);
                let state_path = state_path.clone();
                scope.spawn(move || loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= jobs.len() {
                        break;
                    }
                    let job = &jobs[index];
                    let key = format!("{}/{}", job.parent, job.name);
                    let result = (|| {
                        {
                            let state = shared_state
                                .lock()
                                .map_err(|_| anyhow!("batch state poisoned"))?;
                            if state.completed.binary_search(&key).is_ok() {
                                return Ok(());
                            }
                        }
                        let existing = self
                            .list_folder_fresh(&job.parent)?
                            .into_iter()
                            .find(|item| item.name == job.name && !item.is_dir);
                        if let Some(existing) = existing {
                            match conflict {
                                BatchConflictPolicy::Fail => {
                                    return Err(anyhow!("Filen batch conflict at {key}"));
                                }
                                BatchConflictPolicy::Skip => {}
                                BatchConflictPolicy::Replace => {
                                    self.trash(&existing.uuid, "file")?;
                                }
                            }
                            if conflict == BatchConflictPolicy::Skip {
                                let mut state = shared_state
                                    .lock()
                                    .map_err(|_| anyhow!("batch state poisoned"))?;
                                if state.completed.binary_search(&key).is_err() {
                                    state.completed.push(key.clone());
                                    state.completed.sort_unstable();
                                    Self::save_batch_transfer_state(&state_path, &state)?;
                                }
                                return Ok(());
                            }
                        }
                        let mut upload_progress = |bytes, _total| {
                            let _ = progress_sender
                                .send(ResumableUploadEvent::Progress { index, bytes });
                        };
                        self.upload_file_from_reader_with_progress(
                            &job.parent,
                            &job.name,
                            &job.mime,
                            job.data.len() as u64,
                            std::io::Cursor::new(job.data.as_slice()),
                            &mut upload_progress,
                        )?;
                        let mut state = shared_state
                            .lock()
                            .map_err(|_| anyhow!("batch state poisoned"))?;
                        if state.completed.binary_search(&key).is_err() {
                            state.completed.push(key);
                            state.completed.sort_unstable();
                            Self::save_batch_transfer_state(&state_path, &state)?;
                        }
                        Ok(())
                    })();
                    if sender
                        .send(ResumableUploadEvent::Done { index, result })
                        .is_err()
                    {
                        break;
                    }
                });
            }
            drop(sender);
            let mut completed = 0;
            let total_bytes = jobs.iter().map(|job| job.data.len() as u64).sum();
            let mut completed_bytes = 0;
            let mut per_file_bytes = vec![0u64; total];
            for event in receiver {
                match event {
                    ResumableUploadEvent::Progress { index, bytes } => {
                        let previous = per_file_bytes[index];
                        let current = bytes.max(previous);
                        per_file_bytes[index] = current;
                        completed_bytes += current - previous;
                        progress(completed, total, completed_bytes, total_bytes);
                    }
                    ResumableUploadEvent::Done { index, result } => {
                        result?;
                        if per_file_bytes[index] < jobs[index].data.len() as u64 {
                            completed_bytes +=
                                jobs[index].data.len() as u64 - per_file_bytes[index];
                            per_file_bytes[index] = jobs[index].data.len() as u64;
                        }
                        completed += 1;
                        progress(completed, total, completed_bytes, total_bytes);
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        })?;
        Self::clear_batch_transfer_state(&state_path)
    }

    /// Drop all cached directory listings. Every tree mutation calls this so
    /// path resolution cannot observe stale entries after a write.
    pub fn invalidate_listings(&self) {
        if let Ok(mut cache) = self.listing_cache.lock() {
            cache.clear();
        }
    }

    fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        url: String,
        body: Option<serde_json::Value>,
    ) -> Result<T> {
        let mut last_error = None;
        for attempt in 0..self.transfer_config.retries.max(1) {
            let mut request = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&self.api_key);
            if let Some(value) = &body {
                request = request.json(value);
            }
            match request.send() {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text()?;
                    if status.is_success() {
                        let envelope: ApiEnvelope<T> = serde_json::from_str(&text)
                            .with_context(|| format!("decoding Filen response: {text}"))?;
                        anyhow::ensure!(envelope.status, "Filen API error: {}", envelope.message);
                        return envelope
                            .data
                            .ok_or_else(|| anyhow!("Filen response has no data"));
                    }
                    last_error = Some(anyhow!("Filen HTTP {status}: {text}"));
                    if !status.is_server_error() {
                        break;
                    }
                }
                Err(error) => {
                    last_error = Some(error.into());
                }
            }
            if attempt + 1 < self.transfer_config.retries.max(1) {
                std::thread::sleep(Duration::from_millis(
                    self.transfer_config
                        .retry_backoff_ms
                        .saturating_mul(1 << attempt),
                ));
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("Filen request failed")))
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
        if let Some(items) = self
            .listing_cache
            .lock()
            .map_err(|_| anyhow!("listing cache poisoned"))?
            .get(uuid)
            .filter(|entry| entry.loaded_at.elapsed() < self.listing_cache_ttl)
            .map(|entry| entry.items.clone())
        {
            return Ok(items);
        }
        self.list_folder_fresh(uuid)
    }

    /// Bypass the listing cache and refresh the folder from the gateway.
    pub fn list_folder_fresh(&self, uuid: &str) -> Result<Vec<NativeItem>> {
        let items = self.list_folder_uncached(uuid)?;
        self.listing_cache
            .lock()
            .map_err(|_| anyhow!("listing cache poisoned"))?
            .insert(
                uuid.to_owned(),
                CachedListing {
                    loaded_at: Instant::now(),
                    items: items.clone(),
                },
            );
        Ok(items)
    }

    /// Compatibility alias shared with the native Internxt client.
    pub fn list_folder_cached(&self, uuid: &str) -> Result<Vec<NativeItem>> {
        self.list_folder(uuid)
    }

    pub fn file_exists(&self, parent: &str, name: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct Exists {
            #[serde(default)]
            exists: bool,
        }
        let value: Exists = self.request(
            reqwest::Method::POST,
            format!("{}/v3/file/exists", self.gateway_url),
            Some(serde_json::json!({
                "parent": parent,
                "nameHashed": self.hash_name(name)?
            })),
        )?;
        Ok(value.exists)
    }

    pub fn get_flat_folder_tree(&self, folder_uuid: &str) -> Result<serde_json::Value> {
        self.request(
            reqwest::Method::POST,
            format!("{}/v3/dir/tree", self.gateway_url),
            Some(serde_json::json!({
                "uuid": folder_uuid,
                "deviceId": random_uuid(),
                "skipCache": 0
            })),
        )
    }

    /// Fetch and decrypt one file's metadata without listing its parent.
    pub fn get_file(&self, uuid: &str) -> Result<NativeItem> {
        let upload: RemoteUpload = self.request(
            reqwest::Method::POST,
            format!("{}/v3/file", self.gateway_url),
            Some(serde_json::json!({"uuid": uuid})),
        )?;
        self.remote_upload_to_item(upload)
    }

    fn remote_upload_to_item(&self, upload: RemoteUpload) -> Result<NativeItem> {
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
        Ok(NativeItem {
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
            mime: value
                .get("mime")
                .and_then(|v| v.as_str())
                .unwrap_or("application/octet-stream")
                .to_owned(),
            created: value.get("creation").and_then(|v| v.as_i64()).unwrap_or(0),
            modified: value
                .get("lastModified")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            hash: value
                .get("blake3")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
        })
    }

    fn list_folder_uncached(&self, uuid: &str) -> Result<Vec<NativeItem>> {
        let content: DirContent = self.request(
            reqwest::Method::POST,
            format!("{}/v3/dir/content", self.gateway_url),
            Some(serde_json::json!({"uuid": uuid})),
        )?;
        let mut items = Vec::with_capacity(content.uploads.len() + content.folders.len());
        for folder in content.folders {
            let name = self.crypto_metadata(&folder.metadata)?;
            let folder_meta = serde_json::from_str::<serde_json::Value>(&name).ok();
            let created = folder_meta
                .as_ref()
                .and_then(|v| v.get("creation"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let name = folder_meta
                .as_ref()
                .and_then(|v| v.get("name").and_then(|n| n.as_str()))
                .unwrap_or(&name)
                .to_owned();
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
                mime: String::new(),
                created,
                modified: 0,
                hash: String::new(),
            });
        }
        for upload in content.uploads {
            items.push(self.remote_upload_to_item(upload)?);
        }
        Ok(items)
    }

    /// Find files and folders using slash-separated glob paths. * and ?
    /// never cross a directory boundary; ** is recursive.
    pub fn search(&self, session: &FilenSession, pattern: &str) -> Result<Vec<NativeItem>> {
        self.search_with_max_depth(session, pattern, None)
    }

    pub fn search_with_max_depth(
        &self,
        session: &FilenSession,
        pattern: &str,
        max_depth: Option<usize>,
    ) -> Result<Vec<NativeItem>> {
        let pattern = pattern.trim_matches('/');
        let parts: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
        let mut current_uuid = session.root_folder_uuid.clone();
        let mut prefix = String::new();
        let mut offset = 0;
        while offset < parts.len() && is_literal_glob_component(parts[offset]) {
            let name = parts[offset];
            let item = self
                .list_folder(&current_uuid)?
                .into_iter()
                .find(|item| item.name == name);
            let Some(item) = item else {
                return Ok(Vec::new());
            };
            prefix = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            current_uuid = item.uuid.clone();
            offset += 1;
            if offset == parts.len() {
                return Ok(vec![item]);
            }
        }
        let remaining = parts[offset..].join("/");
        let mut results = Vec::new();
        self.search_folder_depth(
            &current_uuid,
            &prefix,
            &remaining,
            0,
            max_depth,
            &mut results,
        )?;
        Ok(results)
    }

    /// Search recursively and return path-qualified results, matching the
    /// native Internxt client's `search_files` result shape. The original
    /// `search` API remains available for callers that only need items.
    pub fn search_files(
        &self,
        session: &FilenSession,
        pattern: &str,
        max_depth: Option<usize>,
    ) -> Result<Vec<SearchResult>> {
        let pattern = pattern.trim_matches('/');
        let depth = max_depth.map(|value| value as isize).unwrap_or(-1);
        Ok(self
            .list_folder_with_paths(session, Path::new("."), depth)?
            .into_iter()
            .filter(|entry| glob_match(pattern, &entry.path.to_string_lossy()))
            .collect())
    }

    /// List every item below a folder together with its path relative to the
    /// requested folder. `max_depth` is relative to that folder; negative
    /// values mean unlimited recursion.
    pub fn list_folder_with_paths(
        &self,
        session: &FilenSession,
        folder_path: &Path,
        max_depth: isize,
    ) -> Result<Vec<NativePathListing>> {
        let folder = self.resolve_path(session, folder_path)?;
        anyhow::ensure!(folder.is_dir, "listing starting path is not a folder");
        let base = if folder_path == Path::new(".") {
            Path::new("")
        } else {
            folder_path
        };
        let mut output = Vec::new();
        self.list_paths_recursive(&folder.uuid, base, max_depth, 0, &mut output)?;
        output.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(output)
    }

    fn list_paths_recursive(
        &self,
        folder_uuid: &str,
        parent_path: &Path,
        max_depth: isize,
        depth: isize,
        output: &mut Vec<NativePathListing>,
    ) -> Result<()> {
        for item in self.list_folder_cached(folder_uuid)? {
            let path = parent_path.join(&item.name);
            output.push(NativePathListing {
                path: path.clone(),
                item: item.clone(),
            });
            if item.is_dir && (max_depth < 0 || depth < max_depth) {
                self.list_paths_recursive(&item.uuid, &path, max_depth, depth + 1, output)?;
            }
        }
        Ok(())
    }

    fn search_folder_depth(
        &self,
        uuid: &str,
        prefix: &str,
        pattern: &str,
        depth: usize,
        max_depth: Option<usize>,
        results: &mut Vec<NativeItem>,
    ) -> Result<()> {
        for item in self.list_folder(uuid)? {
            let path = if prefix.is_empty() {
                item.name.clone()
            } else {
                format!("{prefix}/{}", item.name)
            };
            if glob_match(pattern, &path) {
                results.push(item.clone());
            }
            if item.is_dir && max_depth.is_none_or(|limit| depth < limit) {
                self.search_folder_depth(
                    &item.uuid,
                    &path,
                    pattern,
                    depth + 1,
                    max_depth,
                    results,
                )?;
            }
        }
        Ok(())
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
            mime: String::new(),
            created: 0,
            modified: 0,
            hash: String::new(),
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
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        let base = gateway_url.trim_end_matches('/');
        let auth: ApiEnvelope<AuthInfo> = post_json_retry(
            &http,
            format!("{base}/v3/auth/info"),
            serde_json::json!({"email": email}),
            TransferConfig::default(),
        )?;
        anyhow::ensure!(
            auth.status,
            "Filen auth info failed{}: {}",
            if auth.code.is_empty() {
                String::new()
            } else {
                format!(" ({})", auth.code)
            },
            auth.message
        );
        let auth = auth
            .data
            .ok_or_else(|| anyhow!("Filen auth info missing"))?;
        let (auth_password, master, dek, hmac) = if auth.auth_version >= 3 {
            let (k, p) = argon2id_login(password, &auth.salt)?;
            (p, None, Some(k), None)
        } else {
            let (raw, p) = pbkdf2_login(password, &auth.salt);
            let derived = hex::encode(raw);
            (p, Some(derived.as_bytes()[..64].to_vec()), None, None)
        };
        let mut login_body = serde_json::json!({
            "email": email,
            "password": auth_password,
            "authVersion": auth.auth_version,
        });
        // The API requires a syntactically valid 2FA value even when 2FA is
        // disabled. This is the same sentinel used by the reference clients.
        login_body["twoFactorCode"] = serde_json::Value::String(
            tfa.filter(|value| !value.is_empty())
                .unwrap_or("XXXXXX")
                .to_owned(),
        );
        let login: ApiEnvelope<LoginResponse> = post_json_retry(
            &http,
            format!("{base}/v3/login"),
            login_body,
            TransferConfig::default(),
        )?;
        anyhow::ensure!(
            login.status,
            "Filen login failed{}: {}",
            if login.code.is_empty() {
                String::new()
            } else {
                format!(" ({})", login.code)
            },
            login.message
        );
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

    pub fn begin_upload(
        &self,
        parent: &str,
        name: &str,
        mime: &str,
        size: u64,
    ) -> Result<UploadResumeState> {
        let (file_key, _) = self.new_file_key()?;
        Ok(UploadResumeState {
            uuid: random_uuid(),
            upload_key: random_uuid().replace('-', ""),
            file_key,
            parent: parent.to_owned(),
            name: name.to_owned(),
            mime: mime.to_owned(),
            size,
            chunk_size: self.transfer_config.chunk_size,
            completed_chunks: Vec::new(),
            bucket: String::new(),
            region: String::new(),
        })
    }

    /// Persist a resumable upload checkpoint atomically enough for callers to
    /// recover after a process interruption.
    pub fn save_upload_resume_state(&self, path: &Path, state: &UploadResumeState) -> Result<()> {
        let encoded = serde_json::to_vec_pretty(state)?;
        let temporary = path.with_extension("filen-upload.tmp");
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("writing Filen upload checkpoint {}", path.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("installing Filen upload checkpoint {}", path.display()))?;
        Ok(())
    }

    /// Load a resumable upload checkpoint, returning `None` when absent.
    pub fn load_upload_resume_state(path: &Path) -> Result<Option<UploadResumeState>> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading Filen upload checkpoint {}", path.display()))?;
        Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("parsing Filen upload checkpoint {}", path.display())
        })?))
    }

    /// Remove a completed or abandoned resumable upload checkpoint.
    pub fn clear_upload_resume_state(path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path)
                .with_context(|| format!("removing Filen upload checkpoint {}", path.display()))?;
        }
        Ok(())
    }

    pub fn save_batch_transfer_state(path: &Path, state: &BatchTransferState) -> Result<()> {
        let encoded = serde_json::to_vec_pretty(state)?;
        let temporary = path.with_extension("filen-batch.tmp");
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("writing Filen batch checkpoint {}", path.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("installing Filen batch checkpoint {}", path.display()))?;
        Ok(())
    }

    pub fn load_batch_transfer_state(path: &Path) -> Result<Option<BatchTransferState>> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading Filen batch checkpoint {}", path.display()))?;
        Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("parsing Filen batch checkpoint {}", path.display())
        })?))
    }

    pub fn clear_batch_transfer_state(path: &Path) -> Result<()> {
        if path.exists() {
            std::fs::remove_file(path)
                .with_context(|| format!("removing Filen batch checkpoint {}", path.display()))?;
        }
        Ok(())
    }

    /// Continue an upload from its caller-owned state. On a transient or
    /// permanent failure, completed_chunks remains updated and the same state
    /// can be passed back after the caller reconnects.
    pub fn resume_upload(&self, state: &mut UploadResumeState, data: &[u8]) -> Result<()> {
        anyhow::ensure!(
            data.len() as u64 == state.size,
            "resume data length does not match upload state"
        );
        if data.is_empty() {
            let _: serde_json::Value = self.request(
                reqwest::Method::POST,
                format!("{}/v3/upload/empty", self.gateway_url),
                Some(serde_json::json!({
                    "uuid": state.uuid,
                    "name": self.encrypt_metadata(&state.name)?,
                    "nameHashed": self.hash_name(&state.name)?,
                    "size": self.encrypt_metadata("0")?,
                    "parent": state.parent,
                    "mime": self.encrypt_metadata(&state.mime)?,
                    "metadata": self.encrypt_metadata("{}")?,
                    "version": if self.mode == CryptoMode::V3 { 3 } else { 2 }
                })),
            )?;
            self.invalidate_listings();
            return Ok(());
        }
        let chunk_size = state.chunk_size;
        anyhow::ensure!(chunk_size > 0, "resume state has invalid chunk size");
        let total = data.len().div_ceil(chunk_size);
        let mut completed: std::collections::HashSet<usize> =
            state.completed_chunks.iter().copied().collect();
        for index in 0..total {
            if completed.contains(&index) {
                continue;
            }
            let start = index * chunk_size;
            let chunk = &data[start..data.len().min(start + chunk_size)];
            let mut nonce = [0u8; 12];
            getrandom::getrandom(&mut nonce)?;
            let encrypted = encrypt_file_chunk(chunk, &state.file_key, nonce)?;
            let hash = hex::encode(Sha512::digest(&encrypted));
            let value = upload_chunk_request(
                &self.http,
                &self.api_key,
                format!(
                    "{}/v3/upload?uuid={}&index={index}&parent={}&uploadKey={}&hash={hash}",
                    self.ingest_url, state.uuid, state.parent, state.upload_key
                ),
                &encrypted,
                self.transfer_config.retries,
                self.transfer_config.retry_backoff_ms,
            )?;
            state.bucket = value.bucket;
            state.region = value.region;
            completed.insert(index);
            state.completed_chunks = completed.iter().copied().collect();
            state.completed_chunks.sort_unstable();
        }
        let key_string = if self.mode == CryptoMode::V3 {
            hex::encode(state.file_key)
        } else {
            String::from_utf8_lossy(&state.file_key).into_owned()
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);
        let now = chrono::Utc::now().timestamp_millis();
        let metadata = serde_json::json!({
            "name": state.name,
            "size": data.len(),
            "mime": state.mime,
            "key": key_string,
            "creation": now,
            "lastModified": now,
            "blake3": hasher.finalize().to_hex().to_string()
        })
        .to_string();
        let _: serde_json::Value = self.request(
            reqwest::Method::POST,
            format!("{}/v3/upload/done", self.gateway_url),
            Some(serde_json::json!({
                "uuid": state.uuid,
                "name": self.encrypt_metadata(&state.name)?,
                "nameHashed": self.hash_name(&state.name)?,
                "size": self.encrypt_metadata(&data.len().to_string())?,
                "parent": state.parent,
                "mime": self.encrypt_metadata(&state.mime)?,
                "metadata": self.encrypt_metadata(&metadata)?,
                "version": if self.mode == CryptoMode::V3 { 3 } else { 2 },
                "chunks": total,
                "rm": "0",
                "uploadKey": state.upload_key,
                "bucket": state.bucket,
                "region": state.region
            })),
        )?;
        self.invalidate_listings();
        Ok(())
    }

    /// Continue a resumable upload from an exact-size reader. The reader is
    /// consumed chunk-by-chunk, so large interrupted uploads do not require a
    /// second complete plaintext allocation.
    pub fn resume_upload_from_reader<R: std::io::Read>(
        &self,
        state: &mut UploadResumeState,
        mut reader: R,
    ) -> Result<()> {
        anyhow::ensure!(state.chunk_size > 0, "resume state has invalid chunk size");
        let total = (state.size as usize).div_ceil(state.chunk_size);
        let completed: std::collections::HashSet<usize> =
            state.completed_chunks.iter().copied().collect();
        let mut hasher = blake3::Hasher::new();
        for index in 0..total {
            let remaining = state.size as usize - index * state.chunk_size;
            let length = remaining.min(state.chunk_size);
            let mut chunk = vec![0u8; length];
            reader
                .read_exact(&mut chunk)
                .with_context(|| format!("reading resumable Filen upload chunk {index}"))?;
            hasher.update(&chunk);
            if completed.contains(&index) {
                continue;
            }
            let mut nonce = [0u8; 12];
            getrandom::getrandom(&mut nonce)?;
            let encrypted = encrypt_file_chunk(&chunk, &state.file_key, nonce)?;
            let hash = hex::encode(Sha512::digest(&encrypted));
            let value = upload_chunk_request(
                &self.http,
                &self.api_key,
                format!(
                    "{}/v3/upload?uuid={}&index={index}&parent={}&uploadKey={}&hash={hash}",
                    self.ingest_url, state.uuid, state.parent, state.upload_key
                ),
                &encrypted,
                self.transfer_config.retries,
                self.transfer_config.retry_backoff_ms,
            )?;
            state.bucket = value.bucket;
            state.region = value.region;
            state.completed_chunks.push(index);
            state.completed_chunks.sort_unstable();
            state.completed_chunks.dedup();
        }
        anyhow::ensure!(
            reader.read(&mut [0u8; 1])? == 0,
            "resume reader has extra data"
        );
        if state.size == 0 {
            let _: serde_json::Value = self.request(
                reqwest::Method::POST,
                format!("{}/v3/upload/empty", self.gateway_url),
                Some(serde_json::json!({
                    "uuid": state.uuid,
                    "name": self.encrypt_metadata(&state.name)?,
                    "nameHashed": self.hash_name(&state.name)?,
                    "size": self.encrypt_metadata("0")?,
                    "parent": state.parent,
                    "mime": self.encrypt_metadata(&state.mime)?,
                    "metadata": self.encrypt_metadata("{}")?,
                    "version": if self.mode == CryptoMode::V3 { 3 } else { 2 }
                })),
            )?;
            self.invalidate_listings();
            return Ok(());
        }
        let key_string = if self.mode == CryptoMode::V3 {
            hex::encode(state.file_key)
        } else {
            String::from_utf8_lossy(&state.file_key).into_owned()
        };
        let now = chrono::Utc::now().timestamp_millis();
        let metadata = serde_json::json!({
            "name": state.name,
            "size": state.size,
            "mime": state.mime,
            "key": key_string,
            "creation": now,
            "lastModified": now,
            "blake3": hasher.finalize().to_hex().to_string()
        })
        .to_string();
        let _: serde_json::Value = self.request(
            reqwest::Method::POST,
            format!("{}/v3/upload/done", self.gateway_url),
            Some(serde_json::json!({
                "uuid": state.uuid,
                "name": self.encrypt_metadata(&state.name)?,
                "nameHashed": self.hash_name(&state.name)?,
                "size": self.encrypt_metadata(&state.size.to_string())?,
                "parent": state.parent,
                "mime": self.encrypt_metadata(&state.mime)?,
                "metadata": self.encrypt_metadata(&metadata)?,
                "version": if self.mode == CryptoMode::V3 { 3 } else { 2 },
                "chunks": total,
                "rm": "0",
                "uploadKey": state.upload_key,
                "bucket": state.bucket,
                "region": state.region
            })),
        )?;
        self.invalidate_listings();
        Ok(())
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
        self.invalidate_listings();
        Ok(value.uuid)
    }

    fn encrypt_file_name(&self, name: &str, file_key: &[u8; 32]) -> Result<String> {
        let mut nonce = [0u8; 12];
        getrandom::getrandom(&mut nonce)?;
        for value in &mut nonce {
            *value = b'A' + (*value % 26);
        }
        v2_encrypt_metadata(name, file_key, nonce)
    }

    pub fn move_item(&self, uuid: &str, new_parent: &str, is_dir: bool) -> Result<()> {
        let result = self.request_empty(
            reqwest::Method::POST,
            format!(
                "{}/v3/{}/move",
                self.gateway_url,
                if is_dir { "dir" } else { "file" }
            ),
            Some(serde_json::json!({"uuid": uuid, "to": new_parent})),
        );
        if result.is_ok() {
            self.invalidate_listings();
        }
        result
    }

    pub fn rename_item(&self, item: &NativeItem, new_name: &str) -> Result<()> {
        anyhow::ensure!(
            !new_name.is_empty() && !new_name.contains('/'),
            "invalid Filen name"
        );
        if item.is_dir {
            let metadata =
                serde_json::json!({"name": new_name, "creation": item.created}).to_string();
            let result = self.request_empty(
                reqwest::Method::POST,
                format!("{}/v3/dir/metadata", self.gateway_url),
                Some(serde_json::json!({"uuid": item.uuid, "nameHashed": self.hash_name(new_name)?, "name": self.encrypt_metadata(&metadata)?})),
            );
            if result.is_ok() {
                self.invalidate_listings();
            }
            result
        } else {
            let key = item
                .file_key
                .ok_or_else(|| anyhow!("Filen file has no encryption key"))?;
            let metadata = serde_json::json!({"name": new_name, "size": item.size, "mime": item.mime, "key": if self.mode == CryptoMode::V3 { hex::encode(key) } else { String::from_utf8_lossy(&key).into_owned() }, "creation": item.created, "lastModified": item.modified, "blake3": item.hash}).to_string();
            let result = self.request_empty(
                reqwest::Method::POST,
                format!("{}/v3/file/metadata", self.gateway_url),
                Some(serde_json::json!({"uuid": item.uuid, "name": self.encrypt_file_name(new_name, &key)?, "nameHashed": self.hash_name(new_name)?, "metadata": self.encrypt_metadata(&metadata)?})),
            );
            if result.is_ok() {
                self.invalidate_listings();
            }
            result
        }
    }

    /// Update gateway metadata without changing file contents. Timestamps are
    /// sent in milliseconds, matching the Go SDK's metadata representation.
    pub fn update_timestamps(&self, item: &NativeItem, created: i64, modified: i64) -> Result<()> {
        if item.is_dir {
            let metadata = serde_json::json!({"name": item.name, "creation": created}).to_string();
            let result = self.request_empty(
                reqwest::Method::POST,
                format!("{}/v3/dir/metadata", self.gateway_url),
                Some(serde_json::json!({
                    "uuid": item.uuid,
                    "nameHashed": self.hash_name(&item.name)?,
                    "name": self.encrypt_metadata(&metadata)?
                })),
            );
            if result.is_ok() {
                self.invalidate_listings();
            }
            result
        } else {
            let key = item
                .file_key
                .ok_or_else(|| anyhow!("Filen file has no encryption key"))?;
            let metadata = serde_json::json!({
                "name": item.name,
                "size": item.size,
                "mime": item.mime,
                "key": if self.mode == CryptoMode::V3 {
                    hex::encode(key)
                } else {
                    String::from_utf8_lossy(&key).into_owned()
                },
                "creation": created,
                "lastModified": modified,
                "blake3": item.hash
            })
            .to_string();
            let result = self.request_empty(
                reqwest::Method::POST,
                format!("{}/v3/file/metadata", self.gateway_url),
                Some(serde_json::json!({
                    "uuid": item.uuid,
                    "name": self.encrypt_file_name(&item.name, &key)?,
                    "nameHashed": self.hash_name(&item.name)?,
                    "metadata": self.encrypt_metadata(&metadata)?
                })),
            );
            if result.is_ok() {
                self.invalidate_listings();
            }
            result
        }
    }

    /// Replace a remote file using the gateway-safe trash-then-upload flow.
    /// Filen permits duplicate names, so uploading directly would create a
    /// sibling rather than update the existing path.
    pub fn replace_file(&self, item: &NativeItem, mime: &str, data: &[u8]) -> Result<()> {
        anyhow::ensure!(!item.is_dir, "cannot replace a folder with file data");
        self.trash(&item.uuid, "file")?;
        self.upload_file(&item.parent, &item.name, mime, data)
    }

    /// Replace a remote file from a reader without materializing plaintext in
    /// memory. The existing item is trashed before the replacement is
    /// uploaded, matching the gateway-safe semantics of `replace_file`.
    pub fn replace_file_from_reader<R: std::io::Read>(
        &self,
        item: &NativeItem,
        mime: &str,
        size: u64,
        reader: R,
    ) -> Result<()> {
        anyhow::ensure!(!item.is_dir, "cannot replace a folder with file data");
        self.trash(&item.uuid, "file")?;
        self.upload_file_from_reader(&item.parent, &item.name, mime, size, reader)
    }

    /// Replace a remote file from a local path using the streaming reader
    /// implementation.
    pub fn replace_file_from_path(
        &self,
        item: &NativeItem,
        mime: &str,
        local_path: &Path,
    ) -> Result<()> {
        let metadata = std::fs::metadata(local_path)
            .with_context(|| format!("reading Filen replacement {}", local_path.display()))?;
        anyhow::ensure!(metadata.is_file(), "Filen replacement path is not a file");
        let file = std::fs::File::open(local_path)
            .with_context(|| format!("opening Filen replacement {}", local_path.display()))?;
        self.replace_file_from_reader(item, mime, metadata.len(), file)
    }

    pub fn delete_permanent(&self, uuid: &str, is_dir: bool) -> Result<()> {
        let result = self.request_empty(
            reqwest::Method::POST,
            format!(
                "{}/v3/{}/delete/permanent",
                self.gateway_url,
                if is_dir { "dir" } else { "file" }
            ),
            Some(serde_json::json!({"uuid": uuid})),
        );
        if result.is_ok() {
            self.invalidate_listings();
        }
        result
    }

    pub fn copy_file(&self, item: &NativeItem, destination_parent: &str) -> Result<()> {
        anyhow::ensure!(!item.is_dir, "cannot copy a folder with copy_file");
        let temporary =
            std::env::temp_dir().join(format!(".crispsorter-filen-copy-{}", random_uuid()));
        let result = (|| {
            let mut file = std::fs::File::create(&temporary).with_context(|| {
                format!("creating Filen copy staging file {}", temporary.display())
            })?;
            self.download_file_to_writer(item, &mut file)?;
            drop(file);
            let file = std::fs::File::open(&temporary).with_context(|| {
                format!("opening Filen copy staging file {}", temporary.display())
            })?;
            self.upload_file_from_reader(
                destination_parent,
                &item.name,
                &item.mime,
                item.size,
                file,
            )
        })();
        let _ = std::fs::remove_file(&temporary);
        result
    }

    /// Copy a complete subtree, preserving names and file MIME types.
    pub fn copy_item(&self, item: &NativeItem, destination_parent: &str) -> Result<String> {
        if !item.is_dir {
            self.copy_file(item, destination_parent)?;
            return self
                .list_folder(destination_parent)?
                .into_iter()
                .rev()
                .find(|candidate| candidate.name == item.name && !candidate.is_dir)
                .map(|candidate| candidate.uuid)
                .ok_or_else(|| anyhow!("copied file was not returned by the gateway"));
        }
        let copied = self.create_folder(destination_parent, &item.name)?;
        for child in self.list_folder(&item.uuid)? {
            self.copy_item(&child, &copied)?;
        }
        Ok(copied)
    }

    /// Compatibility name shared with the native Internxt client.
    pub fn copy_folder(&self, item: &NativeItem, destination_parent: &str) -> Result<String> {
        anyhow::ensure!(item.is_dir, "cannot copy a file with copy_folder");
        self.copy_item(item, destination_parent)
    }

    pub fn trash(&self, uuid: &str, kind: &str) -> Result<()> {
        let endpoint = if kind == "folder" {
            "v3/dir/trash"
        } else {
            "v3/file/trash"
        };
        let result = self.request_empty(
            reqwest::Method::POST,
            format!("{}/{endpoint}", self.gateway_url),
            Some(serde_json::json!({"uuid": uuid})),
        );
        if result.is_ok() {
            self.invalidate_listings();
        }
        result
    }

    pub fn restore(&self, uuid: &str, kind: &str) -> Result<()> {
        let endpoint = if kind == "folder" {
            "v3/dir/restore"
        } else {
            "v3/file/restore"
        };
        let result = self.request_empty(
            reqwest::Method::POST,
            format!("{}/{endpoint}", self.gateway_url),
            Some(serde_json::json!({"uuid": uuid})),
        );
        if result.is_ok() {
            self.invalidate_listings();
        }
        result
    }

    pub fn list_trash(&self) -> Result<Vec<NativeItem>> {
        self.list_folder("trash")
    }

    pub fn upload_file(&self, parent: &str, name: &str, mime: &str, data: &[u8]) -> Result<()> {
        let (key, key_string) = self.new_file_key()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);
        let metadata = serde_json::json!({"name": name, "size": data.len(), "mime": mime, "key": key_string, "creation": chrono::Utc::now().timestamp_millis(), "lastModified": chrono::Utc::now().timestamp_millis(), "blake3": hasher.finalize().to_hex().to_string()}).to_string();
        let uuid = random_uuid();
        let upload_key = random_uuid().replace('-', "");
        let chunks = data.len().div_ceil(self.transfer_config.chunk_size);
        if data.is_empty() {
            let _: serde_json::Value = self.request(reqwest::Method::POST, format!("{}/v3/upload/empty", self.gateway_url), Some(serde_json::json!({"uuid": uuid, "name": self.encrypt_metadata(name)?, "nameHashed": self.hash_name(name)?, "size": self.encrypt_metadata("0")?, "parent": parent, "mime": self.encrypt_metadata(mime)?, "metadata": self.encrypt_metadata(&metadata)?, "version": if self.mode == CryptoMode::V3 {3} else {2}})))?;
            self.invalidate_listings();
            return Ok(());
        }
        let uploaded = self.upload_chunks(&uuid, parent, &upload_key, &key, data)?;
        let (bucket, region) = uploaded
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("Filen upload produced no chunks"))?;
        let _: serde_json::Value = self.request(reqwest::Method::POST, format!("{}/v3/upload/done", self.gateway_url), Some(serde_json::json!({"uuid": uuid, "name": self.encrypt_metadata(name)?, "nameHashed": self.hash_name(name)?, "size": self.encrypt_metadata(&data.len().to_string())?, "parent": parent, "mime": self.encrypt_metadata(mime)?, "metadata": self.encrypt_metadata(&metadata)?, "version": if self.mode == CryptoMode::V3 {3} else {2}, "chunks": chunks, "rm": "0", "uploadKey": upload_key, "bucket": bucket, "region": region})))?;
        self.invalidate_listings();
        Ok(())
    }

    /// Upload from a reader without requiring the complete plaintext in memory.
    /// `size` must be the exact number of plaintext bytes available from the reader.
    pub fn upload_file_from_reader<R: std::io::Read>(
        &self,
        parent: &str,
        name: &str,
        mime: &str,
        size: u64,
        reader: R,
    ) -> Result<()> {
        self.upload_file_from_reader_with_progress(parent, name, mime, size, reader, |_, _| {})
    }

    /// Reader-based upload with `(completed_bytes, total_bytes)` progress.
    pub fn upload_file_from_reader_with_progress<R: std::io::Read, F: FnMut(u64, u64)>(
        &self,
        parent: &str,
        name: &str,
        mime: &str,
        size: u64,
        mut reader: R,
        mut progress: F,
    ) -> Result<()> {
        let size_usize = usize::try_from(size).context("Filen upload is too large")?;
        let (key, key_string) = self.new_file_key()?;
        let uuid = random_uuid();
        let upload_key = random_uuid().replace('-', "");
        let chunks = size_usize.div_ceil(self.transfer_config.chunk_size);
        let mut hasher = blake3::Hasher::new();
        let mut uploaded_bytes = 0u64;

        if size_usize == 0 {
            let metadata = serde_json::json!({
                "name": name,
                "size": 0,
                "mime": mime,
                "key": key_string,
                "creation": chrono::Utc::now().timestamp_millis(),
                "lastModified": chrono::Utc::now().timestamp_millis(),
                "blake3": blake3::hash(b"").to_hex().to_string()
            })
            .to_string();
            let _: serde_json::Value = self.request(
                reqwest::Method::POST,
                format!("{}/v3/upload/empty", self.gateway_url),
                Some(serde_json::json!({
                    "uuid": uuid,
                    "name": self.encrypt_metadata(name)?,
                    "nameHashed": self.hash_name(name)?,
                    "size": self.encrypt_metadata("0")?,
                    "parent": parent,
                    "mime": self.encrypt_metadata(mime)?,
                    "metadata": self.encrypt_metadata(&metadata)?,
                    "version": if self.mode == CryptoMode::V3 { 3 } else { 2 }
                })),
            )?;
            progress(0, size);
            self.invalidate_listings();
            return Ok(());
        }

        let mut bucket = String::new();
        let mut region = String::new();
        for index in 0..chunks {
            let wanted = self
                .transfer_config
                .chunk_size
                .min(size_usize - uploaded_bytes as usize);
            let mut plain = vec![0u8; wanted];
            let mut read = 0;
            while read < wanted {
                let count = reader.read(&mut plain[read..])?;
                anyhow::ensure!(count > 0, "Filen upload reader ended before declared size");
                read += count;
            }
            hasher.update(&plain);
            let mut nonce = [0u8; 12];
            getrandom::getrandom(&mut nonce)?;
            let encrypted = encrypt_file_chunk(&plain, &key, nonce)?;
            let hash = hex::encode(Sha512::digest(&encrypted));
            let value = upload_chunk_request(
                &self.http,
                &self.api_key,
                format!(
                    "{}/v3/upload?uuid={uuid}&index={index}&parent={parent}&uploadKey={upload_key}&hash={hash}",
                    self.ingest_url
                ),
                &encrypted,
                self.transfer_config.retries,
                self.transfer_config.retry_backoff_ms,
            )?;
            bucket = value.bucket;
            region = value.region;
            uploaded_bytes += read as u64;
            progress(uploaded_bytes, size);
        }
        anyhow::ensure!(
            reader.read(&mut [0u8; 1])? == 0,
            "Filen upload reader has extra data"
        );
        let metadata = serde_json::json!({
            "name": name,
            "size": size,
            "mime": mime,
            "key": key_string,
            "creation": chrono::Utc::now().timestamp_millis(),
            "lastModified": chrono::Utc::now().timestamp_millis(),
            "blake3": hasher.finalize().to_hex().to_string()
        })
        .to_string();
        let _: serde_json::Value = self.request(
            reqwest::Method::POST,
            format!("{}/v3/upload/done", self.gateway_url),
            Some(serde_json::json!({
                "uuid": uuid,
                "name": self.encrypt_metadata(name)?,
                "nameHashed": self.hash_name(name)?,
                "size": self.encrypt_metadata(&size.to_string())?,
                "parent": parent,
                "mime": self.encrypt_metadata(mime)?,
                "metadata": self.encrypt_metadata(&metadata)?,
                "version": if self.mode == CryptoMode::V3 { 3 } else { 2 },
                "chunks": chunks,
                "rm": "0",
                "uploadKey": upload_key,
                "bucket": bucket,
                "region": region
            })),
        )?;
        self.invalidate_listings();
        Ok(())
    }

    /// Upload a local file or directory below `parent` using its local name.
    /// Files are streamed through the reader-based transfer path.
    pub fn upload_path(
        &self,
        parent: &str,
        name: &str,
        mime: &str,
        local_path: &Path,
    ) -> Result<()> {
        self.upload_path_with_timestamps(parent, name, mime, local_path, false)
    }

    /// Recursively upload a local path and optionally preserve local
    /// creation/modification timestamps in Filen metadata.
    pub fn upload_path_with_timestamps(
        &self,
        parent: &str,
        name: &str,
        mime: &str,
        local_path: &Path,
        preserve_timestamps: bool,
    ) -> Result<()> {
        let metadata = std::fs::metadata(local_path)
            .with_context(|| format!("reading local Filen source {}", local_path.display()))?;
        if metadata.is_dir() {
            let folder = self.create_folder(parent, name)?;
            let mut entries: Vec<_> = std::fs::read_dir(local_path)
                .with_context(|| format!("listing local Filen source {}", local_path.display()))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let child_name = entry.file_name().to_string_lossy().into_owned();
                self.upload_path_with_timestamps(
                    &folder,
                    &child_name,
                    mime,
                    &entry.path(),
                    preserve_timestamps,
                )?;
            }
            if preserve_timestamps {
                if let Some(item) = self
                    .list_folder(parent)?
                    .into_iter()
                    .find(|item| item.uuid == folder)
                {
                    let (created, modified) = local_timestamps(&metadata);
                    self.update_timestamps(&item, created, modified)?;
                }
            }
            return Ok(());
        }
        let file = std::fs::File::open(local_path)
            .with_context(|| format!("opening local Filen source {}", local_path.display()))?;
        self.upload_file_from_reader(parent, name, mime, metadata.len(), file)?;
        if preserve_timestamps {
            let item = self
                .list_folder(parent)?
                .into_iter()
                .rev()
                .find(|item| !item.is_dir && item.name == name)
                .ok_or_else(|| anyhow!("uploaded Filen file {name} not returned by listing"))?;
            let (created, modified) = local_timestamps(&metadata);
            self.update_timestamps(&item, created, modified)?;
        }
        Ok(())
    }

    fn upload_chunks(
        &self,
        uuid: &str,
        parent: &str,
        upload_key: &str,
        key: &[u8; 32],
        data: &[u8],
    ) -> Result<Vec<(String, String)>> {
        let chunk_size = self.transfer_config.chunk_size;
        let total = data.len().div_ceil(chunk_size);
        let workers = self.transfer_config.workers.min(total).max(1);
        let next = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel();
        let http = self.http.clone();
        let ingest_url = self.ingest_url.clone();
        let api_key = self.api_key.clone();
        let key = *key;
        let retries = self.transfer_config.retries;
        let retry_backoff_ms = self.transfer_config.retry_backoff_ms;
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let next = Arc::clone(&next);
                let sender = sender.clone();
                let http = http.clone();
                let ingest_url = ingest_url.clone();
                let api_key = api_key.clone();
                scope.spawn(move || {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= total {
                            break;
                        }
                        let result = (|| {
                            let chunk = &data[index * chunk_size
                                ..data.len().min((index + 1) * chunk_size)];
                            let mut nonce = [0u8; 12];
                            getrandom::getrandom(&mut nonce)?;
                            let encrypted = encrypt_file_chunk(chunk, &key, nonce)?;
                            let hash = hex::encode(Sha512::digest(&encrypted));
                            let url = format!("{ingest_url}/v3/upload?uuid={uuid}&index={index}&parent={parent}&uploadKey={upload_key}&hash={hash}");
                            let value = upload_chunk_request(
                                &http,
                                &api_key,
                                url,
                                &encrypted,
                                retries,
                                retry_backoff_ms,
                            )?;
                            Ok::<_, anyhow::Error>((index, value.bucket, value.region))
                        })();
                        if sender.send(result).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(sender);
            let mut results = vec![None; total];
            for result in receiver {
                let (index, bucket, region) = result?;
                results[index] = Some((bucket, region));
            }
            results
                .into_iter()
                .map(|value| value.ok_or_else(|| anyhow!("Filen chunk worker exited early")))
                .collect()
        })
    }

    fn request_empty(
        &self,
        method: reqwest::Method,
        url: String,
        body: Option<serde_json::Value>,
    ) -> Result<()> {
        let mut last_error = None;
        for attempt in 0..self.transfer_config.retries.max(1) {
            let mut request = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&self.api_key);
            if let Some(body) = &body {
                request = request.json(body);
            }
            match request.send() {
                Ok(response) => {
                    let status = response.status();
                    let text = response.text()?;
                    if status.is_success() {
                        let envelope: ApiEnvelope<serde_json::Value> = serde_json::from_str(&text)?;
                        anyhow::ensure!(envelope.status, "Filen API error: {}", envelope.message);
                        return Ok(());
                    }
                    last_error = Some(anyhow!("Filen HTTP {status}: {text}"));
                    if !status.is_server_error() {
                        break;
                    }
                }
                Err(error) => last_error = Some(error.into()),
            }
            if attempt + 1 < self.transfer_config.retries.max(1) {
                std::thread::sleep(Duration::from_millis(
                    self.transfer_config
                        .retry_backoff_ms
                        .saturating_mul(1 << attempt),
                ));
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("Filen mutation request failed")))
    }

    pub fn download_file(&self, item: &NativeItem) -> Result<Vec<u8>> {
        let mut plain = self.download_chunks(item, 0, item.chunks.max(1) as usize)?;
        plain.truncate(item.size as usize);
        Ok(plain)
    }

    /// Download a file directly into a writer, preserving bounded chunk
    /// concurrency without materializing the complete plaintext in memory.
    pub fn download_file_to_writer<W: Write>(
        &self,
        item: &NativeItem,
        writer: &mut W,
    ) -> Result<u64> {
        self.download_chunks_to_writer(item, 0, item.chunks.max(1) as usize, writer)
    }

    /// Streaming download with `(completed_bytes, total_bytes)` progress.
    pub fn download_file_to_writer_with_progress<W: Write, F: FnMut(u64, u64)>(
        &self,
        item: &NativeItem,
        writer: &mut W,
        mut progress: F,
    ) -> Result<u64> {
        self.download_chunks_to_writer_with_progress(
            item,
            0,
            item.chunks.max(1) as usize,
            writer,
            Some(&mut progress),
        )
    }

    /// Download a remote file or directory to a local path. Directory
    /// contents are recreated recursively; files stream directly to disk.
    pub fn download_path(&self, item: &NativeItem, local_path: &Path) -> Result<()> {
        self.download_path_with_timestamps(item, local_path, false)
    }

    /// Recursively download a remote path and optionally preserve remote
    /// modification timestamps. Local timestamp application is best effort:
    /// unsupported precision or permissions must not turn a successful
    /// gateway transfer into a failure.
    pub fn download_path_with_timestamps(
        &self,
        item: &NativeItem,
        local_path: &Path,
        preserve_timestamps: bool,
    ) -> Result<()> {
        if item.is_dir {
            std::fs::create_dir_all(local_path).with_context(|| {
                format!("creating local Filen destination {}", local_path.display())
            })?;
            for child in self.list_folder(&item.uuid)? {
                self.download_path_with_timestamps(
                    &child,
                    &local_path.join(&child.name),
                    preserve_timestamps,
                )?;
            }
            if preserve_timestamps {
                set_local_modified_best_effort(local_path, item.modified);
            }
            return Ok(());
        }
        if let Some(parent) = local_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(local_path).with_context(|| {
            format!("creating local Filen destination {}", local_path.display())
        })?;
        self.download_file_to_writer(item, &mut file)?;
        if preserve_timestamps {
            set_local_modified_best_effort(local_path, item.modified);
        }
        Ok(())
    }

    fn remote_path_size(&self, item: &NativeItem) -> Result<u64> {
        if !item.is_dir {
            return Ok(item.size);
        }
        self.list_folder(&item.uuid)?
            .into_iter()
            .map(|child| self.remote_path_size(&child))
            .sum()
    }

    fn download_path_with_progress<F: FnMut(&str, u64, u64)>(
        &self,
        item: &NativeItem,
        local_path: &Path,
        progress: &mut F,
    ) -> Result<()> {
        if item.is_dir {
            std::fs::create_dir_all(local_path).with_context(|| {
                format!("creating local Filen destination {}", local_path.display())
            })?;
            for child in self.list_folder(&item.uuid)? {
                self.download_path_with_progress(&child, &local_path.join(&child.name), progress)?;
            }
            return Ok(());
        }
        if let Some(parent) = local_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(local_path).with_context(|| {
            format!("creating local Filen destination {}", local_path.display())
        })?;
        self.download_file_to_writer_with_progress(item, &mut file, |done, total| {
            progress(&item.uuid, done, total)
        })?;
        Ok(())
    }

    /// Download only the byte range requested. Only the necessary encrypted
    /// chunks are fetched; the first and last plaintext chunks are trimmed
    /// after authenticated decryption.
    pub fn download_file_range(
        &self,
        item: &NativeItem,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        anyhow::ensure!(offset <= item.size, "Filen range starts beyond file size");
        let end = offset.saturating_add(length).min(item.size);
        if end <= offset {
            return Ok(Vec::new());
        }
        let chunk_size = self.transfer_config.chunk_size;
        let first = (offset as usize) / chunk_size;
        let last = ((end - 1) as usize) / chunk_size;
        let mut plain = self.download_chunks(item, first, last + 1)?;
        let skip = (offset as usize) % chunk_size;
        plain.drain(..skip.min(plain.len()));
        plain.truncate((end - offset) as usize);
        Ok(plain)
    }

    pub fn verify_file_bytes(&self, item: &NativeItem, data: &[u8]) -> bool {
        let mut hasher = blake3::Hasher::new();
        hasher.update(data);
        item.hash.is_empty() || item.hash == hasher.finalize().to_hex().to_string()
    }

    fn download_chunks(
        &self,
        item: &NativeItem,
        start_chunk: usize,
        end_chunk: usize,
    ) -> Result<Vec<u8>> {
        let mut plain = Vec::new();
        self.download_chunks_to_writer(item, start_chunk, end_chunk, &mut plain)?;
        plain.truncate(item.size as usize);
        Ok(plain)
    }

    fn download_chunks_to_writer<W: Write>(
        &self,
        item: &NativeItem,
        start_chunk: usize,
        end_chunk: usize,
        writer: &mut W,
    ) -> Result<u64> {
        self.download_chunks_to_writer_with_progress(item, start_chunk, end_chunk, writer, None)
    }

    fn download_chunks_to_writer_with_progress<W: Write>(
        &self,
        item: &NativeItem,
        start_chunk: usize,
        end_chunk: usize,
        writer: &mut W,
        mut progress: Option<&mut dyn FnMut(u64, u64)>,
    ) -> Result<u64> {
        let key = item
            .file_key
            .ok_or_else(|| anyhow!("Filen item has no file key"))?;
        let total = end_chunk.saturating_sub(start_chunk);
        anyhow::ensure!(total > 0, "Filen download range has no chunks");
        let workers = self.transfer_config.workers.min(total).max(1);
        let next = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel();
        let http = self.http.clone();
        let egest_url = self.egest_url.clone();
        let api_key = self.api_key.clone();
        let region = item.region.clone();
        let bucket = item.bucket.clone();
        let uuid = item.uuid.clone();
        let retries = self.transfer_config.retries;
        let retry_backoff_ms = self.transfer_config.retry_backoff_ms;
        let written = std::thread::scope(|scope| -> Result<u64> {
            for _ in 0..workers {
                let next = Arc::clone(&next);
                let sender = sender.clone();
                let http = http.clone();
                let egest_url = egest_url.clone();
                let api_key = api_key.clone();
                let region = region.clone();
                let bucket = bucket.clone();
                let uuid = uuid.clone();
                scope.spawn(move || loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= total {
                        break;
                    }
                    let result = (|| {
                        let encrypted = download_chunk_request(
                            &http,
                            &api_key,
                            format!(
                                "{egest_url}/{region}/{bucket}/{uuid}/{}",
                                start_chunk + index
                            ),
                            retries,
                            retry_backoff_ms,
                        )?;
                        Ok::<_, anyhow::Error>((index, decrypt_file_chunk(&encrypted, &key)?))
                    })();
                    if sender.send(result).is_err() {
                        break;
                    }
                });
            }
            drop(sender);
            let mut chunks = vec![None; total];
            for result in receiver {
                let (index, plain) = result?;
                chunks[index] = Some(plain);
            }
            let mut written = 0u64;
            let limit = item.size;
            for chunk in chunks {
                let chunk = chunk.ok_or_else(|| anyhow!("Filen download worker exited early"))?;
                let remaining = limit.saturating_sub(written);
                if remaining == 0 {
                    break;
                }
                let take = chunk.len().min(remaining as usize);
                writer.write_all(&chunk[..take])?;
                written += take as u64;
                if let Some(progress) = progress.as_mut() {
                    progress(written, item.size);
                }
            }
            Ok(written)
        })?;
        Ok(written)
    }

    pub fn download_files(&self, items: Vec<NativeItem>) -> Result<Vec<Vec<u8>>> {
        self.download_files_with_progress(items, |_, _| {})
    }

    /// Batch download with `(completed_files, total_files)` progress.
    pub fn download_files_with_progress<F: FnMut(usize, usize)>(
        &self,
        items: Vec<NativeItem>,
        mut progress: F,
    ) -> Result<Vec<Vec<u8>>> {
        let mut last_completed = 0;
        self.download_files_with_byte_progress(items, |completed, total, _, _| {
            if completed > last_completed {
                last_completed = completed;
                progress(completed, total);
            }
        })
    }

    /// Batch download progress with both completed-file and completed-byte
    /// totals. Callbacks are serialized and emitted when each file finishes.
    pub fn download_files_with_byte_progress<F: FnMut(usize, usize, u64, u64)>(
        &self,
        items: Vec<NativeItem>,
        mut progress: F,
    ) -> Result<Vec<Vec<u8>>> {
        let workers = self.transfer_config.file_workers.min(items.len()).max(1);
        let item_count = items.len();
        let total_bytes = items.iter().map(|item| item.size).sum();
        let items = items.as_slice();
        let next = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel();
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let next = Arc::clone(&next);
                let sender = sender.clone();
                scope.spawn(move || loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= items.len() {
                        break;
                    }
                    let result = self.download_file(&items[index]).map(|data| (index, data));
                    if sender.send(result).is_err() {
                        break;
                    }
                });
            }
            drop(sender);
            let mut results = vec![None; item_count];
            let mut completed = 0;
            let mut completed_bytes = 0;
            for result in receiver {
                let (index, data) = result?;
                completed_bytes += data.len() as u64;
                results[index] = Some(data);
                completed += 1;
                progress(completed, item_count, completed_bytes, total_bytes);
            }
            results
                .into_iter()
                .map(|data| data.ok_or_else(|| anyhow!("Filen batch worker exited early")))
                .collect()
        })
    }

    /// Batch path download with durable per-file state and explicit local
    /// conflict handling. Completed jobs are omitted on retry.
    pub fn download_paths_resumable<F: FnMut(usize, usize)>(
        &self,
        jobs: Vec<DownloadPathJob>,
        state_path: &Path,
        conflict: BatchConflictPolicy,
        mut progress: F,
    ) -> Result<()> {
        let mut last_completed = 0;
        self.download_paths_resumable_with_byte_progress(
            jobs,
            state_path,
            conflict,
            |completed, total, _, _| {
                if completed > last_completed {
                    last_completed = completed;
                    progress(completed, total);
                }
            },
        )
    }

    /// Resumable recursive download with serialized aggregate byte progress.
    /// Progress includes decrypted chunks from nested files and treats a
    /// skipped local conflict as completed bytes.
    pub fn download_paths_resumable_with_byte_progress<F: FnMut(usize, usize, u64, u64)>(
        &self,
        jobs: Vec<DownloadPathJob>,
        state_path: &Path,
        conflict: BatchConflictPolicy,
        mut progress: F,
    ) -> Result<()> {
        let mut state = Self::load_batch_transfer_state(state_path)?.unwrap_or_default();
        anyhow::ensure!(state.version == 1, "unsupported Filen batch state version");
        state.completed.sort_unstable();
        Self::save_batch_transfer_state(state_path, &state)?;
        let shared_state = Arc::new(Mutex::new(state));
        let workers = self.transfer_config.file_workers.min(jobs.len()).max(1);
        let total = jobs.len();
        let jobs = jobs.as_slice();
        let next = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel::<ResumableDownloadEvent>();
        let state_path = state_path.to_owned();
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let next = Arc::clone(&next);
                let sender = sender.clone();
                let progress_sender = sender.clone();
                let shared_state = Arc::clone(&shared_state);
                let state_path = state_path.clone();
                scope.spawn(move || loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= jobs.len() {
                        break;
                    }
                    let job = &jobs[index];
                    let key = job.item.uuid.clone();
                    let result = (|| {
                        {
                            let state = shared_state
                                .lock()
                                .map_err(|_| anyhow!("batch state poisoned"))?;
                            if state.completed.binary_search(&key).is_ok() {
                                return Ok(());
                            }
                        }
                        if job.local_path.exists() {
                            match conflict {
                                BatchConflictPolicy::Fail => {
                                    return Err(anyhow!(
                                        "Filen local batch conflict at {}",
                                        job.local_path.display()
                                    ));
                                }
                                BatchConflictPolicy::Skip => {}
                                BatchConflictPolicy::Replace => {
                                    if job.local_path.is_dir() {
                                        std::fs::remove_dir_all(&job.local_path)?;
                                    } else {
                                        std::fs::remove_file(&job.local_path)?;
                                    }
                                }
                            }
                            if conflict == BatchConflictPolicy::Skip {
                                let mut state = shared_state
                                    .lock()
                                    .map_err(|_| anyhow!("batch state poisoned"))?;
                                if state.completed.binary_search(&key).is_err() {
                                    state.completed.push(key.clone());
                                    state.completed.sort_unstable();
                                    Self::save_batch_transfer_state(&state_path, &state)?;
                                }
                                return Ok(());
                            }
                        }
                        let mut file_progress = HashMap::<String, u64>::new();
                        let mut job_bytes = 0u64;
                        let mut report_progress = |uuid: &str, done: u64, _total: u64| {
                            let previous = file_progress.get(uuid).copied().unwrap_or(0);
                            let current = done.max(previous);
                            file_progress.insert(uuid.to_owned(), current);
                            job_bytes += current - previous;
                            let _ = progress_sender.send(ResumableDownloadEvent::Progress {
                                index,
                                bytes: job_bytes,
                            });
                        };
                        self.download_path_with_progress(
                            &job.item,
                            &job.local_path,
                            &mut report_progress,
                        )?;
                        let mut state = shared_state
                            .lock()
                            .map_err(|_| anyhow!("batch state poisoned"))?;
                        if state.completed.binary_search(&key).is_err() {
                            state.completed.push(key);
                            state.completed.sort_unstable();
                            Self::save_batch_transfer_state(&state_path, &state)?;
                        }
                        Ok(())
                    })();
                    if sender
                        .send(ResumableDownloadEvent::Done { index, result })
                        .is_err()
                    {
                        break;
                    }
                });
            }
            drop(sender);
            let mut completed = 0;
            let total_bytes = jobs
                .iter()
                .map(|job| self.remote_path_size(&job.item))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .sum::<u64>();
            let mut completed_bytes = 0;
            let mut per_job_bytes = vec![0u64; total];
            for event in receiver {
                match event {
                    ResumableDownloadEvent::Progress { index, bytes } => {
                        let previous = per_job_bytes[index];
                        let current = bytes.max(previous);
                        per_job_bytes[index] = current;
                        completed_bytes += current - previous;
                        progress(completed, total, completed_bytes, total_bytes);
                    }
                    ResumableDownloadEvent::Done { index, result } => {
                        result?;
                        if per_job_bytes[index] < self.remote_path_size(&jobs[index].item)? {
                            completed_bytes +=
                                self.remote_path_size(&jobs[index].item)? - per_job_bytes[index];
                            per_job_bytes[index] = self.remote_path_size(&jobs[index].item)?;
                        }
                        completed += 1;
                        progress(completed, total, completed_bytes, total_bytes);
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        })?;
        Self::clear_batch_transfer_state(&state_path)
    }
}

#[derive(Debug, Deserialize)]
struct UploadedChunk {
    bucket: String,
    region: String,
}

fn upload_chunk_request(
    http: &reqwest::blocking::Client,
    api_key: &str,
    url: String,
    bytes: &[u8],
    retries: usize,
    retry_backoff_ms: u64,
) -> Result<UploadedChunk> {
    let mut last_error = None;
    for attempt in 0..retries.max(1) {
        match http
            .post(&url)
            .bearer_auth(api_key)
            .body(bytes.to_vec())
            .send()
        {
            Ok(response) => {
                let status = response.status();
                let text = response.text()?;
                if status.is_success() {
                    let envelope: ApiEnvelope<UploadedChunk> = serde_json::from_str(&text)?;
                    anyhow::ensure!(
                        envelope.status,
                        "Filen upload API error: {}",
                        envelope.message
                    );
                    return envelope
                        .data
                        .ok_or_else(|| anyhow!("Filen upload response has no data"));
                }
                last_error = Some(anyhow!("Filen upload HTTP {status}: {text}"));
                if !status.is_server_error() {
                    break;
                }
            }
            Err(error) => last_error = Some(error.into()),
        }
        if attempt + 1 < retries.max(1) {
            std::thread::sleep(Duration::from_millis(
                retry_backoff_ms.saturating_mul(1 << attempt),
            ));
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("Filen upload failed")))
}

fn download_chunk_request(
    http: &reqwest::blocking::Client,
    api_key: &str,
    url: String,
    retries: usize,
    retry_backoff_ms: u64,
) -> Result<Vec<u8>> {
    let mut last_error = None;
    for attempt in 0..retries.max(1) {
        match http.get(&url).bearer_auth(api_key).send() {
            Ok(response) if response.status().is_success() => {
                return Ok(response.bytes()?.to_vec());
            }
            Ok(response) => {
                let status = response.status();
                last_error = Some(anyhow!("Filen download HTTP {status}"));
                if !status.is_server_error() {
                    break;
                }
            }
            Err(error) => last_error = Some(error.into()),
        }
        if attempt + 1 < retries.max(1) {
            std::thread::sleep(Duration::from_millis(
                retry_backoff_ms.saturating_mul(1 << attempt),
            ));
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("Filen download failed")))
}

fn decode_file_key(value: &str) -> Result<[u8; 32]> {
    if value.len() == 64 {
        return hex::decode(value)?
            .try_into()
            .map_err(|_| anyhow!("invalid file key"));
    }
    value
        .as_bytes()
        .try_into()
        .map_err(|_| anyhow!("invalid v2 file key"))
}

fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut dp = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    dp[0][0] = true;
    for i in 0..pattern.len() {
        for j in 0..=value.len() {
            if !dp[i][j] {
                continue;
            }
            match pattern[i] {
                '*' => {
                    let recursive = i + 1 < pattern.len() && pattern[i + 1] == '*';
                    if recursive {
                        dp[i + 1][j] = true;
                        if j < value.len() {
                            dp[i][j + 1] = true;
                        }
                    } else {
                        dp[i + 1][j] = true;
                        if j < value.len() && value[j] != '/' {
                            dp[i][j + 1] = true;
                        }
                    }
                }
                '?' if j < value.len() && value[j] != '/' => dp[i + 1][j + 1] = true,
                c if j < value.len() && c == value[j] => dp[i + 1][j + 1] = true,
                _ => {}
            }
        }
    }
    dp[pattern.len()][value.len()]
}

fn is_literal_glob_component(value: &str) -> bool {
    !value.contains(['*', '?', '[', ']'])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    fn test_session(gateway_url: String) -> FilenSession {
        FilenSession {
            gateway_url,
            ingest_url: "http://127.0.0.1:1".into(),
            egest_url: "http://127.0.0.1:1".into(),
            email: "test@example.test".into(),
            api_key: "test-api-key".into(),
            auth_version: 2,
            file_encryption_version: 2,
            metadata_encryption_version: 2,
            root_folder_uuid: "root".into(),
            master_keys: vec![
                b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_vec(),
            ],
            dek: None,
            kek: None,
            private_key: None,
            hmac_key: None,
        }
    }

    fn spawn_http_server(responses: Vec<String>) -> String {
        spawn_status_server(responses.into_iter().map(|body| (200, body)).collect())
    }

    fn spawn_status_server(responses: Vec<(u16, String)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            }
        });
        address
    }

    fn spawn_raw_server(responses: Vec<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || {
            for body in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        address
    }

    fn spawn_counting_server(
        expected_requests: usize,
        delay_ms: u64,
    ) -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let server_active = Arc::clone(&active);
        let server_peak = Arc::clone(&peak);
        thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().unwrap();
                let active = Arc::clone(&server_active);
                let peak = Arc::clone(&server_peak);
                handlers.push(thread::spawn(move || {
                    let mut request = [0u8; 8192];
                    let size = stream.read(&mut request).unwrap_or(0);
                    let request = String::from_utf8_lossy(&request[..size]);
                    let body = if request.contains("/v3/upload?") {
                        r#"{"status":true,"data":{"bucket":"b","region":"r"}}"#
                    } else {
                        r#"{"status":true,"data":{}}"#
                    };
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(delay_ms));
                    active.fetch_sub(1, Ordering::SeqCst);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                }));
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        (address, active, peak)
    }

    #[test]
    fn v2_login_matches_reference_formula() {
        let (_, password) = pbkdf2_login("password", "salt");
        assert_eq!(password.len(), 128);
        assert_eq!(password, "65773430407d1049af0d42763b5bc2bc8f60ab7f4143d98f7f57a877a951801d38054187db31989a02e83e7a0f5f1a9085a85197d2846b7df28053b46aed4790");
    }

    #[test]
    fn login_preserves_enter_2fa_error_code() {
        let gateway = spawn_http_server(vec![
            r#"{"status":true,"data":{"authVersion":2,"salt":"salt"}}"#.into(),
            r#"{"status":false,"code":"enter_2fa","message":"2FA required"}"#.into(),
        ]);
        let error = FilenNativeClient::login(&gateway, "test@example.test", "password", None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("enter_2fa"), "unexpected error: {error}");
        assert!(error.contains("2FA required"), "unexpected error: {error}");
    }

    #[test]
    fn login_preserves_wrong_2fa_error_code() {
        let gateway = spawn_http_server(vec![
            r#"{"status":true,"data":{"authVersion":2,"salt":"salt"}}"#.into(),
            r#"{"status":false,"code":"wrong_2fa","message":"Invalid code"}"#.into(),
        ]);
        let error =
            FilenNativeClient::login(&gateway, "test@example.test", "password", Some("000000"))
                .unwrap_err()
                .to_string();
        assert!(error.contains("wrong_2fa"), "unexpected error: {error}");
        assert!(error.contains("Invalid code"), "unexpected error: {error}");
    }

    #[test]
    fn v2_filename_hash_matches_vendor_vectors() {
        // FilenCloudDienste/filen-sdk-go/filen/crypto_test.go.
        assert_eq!(v2_hash(b"abc"), "5c5a4ad792911a5a58741e16257f62b664aa2df3");
        assert_eq!(v2_hash(b"cde"), "dc4237084f19afa9eb668edcbc39b5da51f63273");
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

    #[test]
    fn recursive_glob_matches_only_expected_paths() {
        assert!(glob_match("docs/**/*.pdf", "docs/a/b/report.pdf"));
        assert!(glob_match("**/*.pdf", "docs/report.pdf"));
        assert!(glob_match("*.txt", "readme.txt"));
        assert!(!glob_match("*.txt", "docs/readme.txt"));
        assert!(!glob_match("**/*.pdf", "docs/report.txt"));
    }

    #[test]
    fn transfer_config_rejects_invalid_limits() {
        assert!(TransferConfig {
            chunk_size: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert!(TransferConfig {
            workers: 0,
            ..Default::default()
        }
        .validate()
        .is_err());
        assert_eq!(TransferConfig::default().validate().unwrap().workers, 4);
    }

    #[test]
    fn empty_batches_are_noops() {
        let client =
            FilenNativeClient::from_session(&test_session("http://127.0.0.1:1".into())).unwrap();
        client.upload_files(Vec::new()).unwrap();
        assert!(client.download_files(Vec::new()).unwrap().is_empty());
    }

    #[test]
    fn empty_batch_byte_progress_reports_zero_totals() {
        let client =
            FilenNativeClient::from_session(&test_session("http://127.0.0.1:1".into())).unwrap();
        let mut upload = Vec::new();
        client
            .upload_files_with_byte_progress(Vec::new(), |a, b, c, d| upload.push((a, b, c, d)))
            .unwrap();
        assert!(upload.is_empty());
        let mut download = Vec::new();
        assert!(client
            .download_files_with_byte_progress(Vec::new(), |a, b, c, d| {
                download.push((a, b, c, d))
            })
            .unwrap()
            .is_empty());
        assert!(download.is_empty());
    }

    #[test]
    fn batch_file_workers_bound_in_flight_requests() {
        let (gateway, active, peak) = spawn_counting_server(6, 25);
        let mut session = test_session(gateway.clone());
        session.ingest_url = gateway;
        let client = FilenNativeClient::from_session_with_config(
            &session,
            TransferConfig {
                workers: 1,
                file_workers: 2,
                ..Default::default()
            },
        )
        .unwrap();
        let jobs = (0..6)
            .map(|index| UploadJob {
                parent: "root".into(),
                name: format!("empty-{index}.txt"),
                mime: "text/plain".into(),
                data: Vec::new(),
            })
            .collect();
        let mut progress = Vec::new();
        client
            .upload_files_with_byte_progress(jobs, |completed, total, bytes, total_bytes| {
                progress.push((completed, total, bytes, total_bytes))
            })
            .unwrap();
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert!(peak.load(Ordering::SeqCst) >= 2);
        assert_eq!(progress.len(), 6);
        assert!(progress
            .iter()
            .all(|(_, total, _, total_bytes)| { *total == 6 && *total_bytes == 0 }));
        assert_eq!(progress.last(), Some(&(6, 6, 0, 0)));
    }

    #[test]
    fn chunk_workers_bound_in_flight_requests() {
        let (gateway, active, peak) = spawn_counting_server(3, 25);
        let mut session = test_session(gateway.clone());
        session.ingest_url = gateway;
        let client = FilenNativeClient::from_session_with_config(
            &session,
            TransferConfig {
                chunk_size: 8,
                workers: 2,
                file_workers: 1,
                retries: 1,
                ..Default::default()
            },
        )
        .unwrap();
        client
            .upload_file(
                "root",
                "two-chunks.bin",
                "application/octet-stream",
                &[7; 16],
            )
            .unwrap();
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert!(peak.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn upload_resume_state_round_trips_with_completed_chunks() {
        let state = UploadResumeState {
            uuid: "u".into(),
            upload_key: "k".into(),
            file_key: [7; 32],
            parent: "p".into(),
            name: "f".into(),
            mime: "text/plain".into(),
            size: 2_000_000,
            chunk_size: CHUNK_SIZE,
            completed_chunks: vec![0, 2],
            bucket: "b".into(),
            region: "r".into(),
        };
        assert_eq!(
            serde_json::from_str::<UploadResumeState>(&serde_json::to_string(&state).unwrap())
                .unwrap(),
            state
        );
    }

    #[test]
    fn upload_resume_state_persists_and_clears_checkpoint_file() {
        let client =
            FilenNativeClient::from_session(&test_session("http://127.0.0.1:1".into())).unwrap();
        let state = client
            .begin_upload("root", "checkpoint.txt", "text/plain", 5)
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("upload.json");
        client.save_upload_resume_state(&path, &state).unwrap();
        assert_eq!(
            FilenNativeClient::load_upload_resume_state(&path).unwrap(),
            Some(state)
        );
        FilenNativeClient::clear_upload_resume_state(&path).unwrap();
        assert_eq!(
            FilenNativeClient::load_upload_resume_state(&path).unwrap(),
            None
        );
    }

    #[test]
    fn resumable_batch_upload_skips_existing_file_and_cleans_state() {
        let key = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let metadata = v2_encrypt_metadata(
            r#"{"name":"same.txt","size":0,"mime":"text/plain","key":"0123456789abcdef0123456789abcdef","creation":1,"lastModified":2,"blake3":"hash"}"#,
            key,
            *b"abcdefghijkl",
        )
        .unwrap();
        let gateway = spawn_http_server(vec![
            serde_json::json!({
                "status": true,
                "data": {"folders": [], "uploads": [{"uuid":"existing","metadata":metadata,"parent":"root","size":0,"bucket":"b","region":"r","chunks":0,"version":2}]}
            })
            .to_string(),
        ]);
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let state_path = temp.path().join("batch.json");
        let mut progress = Vec::new();
        client
            .upload_files_resumable_with_byte_progress(
                vec![UploadJob {
                    parent: "root".into(),
                    name: "same.txt".into(),
                    mime: "text/plain".into(),
                    data: b"new".to_vec(),
                }],
                &state_path,
                BatchConflictPolicy::Skip,
                |done, total, bytes, total_bytes| progress.push((done, total, bytes, total_bytes)),
            )
            .unwrap();
        assert_eq!(progress, vec![(1, 1, 3, 3)]);
        assert!(!state_path.exists());
    }

    #[test]
    fn hermetic_resume_retries_only_the_missing_chunk() {
        let gateway = spawn_status_server(vec![
            (500, r#"{"status":false,"message":"temporary"}"#.into()),
            (
                200,
                r#"{"status":true,"data":{"bucket":"b","region":"r"}}"#.into(),
            ),
            (200, r#"{"status":true,"data":{}}"#.into()),
        ]);
        let mut session = test_session(gateway.clone());
        session.ingest_url = gateway;
        let client = FilenNativeClient::from_session_with_config(
            &session,
            TransferConfig {
                retries: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut state = client
            .begin_upload("root", "resume.txt", "text/plain", 5)
            .unwrap();
        assert!(client
            .resume_upload_from_reader(&mut state, std::io::Cursor::new(b"hello"))
            .is_err());
        assert!(state.completed_chunks.is_empty());
        client
            .resume_upload_from_reader(&mut state, std::io::Cursor::new(b"hello"))
            .unwrap();
        assert_eq!(state.completed_chunks, vec![0]);
    }

    #[test]
    fn listing_cache_avoids_http_and_invalidation_reloads() {
        let key = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let metadata =
            v2_encrypt_metadata(r#"{"name":"cached.txt","size":5,"mime":"text/plain","key":"0123456789abcdef0123456789abcdef","creation":1,"lastModified":2,"blake3":"hash"}"#, key, *b"abcdefghijkl").unwrap();
        let body = serde_json::json!({
            "status": true,
            "data": {
                "uploads": [{
                    "uuid": "file-1",
                    "metadata": metadata,
                    "parent": "root",
                    "size": 5,
                    "bucket": "bucket",
                    "region": "region",
                    "chunks": 1,
                    "version": 2
                }],
                "folders": []
            }
        })
        .to_string();
        let gateway = spawn_http_server(vec![body.clone(), body]);
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        assert_eq!(client.list_folder("root").unwrap()[0].name, "cached.txt");
        assert_eq!(client.list_folder("root").unwrap()[0].name, "cached.txt");
        client.invalidate_listings();
        assert_eq!(client.list_folder("root").unwrap()[0].uuid, "file-1");
    }

    #[test]
    fn listing_cache_expires_and_fresh_listing_bypasses_cache() {
        let key = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let metadata = v2_encrypt_metadata(
            r#"{"name":"fresh.txt","size":5,"mime":"text/plain","key":"0123456789abcdef0123456789abcdef","creation":1,"lastModified":2,"blake3":"hash"}"#,
            key,
            *b"abcdefghijkl",
        )
        .unwrap();
        let body = serde_json::json!({
            "status": true,
            "data": {"folders": [], "uploads": [{"uuid":"fresh-file","metadata":metadata,"parent":"root","size":5,"bucket":"b","region":"r","chunks":1,"version":2}]}
        })
        .to_string();
        let gateway = spawn_http_server(vec![body.clone(), body.clone(), body]);
        let mut client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        client.set_listing_cache_ttl(Duration::from_millis(1));
        client.list_folder("root").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        client.list_folder("root").unwrap();
        client.list_folder_fresh("root").unwrap();
    }

    #[test]
    fn path_listing_returns_sorted_recursive_paths_with_depth_limit() {
        let key = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let folder_name =
            v2_encrypt_metadata(r#"{"name":"nested","creation":1}"#, key, *b"abcdefghijkl")
                .unwrap();
        let file_metadata = v2_encrypt_metadata(
            r#"{"name":"inside.txt","size":5,"mime":"text/plain","key":"0123456789abcdef0123456789abcdef","creation":1,"lastModified":2,"blake3":"hash"}"#,
            key,
            *b"mnopqrstuvwx",
        )
        .unwrap();
        let gateway = spawn_http_server(vec![
            serde_json::json!({
                "status": true,
                "data": {"folders": [{"uuid":"nested-1","name":folder_name,"parent":"root"}], "uploads": []}
            })
            .to_string(),
            serde_json::json!({
                "status": true,
                "data": {"folders": [], "uploads": [{"uuid":"file-1","metadata":file_metadata,"parent":"nested-1","size":5,"bucket":"b","region":"r","chunks":1,"version":2}]}
            })
            .to_string(),
        ]);
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        let session = test_session("http://127.0.0.1:1".into());
        let shallow = client
            .list_folder_with_paths(&session, Path::new("."), 0)
            .unwrap();
        assert_eq!(shallow.len(), 1);
        assert_eq!(shallow[0].path, Path::new("nested"));
        let deep = client
            .list_folder_with_paths(&session, Path::new("."), -1)
            .unwrap();
        assert_eq!(
            deep.iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            vec![PathBuf::from("nested"), PathBuf::from("nested/inside.txt")]
        );
    }

    #[test]
    fn hermetic_empty_mutation_accepts_success_without_data() {
        let gateway =
            spawn_http_server(vec![r#"{"status":true,"message":"ok","data":null}"#.into()]);
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        client.trash("file-1", "file").unwrap();
    }

    #[test]
    fn hermetic_mutations_retry_transient_5xx() {
        let gateway = spawn_status_server(vec![
            (503, r#"{"status":false,"message":"temporary"}"#.into()),
            (200, r#"{"status":true,"message":"ok","data":null}"#.into()),
        ]);
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        client.trash("file-1", "file").unwrap();
    }

    #[test]
    fn hermetic_mutation_endpoints_accept_gateway_empty_successes() {
        let gateway = spawn_http_server(
            (0..8)
                .map(|_| r#"{"status":true,"message":"ok","data":null}"#.into())
                .collect(),
        );
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        let key = [b'A'; 32];
        let file = NativeItem {
            uuid: "file-1".into(),
            name: "file.txt".into(),
            is_dir: false,
            size: 3,
            parent: "root".into(),
            file_key: Some(key),
            bucket: "bucket".into(),
            region: "region".into(),
            chunks: 1,
            version: 2,
            mime: "text/plain".into(),
            created: 11,
            modified: 22,
            hash: "hash".into(),
        };
        let folder = NativeItem {
            uuid: "folder-1".into(),
            name: "folder".into(),
            is_dir: true,
            size: 0,
            parent: "root".into(),
            file_key: None,
            bucket: String::new(),
            region: String::new(),
            chunks: 0,
            version: 0,
            mime: String::new(),
            created: 11,
            modified: 22,
            hash: String::new(),
        };
        client.move_item(&file.uuid, "destination", false).unwrap();
        client.rename_item(&folder, "renamed-folder").unwrap();
        client.rename_item(&file, "renamed.txt").unwrap();
        client.update_timestamps(&folder, 101, 202).unwrap();
        client.update_timestamps(&file, 303, 404).unwrap();
        client.trash(&file.uuid, "file").unwrap();
        client.restore(&folder.uuid, "folder").unwrap();
        client.delete_permanent(&file.uuid, false).unwrap();
    }

    #[test]
    fn hermetic_exists_and_tree_requests_parse_gateway_shapes() {
        let gateway = spawn_status_server(vec![
            (503, r#"{"status":false,"message":"temporary"}"#.into()),
            (200, r#"{"status":true,"data":{"exists":true}}"#.into()),
            (
                200,
                r#"{"status":true,"data":{"folders":[],"uploads":[]}}"#.into(),
            ),
        ]);
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        assert!(client.file_exists("root", "name.txt").unwrap());
        assert_eq!(
            client.get_flat_folder_tree("root").unwrap()["folders"],
            serde_json::json!([])
        );
    }

    #[test]
    fn hermetic_file_metadata_endpoint_decrypts_native_item() {
        let key = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let metadata = v2_encrypt_metadata(
            r#"{"name":"one.txt","size":3,"mime":"text/plain","key":"0123456789abcdef0123456789abcdef","creation":11,"lastModified":22,"blake3":"hash"}"#,
            key,
            *b"abcdefghijkl",
        )
        .unwrap();
        let gateway = spawn_http_server(vec![serde_json::json!({
            "status": true,
            "data": {
                "uuid": "file-1",
                "metadata": metadata,
                "parent": "root",
                "size": 3,
                "bucket": "bucket",
                "region": "region",
                "chunks": 1,
                "version": 2
            }
        })
        .to_string()]);
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        let item = client.get_file("file-1").unwrap();
        assert_eq!(item.name, "one.txt");
        assert_eq!(item.mime, "text/plain");
        assert_eq!(item.modified, 22);
    }

    #[test]
    fn file_hash_verification_matches_blake3_metadata() {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"hello");
        let item = NativeItem {
            uuid: "f".into(),
            name: "f".into(),
            is_dir: false,
            size: 5,
            parent: "p".into(),
            file_key: None,
            bucket: String::new(),
            region: String::new(),
            chunks: 1,
            version: 3,
            mime: "text/plain".into(),
            created: 0,
            modified: 0,
            hash: hasher.finalize().to_hex().to_string(),
        };
        let client =
            FilenNativeClient::from_session(&test_session("http://127.0.0.1:1".into())).unwrap();
        assert!(client.verify_file_bytes(&item, b"hello"));
        assert!(!client.verify_file_bytes(&item, b"wrong"));
    }

    #[test]
    fn download_to_writer_streams_decrypted_plaintext() {
        let key = [3u8; 32];
        let encrypted = encrypt_file_chunk(b"hello", &key, [1u8; 12]).unwrap();
        let egest = spawn_raw_server(vec![encrypted]);
        let mut session = test_session("http://127.0.0.1:1".into());
        session.egest_url = egest;
        let client = FilenNativeClient::from_session(&session).unwrap();
        let item = NativeItem {
            uuid: "file".into(),
            name: "file.txt".into(),
            is_dir: false,
            size: 5,
            parent: "root".into(),
            file_key: Some(key),
            hash: String::new(),
            chunks: 1,
            bucket: "bucket".into(),
            region: "region".into(),
            version: 2,
            mime: "text/plain".into(),
            created: 0,
            modified: 0,
        };
        let mut output = Vec::new();
        assert_eq!(
            client.download_file_to_writer(&item, &mut output).unwrap(),
            5
        );
        assert_eq!(output, b"hello");
    }

    #[test]
    fn reader_upload_streams_chunks_and_reports_progress() {
        let gateway = spawn_status_server(vec![
            (
                200,
                r#"{"status":true,"data":{"bucket":"b","region":"r"}}"#.into(),
            ),
            (
                200,
                r#"{"status":true,"data":{"bucket":"b","region":"r"}}"#.into(),
            ),
            (
                200,
                r#"{"status":true,"data":{"bucket":"b","region":"r"}}"#.into(),
            ),
            (200, r#"{"status":true,"data":{}}"#.into()),
        ]);
        let mut session = test_session(gateway.clone());
        session.ingest_url = gateway.clone();
        let client = FilenNativeClient::from_session_with_config(
            &session,
            TransferConfig {
                chunk_size: 4,
                workers: 1,
                file_workers: 1,
                retries: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut progress = Vec::new();
        client
            .upload_file_from_reader_with_progress(
                "root",
                "reader.bin",
                "application/octet-stream",
                9,
                std::io::Cursor::new(b"123456789"),
                |done, total| progress.push((done, total)),
            )
            .unwrap();
        assert_eq!(progress, vec![(4, 9), (8, 9), (9, 9)]);
    }

    #[test]
    fn streaming_download_reports_final_progress() {
        let key = [3u8; 32];
        let encrypted = encrypt_file_chunk(b"hello", &key, [1u8; 12]).unwrap();
        let egest = spawn_raw_server(vec![encrypted]);
        let mut session = test_session("http://127.0.0.1:1".into());
        session.egest_url = egest;
        let client = FilenNativeClient::from_session(&session).unwrap();
        let item = NativeItem {
            uuid: "file".into(),
            name: "file.txt".into(),
            is_dir: false,
            size: 5,
            parent: "root".into(),
            file_key: Some(key),
            hash: String::new(),
            chunks: 1,
            bucket: "bucket".into(),
            region: "region".into(),
            version: 2,
            mime: "text/plain".into(),
            created: 0,
            modified: 0,
        };
        let mut output = Vec::new();
        let mut progress = Vec::new();
        assert_eq!(
            client
                .download_file_to_writer_with_progress(&item, &mut output, |done, total| {
                    progress.push((done, total))
                })
                .unwrap(),
            5
        );
        assert_eq!(output, b"hello");
        assert_eq!(progress.last(), Some(&(5, 5)));
    }

    #[test]
    fn upload_path_recurses_local_directories_without_buffering_files() {
        let gateway = spawn_http_server(vec![
            r#"{"status":true,"data":{"uuid":"remote-folder"}}"#.into(),
            r#"{"status":true,"data":{"uuid":"remote-nested"}}"#.into(),
            r#"{"status":true,"data":{}}"#.into(),
        ]);
        let client = FilenNativeClient::from_session(&test_session(gateway)).unwrap();
        let local = tempfile::tempdir().unwrap();
        std::fs::create_dir(local.path().join("nested")).unwrap();
        std::fs::write(local.path().join("nested").join("empty.txt"), b"").unwrap();
        client
            .upload_path("root", "folder", "application/octet-stream", local.path())
            .unwrap();
    }

    #[test]
    fn download_path_streams_file_to_local_destination() {
        let key = [3u8; 32];
        let encrypted = encrypt_file_chunk(b"hello", &key, [1u8; 12]).unwrap();
        let egest = spawn_raw_server(vec![encrypted]);
        let mut session = test_session("http://127.0.0.1:1".into());
        session.egest_url = egest;
        let client = FilenNativeClient::from_session(&session).unwrap();
        let item = NativeItem {
            uuid: "file".into(),
            name: "file.txt".into(),
            is_dir: false,
            size: 5,
            parent: "root".into(),
            file_key: Some(key),
            bucket: "bucket".into(),
            region: "region".into(),
            chunks: 1,
            version: 2,
            mime: "text/plain".into(),
            created: 0,
            modified: 0,
            hash: String::new(),
        };
        let local = tempfile::tempdir().unwrap();
        let destination = local.path().join("nested/file.txt");
        client.download_path(&item, &destination).unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"hello");
    }

    #[test]
    fn batch_download_byte_progress_reports_file_and_byte_totals() {
        let key = [3u8; 32];
        let encrypted = encrypt_file_chunk(b"hello", &key, [1u8; 12]).unwrap();
        let egest = spawn_raw_server(vec![encrypted]);
        let mut session = test_session("http://127.0.0.1:1".into());
        session.egest_url = egest;
        let client = FilenNativeClient::from_session(&session).unwrap();
        let item = NativeItem {
            uuid: "batch-progress-file".into(),
            name: "file.txt".into(),
            is_dir: false,
            size: 5,
            parent: "root".into(),
            file_key: Some(key),
            bucket: "bucket".into(),
            region: "region".into(),
            chunks: 1,
            version: 2,
            mime: "text/plain".into(),
            created: 0,
            modified: 0,
            hash: String::new(),
        };
        let mut progress = Vec::new();
        assert_eq!(
            client
                .download_files_with_byte_progress(vec![item], |a, b, c, d| {
                    progress.push((a, b, c, d))
                })
                .unwrap()[0],
            b"hello"
        );
        assert_eq!(progress, vec![(1, 1, 5, 5)]);
    }

    #[test]
    fn download_path_preserves_timestamp_best_effort() {
        let key = [3u8; 32];
        let encrypted = encrypt_file_chunk(b"hello", &key, [1u8; 12]).unwrap();
        let egest = spawn_raw_server(vec![encrypted]);
        let mut session = test_session("http://127.0.0.1:1".into());
        session.egest_url = egest;
        let client = FilenNativeClient::from_session(&session).unwrap();
        let item = NativeItem {
            uuid: "timestamped-file".into(),
            name: "file.txt".into(),
            is_dir: false,
            size: 5,
            parent: "root".into(),
            file_key: Some(key),
            bucket: "bucket".into(),
            region: "region".into(),
            chunks: 1,
            version: 2,
            mime: "text/plain".into(),
            created: 1_700_000_000_000,
            modified: 1_700_000_123_000,
            hash: String::new(),
        };
        let local = tempfile::tempdir().unwrap();
        let destination = local.path().join("file.txt");
        client
            .download_path_with_timestamps(&item, &destination, true)
            .unwrap();
        let actual = std::fs::metadata(destination)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        assert!((actual - item.modified).abs() < 2_000);
    }

    #[test]
    fn resumable_download_batch_streams_and_cleans_state() {
        let key = [3u8; 32];
        let encrypted = encrypt_file_chunk(b"hello", &key, [1u8; 12]).unwrap();
        let egest = spawn_raw_server(vec![encrypted]);
        let mut session = test_session("http://127.0.0.1:1".into());
        session.egest_url = egest;
        let client = FilenNativeClient::from_session(&session).unwrap();
        let item = NativeItem {
            uuid: "download-batch-file".into(),
            name: "file.txt".into(),
            is_dir: false,
            size: 5,
            parent: "root".into(),
            file_key: Some(key),
            bucket: "bucket".into(),
            region: "region".into(),
            chunks: 1,
            version: 2,
            mime: "text/plain".into(),
            created: 0,
            modified: 0,
            hash: String::new(),
        };
        let local = tempfile::tempdir().unwrap();
        let state_path = local.path().join("batch.json");
        let destination = local.path().join("file.txt");
        let mut progress = Vec::new();
        client
            .download_paths_resumable_with_byte_progress(
                vec![DownloadPathJob {
                    item,
                    local_path: destination.clone(),
                }],
                &state_path,
                BatchConflictPolicy::Replace,
                |done, total, bytes, total_bytes| progress.push((done, total, bytes, total_bytes)),
            )
            .unwrap();
        assert_eq!(std::fs::read(destination).unwrap(), b"hello");
        assert_eq!(progress, vec![(0, 1, 5, 5), (1, 1, 5, 5)]);
        assert!(!state_path.exists());
    }
}
