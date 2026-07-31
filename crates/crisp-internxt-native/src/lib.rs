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
use ripemd::Digest as RipemdDigest;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Sha512};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};
use unicode_normalization::UnicodeNormalization;

type Aes256Ctr = Ctr128BE<Aes256>;
type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

const OPENSSL_MAGIC: &[u8; 8] = b"Salted__";
const INTERNXT_APP_SECRET: &str = "6KYQBP847D4ATSFA";
const MULTIPART_MIN_SIZE: usize = 100 * 1024 * 1024;
const UPLOAD_PART_SIZE: usize = 30 * 1024 * 1024;
const MAX_MULTIPARTS: usize = 10_000;
const MAX_UPLOAD_RETRIES: usize = 5;
const STREAM_BUFFER_SIZE: usize = 1024 * 1024;
const DOWNLOAD_PART_SIZE: u64 = 30 * 1024 * 1024;
const LISTING_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
pub const DEFAULT_DRIVE_API_URL: &str = "https://gateway.internxt.com/drive";
const INTERNXT_NETWORK_URL: &str = "https://gateway.internxt.com/network";

/// Bounded transfer knobs for the Internxt gateway.
///
/// The default deliberately uses one multipart worker. The gateway is more
/// reliable with serial part submission; callers can opt into bounded
/// concurrency through [`InternxtNativeClient::set_transfer_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferConfig {
    pub multipart_min_size: u64,
    pub part_size: usize,
    pub max_multipart_parts: usize,
    pub max_upload_retries: usize,
    pub retry_backoff_ms: u64,
    pub download_part_size: u64,
    pub download_workers: usize,
    pub request_timeout: Duration,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            multipart_min_size: MULTIPART_MIN_SIZE as u64,
            part_size: UPLOAD_PART_SIZE,
            max_multipart_parts: MAX_MULTIPARTS,
            max_upload_retries: MAX_UPLOAD_RETRIES,
            retry_backoff_ms: 1_000,
            download_part_size: DOWNLOAD_PART_SIZE,
            download_workers: 1,
            request_timeout: Duration::from_secs(300),
        }
    }
}

impl TransferConfig {
    pub fn validate(&self) -> Result<()> {
        if self.part_size == 0 || !self.part_size.is_multiple_of(16) {
            return Err(anyhow!(
                "Internxt upload part_size must be positive and 16-byte aligned"
            ));
        }
        if self.multipart_min_size == 0 || self.max_multipart_parts == 0 {
            return Err(anyhow!("Internxt multipart limits must be positive"));
        }
        if self.max_upload_retries == 0
            || self.download_part_size == 0
            || self.download_workers == 0
        {
            return Err(anyhow!("Internxt transfer limits must be positive"));
        }
        if self.request_timeout.is_zero() {
            return Err(anyhow!("Internxt request timeout must be positive"));
        }
        Ok(())
    }
}

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

    /// Bearer credential currently preferred by the gateway after refresh.
    pub fn active_token(&self) -> &str {
        if self.new_token.is_empty() {
            &self.token
        } else {
            &self.new_token
        }
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

/// Encrypt/decrypt a block at an aligned byte offset in the same continuous
/// AES-CTR stream used for the complete file. This is the key primitive for
/// bounded multipart uploads and ranged downloads: each block can be
/// regenerated independently without buffering or reprocessing the prefix.
pub fn crypt_at(
    data: &mut [u8],
    mnemonic: &str,
    bucket_id: &[u8; 12],
    index: &[u8; 32],
    offset: u64,
) -> Result<()> {
    if !offset.is_multiple_of(16) {
        return Err(anyhow!("AES-CTR offset must be 16-byte aligned: {offset}"));
    }
    let key = file_key(mnemonic, bucket_id, index);
    let counter = u128::from_be_bytes(index[..16].try_into().expect("16-byte IV"))
        .wrapping_add(u128::from(offset / 16));
    let iv = counter.to_be_bytes();
    let mut cipher = Aes256Ctr::new((&key).into(), (&iv).into());
    cipher.apply_keystream(data);
    Ok(())
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
    pub modified_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub path: PathBuf,
    pub item: NativeItem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathListing {
    pub path: PathBuf,
    pub item: NativeItem,
}

#[derive(Debug, Clone)]
struct CachedListing {
    expires_at: Instant,
    items: Vec<NativeItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictPolicy {
    Fail,
    Skip,
    Overwrite,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransferStats {
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    pub path: PathBuf,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub files_completed: u64,
    pub folders_completed: u64,
}

pub type ProgressCallback = Arc<dyn Fn(TransferProgress) + Send + Sync>;

struct ProgressReader<R> {
    inner: R,
    path: PathBuf,
    total_bytes: u64,
    completed_bytes: u64,
    callback: ProgressCallback,
}

struct HashingReader<R> {
    reader: R,
    hash: Sha256,
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.reader.read(buffer)?;
        self.hash.update(&buffer[..read]);
        Ok(read)
    }
}

impl<R> ProgressReader<R> {
    fn new(inner: R, path: &str, total_bytes: u64, callback: ProgressCallback) -> Self {
        Self {
            inner,
            path: PathBuf::from(path),
            total_bytes,
            completed_bytes: 0,
            callback,
        }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.completed_bytes = self.completed_bytes.saturating_add(read as u64);
        (self.callback)(TransferProgress {
            path: self.path.clone(),
            completed_bytes: self.completed_bytes,
            total_bytes: self.total_bytes,
            files_completed: u64::from(self.completed_bytes == self.total_bytes),
            folders_completed: 0,
        });
        Ok(read)
    }
}

struct ProgressWriter<W> {
    inner: W,
    path: PathBuf,
    total_bytes: u64,
    completed_bytes: u64,
    callback: ProgressCallback,
}

impl<W> ProgressWriter<W> {
    fn new(inner: W, path: &str, total_bytes: u64, callback: ProgressCallback) -> Self {
        Self {
            inner,
            path: PathBuf::from(path),
            total_bytes,
            completed_bytes: 0,
            callback,
        }
    }
}

impl<W: Write> Write for ProgressWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.completed_bytes = self.completed_bytes.saturating_add(written as u64);
        (self.callback)(TransferProgress {
            path: self.path.clone(),
            completed_bytes: self.completed_bytes,
            total_bytes: self.total_bytes,
            files_completed: u64::from(self.completed_bytes == self.total_bytes),
            folders_completed: 0,
        });
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TransferFilter {
    pub includes: Vec<String>,
    pub excludes: Vec<String>,
}

#[derive(Clone, Default)]
pub struct TransferOptions {
    pub filter: TransferFilter,
    pub preserve_timestamps: bool,
    pub skip_unchanged: bool,
    pub cancellation: Option<Arc<AtomicBool>>,
    pub progress: Option<ProgressCallback>,
}

fn check_cancelled(options: &TransferOptions) -> Result<()> {
    if options
        .cancellation
        .as_ref()
        .is_some_and(|token| token.load(Ordering::Relaxed))
    {
        return Err(anyhow!("transfer cancelled"));
    }
    Ok(())
}

fn report_progress(options: &TransferOptions, path: &Path, stats: &TransferStats, total: u64) {
    if let Some(callback) = &options.progress {
        callback(TransferProgress {
            path: path.to_owned(),
            completed_bytes: stats.bytes,
            total_bytes: total,
            files_completed: stats.files,
            folders_completed: stats.folders,
        });
    }
}

impl TransferFilter {
    fn accepts(&self, name: &str) -> bool {
        if self
            .excludes
            .iter()
            .any(|pattern| wildcard_matches(name, pattern, false))
        {
            return false;
        }
        self.includes.is_empty()
            || self
                .includes
                .iter()
                .any(|pattern| wildcard_matches(name, pattern, false))
    }
}

/// Inspect a local tree without contacting Internxt. Symlinks are rejected
/// deliberately so an upload cannot escape the requested source directory.
pub fn inspect_local_directory(root: &Path) -> Result<TransferStats> {
    let metadata = fs::symlink_metadata(root)
        .with_context(|| format!("reading local source {}", root.display()))?;
    if !metadata.is_dir() {
        return Err(anyhow!(
            "local upload source is not a directory: {}",
            root.display()
        ));
    }
    let mut stats = TransferStats::default();
    inspect_local_directory_contents(root, &mut stats)?;
    Ok(stats)
}

fn inspect_local_directory_contents(root: &Path, stats: &mut TransferStats) -> Result<()> {
    let mut entries = fs::read_dir(root)
        .with_context(|| format!("reading local directory {}", root.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "refusing symlink in upload source: {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            stats.folders += 1;
            inspect_local_directory_contents(&path, stats)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(metadata.len());
        }
    }
    Ok(())
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
#[derive(Clone)]
pub struct InternxtNativeClient {
    base_url: String,
    bearer_token: String,
    http: Client,
    listing_cache: Arc<Mutex<std::collections::HashMap<String, CachedListing>>>,
    verbose: bool,
    transfer_config: TransferConfig,
}

/// Durable state for a resumable upload.
///
/// The `index` is the stable file-encryption identity. The AES file key is
/// deterministically re-derived from that index, the session mnemonic, and
/// the bucket; it must never be regenerated during a resume. `uuid`,
/// `upload_id`, presigned URLs, and completed-part ETags are persisted so a
/// caller can safely inspect or move this state file between processes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadResumeState {
    pub version: u8,
    pub path: String,
    pub bucket_id: String,
    pub file_size: u64,
    pub modified_ns: u128,
    pub part_size: usize,
    pub parts: usize,
    pub index: String,
    pub uuid: String,
    pub upload_id: String,
    pub urls: Vec<String>,
    pub etags: Vec<Option<String>>,
    pub created: u64,
}

type UploadCheckpoint = UploadResumeState;

/// Durable state for a ranged download. The temporary destination contains
/// decrypted bytes at their final offsets; completed ranges are checkpointed
/// only after the bytes have been flushed successfully.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadResumeState {
    pub version: u8,
    pub path: String,
    pub file_uuid: String,
    pub file_size: u64,
    pub part_size: u64,
    pub parts: usize,
    pub completed_parts: Vec<usize>,
    pub created: u64,
}

fn checkpoint_path(path: &Path, bucket_id: &str) -> PathBuf {
    let mut key = Sha256::new();
    key.update(path.to_string_lossy().as_bytes());
    key.update(b"|");
    key.update(bucket_id.as_bytes());
    let name = hex::encode(key.finalize());
    std::env::temp_dir()
        .join("crispsorter-internxt-upload-checkpoints")
        .join(format!("{}.json", name))
}

fn file_modified_ns(path: &Path) -> Result<u128> {
    Ok(path
        .metadata()
        .with_context(|| format!("reading metadata for {}", path.display()))?
        .modified()
        .context("reading file modification time")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("file modification time predates Unix epoch")?
        .as_nanos())
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0)
}

fn save_checkpoint(path: &Path, checkpoint: &UploadCheckpoint) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating checkpoint directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec(checkpoint).context("serializing upload checkpoint")?;
    {
        let mut file = File::create(&temporary)
            .with_context(|| format!("creating checkpoint {}", temporary.display()))?;
        file.write_all(&bytes)
            .context("writing upload checkpoint")?;
        file.sync_data().context("flushing upload checkpoint")?;
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("installing upload checkpoint {}", path.display()))?;
    Ok(())
}

fn load_checkpoint(path: &Path) -> Result<Option<UploadCheckpoint>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let checkpoint = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing upload checkpoint {}", path.display()))?;
    Ok(Some(checkpoint))
}

fn remove_checkpoint(path: &Path) {
    let _ = fs::remove_file(path);
}

fn save_download_checkpoint(path: &Path, state: &DownloadResumeState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating checkpoint directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec(state).context("serializing download checkpoint")?;
    fs::write(&temporary, bytes).with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("installing download checkpoint {}", path.display()))?;
    Ok(())
}

fn load_download_checkpoint(path: &Path) -> Result<Option<DownloadResumeState>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
        format!("parsing download checkpoint {}", path.display())
    })?))
}

struct EncryptReader<R> {
    reader: R,
    cipher: Aes256Ctr,
    remaining: u64,
    hash: Arc<Mutex<Sha256>>,
}

struct HashingWriter<W> {
    writer: W,
    hash: Sha256,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.writer.write(buffer)?;
        self.hash.update(&buffer[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

impl<R: Read> Read for EncryptReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let limit = buffer.len().min(self.remaining as usize);
        let read = self.reader.read(&mut buffer[..limit])?;
        if read == 0 {
            return Ok(0);
        }
        self.cipher.apply_keystream(&mut buffer[..read]);
        self.hash
            .lock()
            .map_err(|_| std::io::Error::other("upload hash mutex poisoned"))?
            .update(&buffer[..read]);
        self.remaining -= read as u64;
        Ok(read)
    }
}

impl InternxtNativeClient {
    pub fn new(base_url: impl Into<String>, bearer_token: impl Into<String>) -> Result<Self> {
        Self::new_with_timeout(base_url, bearer_token, Duration::from_secs(300))
    }

    /// Construct a client with an explicit total HTTP request timeout.
    /// This is useful for callers that need a tighter bound than the
    /// production default while retaining the same blocking API.
    pub fn new_with_timeout(
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self> {
        let config = TransferConfig {
            request_timeout: timeout,
            ..TransferConfig::default()
        };
        Self::new_with_config(base_url, bearer_token, config)
    }

    /// Construct a client with validated, bounded transfer settings.
    pub fn new_with_config(
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
        transfer_config: TransferConfig,
    ) -> Result<Self> {
        transfer_config.validate()?;
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        reqwest::Url::parse(&base_url)
            .with_context(|| format!("invalid Internxt URL: {base_url}"))?;
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .http1_only()
            .timeout(transfer_config.request_timeout)
            .build()
            .context("building Internxt HTTP client")?;
        Ok(Self {
            base_url,
            bearer_token: bearer_token.into(),
            http,
            listing_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
            verbose: false,
            transfer_config,
        })
    }

    /// Enable diagnostic transfer logging for CLI/debug consumers.
    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    /// Replace transfer settings after construction. The HTTP request timeout
    /// is fixed at construction time; use [`Self::new_with_config`] when that
    /// timeout also needs to change.
    pub fn set_transfer_config(&mut self, transfer_config: TransferConfig) -> Result<()> {
        transfer_config.validate()?;
        self.transfer_config = transfer_config;
        Ok(())
    }

    pub fn transfer_config(&self) -> &TransferConfig {
        &self.transfer_config
    }

    /// Run one authenticated operation and, on an HTTP 401/expired-token
    /// response, refresh the session and retry exactly once. The caller owns
    /// persistence of the updated session (CrispSorter stores it in its
    /// keychain); the explicit CLI `refresh` command remains available.
    pub fn with_auto_refresh<T, F>(
        &mut self,
        session: &mut InternxtSession,
        mut operation: F,
    ) -> Result<T>
    where
        F: FnMut(&Self, &InternxtSession) -> Result<T>,
    {
        let first = operation(self, session);
        if !first.as_ref().err().is_some_and(is_expired_token_error) {
            return first;
        }
        let refreshed = self.refresh_session(session)?;
        self.bearer_token = if refreshed.new_token.is_empty() {
            refreshed.token.clone()
        } else {
            refreshed.new_token.clone()
        };
        *session = refreshed;
        operation(self, session)
    }

    /// Return the default durable state location used by [`upload_path`].
    pub fn default_upload_resume_state_path(
        &self,
        session: &InternxtSession,
        path: &Path,
    ) -> PathBuf {
        checkpoint_path(path, &session.bucket_id)
    }

    pub fn load_upload_resume_state(&self, state_path: &Path) -> Result<Option<UploadResumeState>> {
        load_checkpoint(state_path)
    }

    pub fn save_upload_resume_state(
        &self,
        state_path: &Path,
        state: &UploadResumeState,
    ) -> Result<()> {
        save_checkpoint(state_path, state)
    }

    pub fn clear_upload_resume_state(&self, state_path: &Path) {
        remove_checkpoint(state_path);
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
        let http = Client::builder()
            .http1_only()
            .build()
            .context("building Internxt login HTTP client")?;
        let drive_api_url = drive_api_url.trim_end_matches('/');
        let email = email.trim().to_lowercase();
        let security_url = format!("{drive_api_url}/auth/login");
        let security = http
            .post(&security_url)
            .header("content-type", "application/json")
            .header("internxt-client", "internxt-cli")
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
            .header("internxt-client", "internxt-cli")
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
            .header("internxt-client", "internxt-cli")
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

    /// Refresh an expiring session without re-entering the password. The
    /// returned session keeps the existing account/mnemonic fields and only
    /// replaces the bearer tokens returned by `/users/refresh`.
    pub fn refresh_session(&self, session: &InternxtSession) -> Result<InternxtSession> {
        let url = format!("{}/users/refresh", self.base_url);
        let token = session.active_token();
        let response = self
            .http
            .get(&url)
            .bearer_auth(token)
            .header("content-type", "application/json")
            .header("internxt-client", "internxt-cli")
            .send()
            .context("refreshing Internxt session")?;
        let value = self.json_response(response, &url)?;
        let mut refreshed = session.clone();
        refreshed.token = value
            .get("token")
            .and_then(|v| v.as_str())
            .unwrap_or(&session.token)
            .to_owned();
        refreshed.new_token = value
            .get("newToken")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("token").and_then(|v| v.as_str()))
            .ok_or_else(|| anyhow!("Internxt refresh response has no token"))?
            .to_owned();
        Ok(refreshed)
    }

    fn list_page(&self, folder_uuid: &str, kind: &str, offset: usize) -> Result<Vec<NativeItem>> {
        let url = format!(
            "{}/folders/content/{}/{}/",
            self.base_url, folder_uuid, kind
        );
        let mut url = reqwest::Url::parse(&url).context("building Internxt listing URL")?;
        url.query_pairs_mut()
            .append_pair("offset", &offset.to_string())
            .append_pair("limit", "50")
            .append_pair("sort", "plainName")
            .append_pair("order", "ASC");
        let url_text = url.to_string();
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.bearer_token)
            .header("accept", "application/json")
            .header("internxt-client", "internxt-cli")
            .send()
            .context("requesting Internxt folder contents")?;
        let status = response.status();
        let body = response
            .text()
            .context("reading Internxt folder response")?;
        if !status.is_success() {
            return Err(anyhow!(
                "Internxt gateway returned {status} for {url_text}: {body}"
            ));
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
                name: item_name(&item, kind),
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
                modified_at: item
                    .get("modificationTime")
                    .or_else(|| item.get("updatedAt"))
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            })
            .collect())
    }

    fn bearer_response(&self, method: reqwest::Method, url: &str) -> Result<Response> {
        self.http
            .request(method, url)
            .bearer_auth(&self.bearer_token)
            .header("accept", "application/json")
            .header("internxt-client", "internxt-cli")
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
            .header("x-api-version", "2")
            .header("accept", "application/json")
            .header("internxt-client", "internxt-cli");
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

    pub fn folder_metadata(&self, folder_uuid: &str) -> Result<serde_json::Value> {
        let url = format!("{}/folders/{folder_uuid}/meta", self.base_url);
        let response = self.bearer_response(reqwest::Method::GET, &url)?;
        self.json_response(response, &url)
    }

    /// Update a file by streaming a replacement beside the original, then
    /// swapping names. This preserves the gateway's immutable content model
    /// while avoiding a plaintext buffer in memory.
    pub fn update_file(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        replacement: &Path,
    ) -> Result<()> {
        let metadata = self.file_metadata(file_uuid)?;
        let parent = metadata
            .get("folderUuid")
            .or_else(|| metadata.get("folderId"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no parent folder"))?;
        let plain_name = metadata
            .get("plainName")
            .or_else(|| metadata.get("name"))
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no plain name"))?;
        let file_type = metadata
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("file");
        let temporary_name = format!(".crispsorter-update-{}", transfer_token());
        self.upload_path(session, parent, &temporary_name, file_type, replacement)?;
        let temporary = self
            .wait_for_child(
                parent,
                &format_remote_name(&temporary_name, file_type),
                false,
            )?
            .ok_or_else(|| anyhow!("replacement upload was not visible in its parent folder"))?;
        let result = (|| {
            self.trash(file_uuid, "file")?;
            self.rename_file(&temporary.uuid, plain_name, file_type)
        })();
        if result.is_err() {
            let _ = self.trash(&temporary.uuid, "file");
        }
        result
    }

    /// Copy a remote file through a bounded on-disk temporary file.
    pub fn copy_file(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        destination_folder_uuid: &str,
        name_override: Option<&str>,
    ) -> Result<NativeItem> {
        self.copy_file_with_policy(
            session,
            file_uuid,
            destination_folder_uuid,
            name_override,
            ConflictPolicy::Fail,
        )
    }

    pub fn copy_file_with_policy(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        destination_folder_uuid: &str,
        name_override: Option<&str>,
        policy: ConflictPolicy,
    ) -> Result<NativeItem> {
        let metadata = self.file_metadata(file_uuid)?;
        let name = name_override
            .or_else(|| metadata.get("plainName").and_then(|value| value.as_str()))
            .ok_or_else(|| anyhow!("Internxt file metadata has no plain name"))?;
        let file_type = metadata
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("file");
        let remote_name = format_remote_name(name, file_type);
        if let Some(existing) = self.existing_child(destination_folder_uuid, &remote_name, false)? {
            match policy {
                ConflictPolicy::Fail => {
                    return Err(anyhow!("destination file already exists: {remote_name}"));
                }
                ConflictPolicy::Skip => return Ok(existing),
                ConflictPolicy::Overwrite => self.trash(&existing.uuid, "file")?,
            }
        }
        let temporary = std::env::temp_dir().join(format!("crispsorter-copy-{}", transfer_token()));
        // The gateway's presigned shard URLs are not consistently range-aware
        // in production. Copy still remains streaming/bounded, but uses the
        // broadly supported sequential downloader here.
        self.download_file_to_path(session, file_uuid, &temporary)?;
        let result = (|| {
            self.upload_path(
                session,
                destination_folder_uuid,
                name,
                file_type,
                &temporary,
            )?;
            self.wait_for_child(destination_folder_uuid, &remote_name, false)?
                .ok_or_else(|| anyhow!("copied file was not visible in destination folder"))
        })();
        let _ = fs::remove_file(&temporary);
        result
    }

    /// Recursively copy a remote folder and its contents.
    pub fn copy_folder(
        &self,
        session: &InternxtSession,
        source_folder_uuid: &str,
        destination_parent_uuid: &str,
        name_override: Option<&str>,
    ) -> Result<(NativeItem, TransferStats)> {
        self.copy_folder_with_policy(
            session,
            source_folder_uuid,
            destination_parent_uuid,
            name_override,
            ConflictPolicy::Fail,
        )
    }

    pub fn copy_folder_with_policy(
        &self,
        session: &InternxtSession,
        source_folder_uuid: &str,
        destination_parent_uuid: &str,
        name_override: Option<&str>,
        policy: ConflictPolicy,
    ) -> Result<(NativeItem, TransferStats)> {
        let source = self.folder_metadata(source_folder_uuid)?;
        let name = name_override
            .or_else(|| source.get("plainName").and_then(|value| value.as_str()))
            .ok_or_else(|| anyhow!("Internxt folder metadata has no plain name"))?;
        let new_uuid =
            if let Some(existing) = self.existing_child(destination_parent_uuid, name, true)? {
                match policy {
                    ConflictPolicy::Fail => {
                        return Err(anyhow!("destination folder already exists: {name}"));
                    }
                    ConflictPolicy::Skip => {
                        return Ok((
                            existing,
                            TransferStats {
                                skipped: 1,
                                ..TransferStats::default()
                            },
                        ));
                    }
                    ConflictPolicy::Overwrite => {
                        self.trash(&existing.uuid, "folder")?;
                        self.create_folder(destination_parent_uuid, name)?
                    }
                }
            } else {
                self.create_folder(destination_parent_uuid, name)?
            };
        let mut stats = TransferStats {
            folders: 1,
            ..TransferStats::default()
        };
        for item in self.list_folder_cached(source_folder_uuid)? {
            if item.is_dir {
                let (_, child) =
                    self.copy_folder_with_policy(session, &item.uuid, &new_uuid, None, policy)?;
                stats.files += child.files;
                stats.folders += child.folders;
                stats.bytes += child.bytes;
                stats.skipped += child.skipped;
            } else {
                let copied =
                    self.copy_file_with_policy(session, &item.uuid, &new_uuid, None, policy)?;
                stats.files += 1;
                stats.bytes += copied.size;
            }
        }
        Ok((
            NativeItem {
                name: name.to_owned(),
                uuid: new_uuid,
                is_dir: true,
                size: 0,
                modified_at: None,
            },
            stats,
        ))
    }

    pub fn set_file_timestamp(&self, file_uuid: &str, timestamp: &str) -> Result<()> {
        let metadata = self.file_metadata(file_uuid)?;
        let body = serde_json::to_vec(&serde_json::json!({
            "plainName": metadata.get("plainName").and_then(|v| v.as_str()).unwrap_or(""),
            "type": metadata.get("type").and_then(|v| v.as_str()).unwrap_or("file"),
            "modificationTime": timestamp,
        }))?;
        let url = format!("{}/files/{file_uuid}/meta", self.base_url);
        let response = self.bearer_request(reqwest::Method::PUT, &url, body)?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Internxt file timestamp update returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            ));
        }
        self.clear_listing_cache();
        Ok(())
    }

    pub fn set_folder_timestamp(&self, folder_uuid: &str, timestamp: &str) -> Result<()> {
        let metadata = self.folder_metadata(folder_uuid)?;
        let body = serde_json::to_vec(&serde_json::json!({
            "plainName": metadata.get("plainName").and_then(|v| v.as_str()).unwrap_or(""),
            "modificationTime": timestamp,
        }))?;
        let url = format!("{}/folders/{folder_uuid}/meta", self.base_url);
        let response = self.bearer_request(reqwest::Method::PUT, &url, body)?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Internxt folder timestamp update returned {}: {}",
                response.status(),
                response.text().unwrap_or_default()
            ));
        }
        self.clear_listing_cache();
        Ok(())
    }

    fn json_response(&self, response: Response, url: &str) -> Result<serde_json::Value> {
        let status = response.status();
        let body = response
            .text()
            .with_context(|| format!("reading Internxt response: {url}"))?;
        if !status.is_success() {
            return Err(anyhow!("Internxt endpoint {url} returned {status}: {body}"));
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
        let bucket = session.bucket_bytes()?;
        let (index, encrypted) = encrypt(data, &session.mnemonic, &bucket);
        let index_hex = hex::encode(index);
        let parts = multipart_part_count_with_config(data.len(), &self.transfer_config);
        let start_url = format!(
            "{}/v2/buckets/{}/files/start?multiparts={parts}",
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
        let shard_uuid = upload
            .get("uuid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Internxt upload start returned no shard UUID"))?;
        let mut manifest = Vec::new();
        if parts == 1 {
            let upload_url = upload
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Internxt upload start returned no upload URL"))?;
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
        } else {
            let urls = upload
                .get("urls")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("Internxt multipart start returned no part URLs"))?;
            let upload_id = upload
                .get("UploadId")
                .or_else(|| upload.get("uploadId"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Internxt multipart start returned no UploadId"))?;
            if urls.len() < parts {
                return Err(anyhow!(
                    "Internxt multipart start returned {} URLs for {parts} parts",
                    urls.len()
                ));
            }
            for (part_number, chunk) in encrypted.chunks(self.transfer_config.part_size).enumerate()
            {
                let url = urls[part_number]
                    .as_str()
                    .ok_or_else(|| anyhow!("Internxt multipart URL is not text"))?;
                let response = self
                    .http
                    .put(url)
                    .header("content-type", "application/octet-stream")
                    .body(chunk.to_vec())
                    .send()
                    .with_context(|| format!("uploading Internxt part {}", part_number + 1))?;
                let status = response.status();
                if !status.is_success() {
                    return Err(anyhow!(
                        "Internxt part {} upload returned {status}",
                        part_number + 1
                    ));
                }
                let etag = response
                    .headers()
                    .get("etag")
                    .or_else(|| response.headers().get("ETag"))
                    .and_then(|value| value.to_str().ok())
                    .map(|value| value.trim_matches('"').to_owned())
                    .ok_or_else(|| anyhow!("Internxt part {} returned no ETag", part_number + 1))?;
                manifest.push(serde_json::json!({
                    "PartNumber": part_number + 1,
                    "ETag": etag,
                }));
            }
            manifest = vec![serde_json::json!({
                "UploadId": upload_id,
                "parts": manifest,
            })];
        }
        let hash = shard_hash(&encrypted);
        let finish_url = format!(
            "{}/v2/buckets/{}/files/finish",
            session.network_url.trim_end_matches('/'),
            session.bucket_id
        );
        let mut shard = serde_json::json!({"hash": hash, "uuid": shard_uuid});
        if parts > 1 {
            shard["UploadId"] = manifest[0]["UploadId"].clone();
            shard["parts"] = manifest[0]["parts"].clone();
        }
        let finish_body = serde_json::to_vec(&serde_json::json!({
            "index": index_hex,
            "shards": [shard]
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
        self.clear_listing_cache();
        Ok(())
    }

    /// Upload from a non-seekable reader while buffering at most one
    /// encrypted multipart part. `file_size` is required because the gateway
    /// start request and final metadata are size-bearing operations. This API
    /// is intentionally non-resumable; use `upload_path_with_resume_state`
    /// when durable continuation is required.
    pub fn upload_reader<R: Read>(
        &self,
        session: &InternxtSession,
        parent_folder_uuid: &str,
        plain_name: &str,
        file_type: &str,
        mut reader: R,
        file_size: u64,
    ) -> Result<()> {
        let mut part_size = if file_size < self.transfer_config.multipart_min_size {
            file_size.max(1) as usize
        } else {
            self.transfer_config.part_size
        };
        let mut parts = file_size.div_ceil(part_size as u64) as usize;
        if parts > self.transfer_config.max_multipart_parts {
            parts = self.transfer_config.max_multipart_parts;
            part_size = file_size
                .div_ceil(parts as u64)
                .div_ceil(16)
                .saturating_mul(16) as usize;
        }
        let bucket = session.bucket_bytes()?;
        let mut index = [0u8; 32];
        getrandom::getrandom(&mut index)
            .map_err(|error| anyhow!("generating upload file index: {error}"))?;
        let start_url = format!(
            "{}/v2/buckets/{}/files/start?multiparts={parts}",
            session.network_url.trim_end_matches('/'),
            session.bucket_id
        );
        let started = self.json_response(
            self.bridge_response(
                reqwest::Method::POST,
                &start_url,
                session,
                Some(serde_json::to_vec(&serde_json::json!({
                    "uploads": [{"index": 0, "size": file_size}]
                }))?),
            )?,
            &start_url,
        )?;
        let upload = started
            .get("uploads")
            .and_then(|value| value.as_array())
            .and_then(|value| value.first())
            .ok_or_else(|| anyhow!("Internxt upload start returned no upload"))?;
        let shard_uuid = upload
            .get("uuid")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt upload start returned no shard UUID"))?;
        let urls = if parts == 1 {
            vec![upload
                .get("url")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow!("Internxt upload start returned no upload URL"))?
                .to_owned()]
        } else {
            let values = upload
                .get("urls")
                .and_then(|value| value.as_array())
                .ok_or_else(|| anyhow!("Internxt multipart start returned no part URLs"))?;
            if values.len() < parts {
                return Err(anyhow!(
                    "Internxt multipart start returned {} URLs for {parts} parts",
                    values.len()
                ));
            }
            values
                .iter()
                .take(parts)
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("Internxt multipart URL is not text"))
                })
                .collect::<Result<Vec<_>>>()?
        };
        let upload_id = if parts > 1 {
            Some(
                upload
                    .get("UploadId")
                    .or_else(|| upload.get("uploadId"))
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| anyhow!("Internxt multipart start returned no UploadId"))?
                    .to_owned(),
            )
        } else {
            None
        };
        let mut sha = Sha256::new();
        let mut etags = Vec::with_capacity(parts);
        for (part, url) in urls.iter().enumerate().take(parts) {
            let offset = part as u64 * part_size as u64;
            let length = ((file_size - offset) as usize).min(part_size);
            let mut encrypted = vec![0u8; length];
            reader
                .read_exact(&mut encrypted)
                .with_context(|| format!("reading upload part {}", part + 1))?;
            crypt_at(&mut encrypted, &session.mnemonic, &bucket, &index, offset)?;
            sha.update(&encrypted);
            let response = self.put_with_retry(url, encrypted, part + 1, parts)?;
            if parts > 1 {
                let etag = response
                    .headers()
                    .get("etag")
                    .or_else(|| response.headers().get("ETag"))
                    .and_then(|value| value.to_str().ok())
                    .map(|value| value.trim_matches('"').to_owned())
                    .ok_or_else(|| anyhow!("Internxt part {} returned no ETag", part + 1))?;
                etags.push(serde_json::json!({"PartNumber": part + 1, "ETag": etag}));
            }
        }
        let finish_url = format!(
            "{}/v2/buckets/{}/files/finish",
            session.network_url.trim_end_matches('/'),
            session.bucket_id
        );
        let mut shard = serde_json::json!({
            "hash": hex::encode(<ripemd::Ripemd160 as RipemdDigest>::digest(sha.finalize())),
            "uuid": shard_uuid,
        });
        if let Some(upload_id) = upload_id {
            shard["UploadId"] = serde_json::json!(upload_id);
            shard["parts"] = serde_json::json!(etags);
        }
        let finished = self.json_response(
            self.bridge_response(
                reqwest::Method::POST,
                &finish_url,
                session,
                Some(serde_json::to_vec(&serde_json::json!({
                    "index": hex::encode(index), "shards": [shard]
                }))?),
            )?,
            &finish_url,
        )?;
        let network_file_id = finished
            .get("id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt upload finish returned no file id"))?;
        let create_url = format!("{}/files", self.base_url);
        self.json_response(
            self.bearer_request(
                reqwest::Method::POST,
                &create_url,
                serde_json::to_vec(&serde_json::json!({
                    "folderUuid": parent_folder_uuid, "plainName": plain_name,
                    "type": file_type, "size": file_size, "bucket": session.bucket_id,
                    "fileId": network_file_id, "encryptVersion": "Aes03", "name": ""
                }))?,
            )?,
            &create_url,
        )?;
        self.clear_listing_cache();
        Ok(())
    }

    /// Upload a reader and return the plaintext SHA-256 digest after the
    /// gateway has accepted the file.
    pub fn upload_reader_verified<R: Read>(
        &self,
        session: &InternxtSession,
        parent_folder_uuid: &str,
        plain_name: &str,
        file_type: &str,
        reader: R,
        file_size: u64,
    ) -> Result<String> {
        let mut hashing = HashingReader {
            reader,
            hash: Sha256::new(),
        };
        self.upload_reader(
            session,
            parent_folder_uuid,
            plain_name,
            file_type,
            &mut hashing,
            file_size,
        )?;
        Ok(hex::encode(hashing.hash.finalize()))
    }

    /// Upload from a reader and report byte-level progress after each source
    /// read. The callback is invoked synchronously on the calling thread.
    #[allow(clippy::too_many_arguments)]
    pub fn upload_reader_with_progress<R: Read>(
        &self,
        session: &InternxtSession,
        parent_folder_uuid: &str,
        plain_name: &str,
        file_type: &str,
        reader: R,
        file_size: u64,
        progress: ProgressCallback,
    ) -> Result<()> {
        let reader = ProgressReader::new(reader, plain_name, file_size, progress);
        self.upload_reader(
            session,
            parent_folder_uuid,
            plain_name,
            file_type,
            reader,
            file_size,
        )
    }

    /// Stream a local file into Internxt without buffering the complete
    /// plaintext or ciphertext. Multipart parts are regenerated from the
    /// same CTR stream, retried independently, and checkpointed after each
    /// successful ETag so an interrupted process can resume.
    pub fn upload_path(
        &self,
        session: &InternxtSession,
        parent_folder_uuid: &str,
        plain_name: &str,
        file_type: &str,
        path: &Path,
    ) -> Result<()> {
        let state_path = self.default_upload_resume_state_path(session, path);
        self.upload_path_with_resume_state(
            session,
            parent_folder_uuid,
            plain_name,
            file_type,
            path,
            &state_path,
        )
    }

    /// Upload a local file using an explicit durable resume-state file.
    /// Existing state is validated against the source before reuse; successful
    /// completion removes the state file. The state file can be inspected or
    /// persisted by callers through the `load/save/clear_upload_resume_state`
    /// methods.
    pub fn upload_path_with_resume_state(
        &self,
        session: &InternxtSession,
        parent_folder_uuid: &str,
        plain_name: &str,
        file_type: &str,
        path: &Path,
        state_path: &Path,
    ) -> Result<()> {
        self.upload_path_with_resume_state_with_workers(
            session,
            parent_folder_uuid,
            plain_name,
            file_type,
            path,
            state_path,
            1,
        )
    }

    /// Upload a local file with an explicit multipart worker count.
    ///
    /// A worker count of one is the reliable gateway-safe default. Larger
    /// values use the same bounded producer/queue protocol as Internxt's
    /// official client: encryption is performed once, only a small number of
    /// encrypted parts are buffered, and each part independently reports its
    /// ETag and retry result. Values are capped at ten to match that client.
    #[allow(clippy::too_many_arguments)]
    pub fn upload_path_with_resume_state_with_workers(
        &self,
        session: &InternxtSession,
        parent_folder_uuid: &str,
        plain_name: &str,
        file_type: &str,
        path: &Path,
        state_path: &Path,
        workers: usize,
    ) -> Result<()> {
        let workers = workers.clamp(1, 10);
        let metadata = fs::metadata(path)
            .with_context(|| format!("reading upload metadata for {}", path.display()))?;
        let file_size = metadata.len();
        let modified_ns = file_modified_ns(path)?;
        let mut part_size = if file_size < self.transfer_config.multipart_min_size {
            file_size.max(1) as usize
        } else {
            self.transfer_config.part_size
        };
        let mut parts = file_size.div_ceil(part_size as u64) as usize;
        if parts > self.transfer_config.max_multipart_parts {
            parts = self.transfer_config.max_multipart_parts;
            part_size = file_size
                .div_ceil(parts as u64)
                .div_ceil(16)
                .saturating_mul(16) as usize;
        }
        if self.verbose {
            eprintln!(
                "[verbose] upload layout: {} bytes, {} part(s), {} bytes/part, workers={}",
                file_size, parts, part_size, workers
            );
        }
        let cp_path = state_path;
        let mut checkpoint = load_checkpoint(cp_path)?.filter(|value| {
            value.version == 1
                && value.path == path.to_string_lossy()
                && value.bucket_id == session.bucket_id
                && value.file_size == file_size
                && value.modified_ns == modified_ns
                && value.part_size == part_size
                && value.parts == parts
                && value.urls.len() >= parts
                && value.etags.len() == parts
                && hex::decode(&value.index)
                    .map(|v| v.len() == 32)
                    .unwrap_or(false)
        });
        if checkpoint.is_none() {
            remove_checkpoint(cp_path);
        }

        let mut index = [0u8; 32];
        let (uuid, upload_id, urls, mut etags) = if let Some(value) = checkpoint.as_ref() {
            index.copy_from_slice(&hex::decode(&value.index)?);
            (
                value.uuid.clone(),
                value.upload_id.clone(),
                value.urls.clone(),
                value.etags.clone(),
            )
        } else {
            if self.verbose {
                eprintln!("[verbose] requesting upload URLs (multiparts={parts})");
            }
            getrandom::getrandom(&mut index)
                .map_err(|error| anyhow!("generating upload file index: {error}"))?;
            let start_url = format!(
                "{}/v2/buckets/{}/files/start?multiparts={parts}",
                session.network_url.trim_end_matches('/'),
                session.bucket_id
            );
            let start_body = serde_json::to_vec(&serde_json::json!({
                "uploads": [{"index": 0, "size": file_size}]
            }))?;
            let started = self.json_response(
                self.bridge_response(reqwest::Method::POST, &start_url, session, Some(start_body))?,
                &start_url,
            )?;
            let upload = started
                .get("uploads")
                .and_then(|value| value.as_array())
                .and_then(|value| value.first())
                .ok_or_else(|| anyhow!("Internxt upload start returned no upload"))?;
            let uuid = upload
                .get("uuid")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow!("Internxt upload start returned no shard UUID"))?
                .to_owned();
            let urls = if parts == 1 {
                vec![upload
                    .get("url")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| anyhow!("Internxt upload start returned no upload URL"))?
                    .to_owned()]
            } else {
                let values = upload
                    .get("urls")
                    .and_then(|value| value.as_array())
                    .ok_or_else(|| anyhow!("Internxt multipart start returned no part URLs"))?;
                if values.len() < parts {
                    return Err(anyhow!(
                        "Internxt multipart start returned {} URLs for {parts} parts",
                        values.len()
                    ));
                }
                values
                    .iter()
                    .take(parts)
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow!("Internxt multipart URL is not text"))
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            let upload_id = if parts == 1 {
                String::new()
            } else {
                upload
                    .get("UploadId")
                    .or_else(|| upload.get("uploadId"))
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| anyhow!("Internxt multipart start returned no UploadId"))?
                    .to_owned()
            };
            let etags = vec![None; parts];
            if parts > 1 {
                let value = UploadCheckpoint {
                    version: 1,
                    path: path.to_string_lossy().into_owned(),
                    bucket_id: session.bucket_id.clone(),
                    file_size,
                    modified_ns,
                    part_size,
                    parts,
                    index: hex::encode(index),
                    uuid: uuid.clone(),
                    upload_id: upload_id.clone(),
                    urls: urls.clone(),
                    etags: etags.clone(),
                    created: now_seconds(),
                };
                save_checkpoint(cp_path, &value)?;
                checkpoint = Some(value);
            }
            (uuid, upload_id, urls, etags)
        };

        let mut sha = Sha256::new();
        if parts == 1 {
            sha = self.put_file_with_retry(
                &urls[0],
                path,
                file_size,
                &session.mnemonic,
                &session.bucket_bytes()?,
                &index,
            )?;
        } else {
            let mut file = File::open(path)
                .with_context(|| format!("opening upload file {}", path.display()))?;
            // Multipart PUTs are bounded by `workers`. The default API path
            // passes one because the live gateway has reset simultaneous
            // connections; callers can opt into the official client's
            // concurrent-stream protocol when the endpoint supports it.
            let (job_tx, job_rx) = mpsc::sync_channel::<(usize, String, Vec<u8>)>(workers * 2);
            let job_rx = Arc::new(Mutex::new(job_rx));
            let (result_tx, result_rx) = mpsc::channel::<(usize, Result<String>)>();
            let mut jobs = 0usize;
            let bucket = session.bucket_bytes()?;
            let mnemonic = session.mnemonic.clone();

            std::thread::scope(|scope| -> Result<()> {
                for _ in 0..workers {
                    let receiver = Arc::clone(&job_rx);
                    let sender = result_tx.clone();
                    let client = self.clone();
                    scope.spawn(move || loop {
                        let job = receiver.lock().ok().and_then(|guard| guard.recv().ok());
                        let Some((part, url, encrypted)) = job else {
                            break;
                        };
                        if client.verbose {
                            eprintln!(
                                "[verbose] PUT part {}/{} started ({} bytes)",
                                part + 1,
                                parts,
                                encrypted.len()
                            );
                        }
                        let result = client
                            .put_with_retry(&url, encrypted, part + 1, parts)
                            .and_then(|response| {
                                response
                                    .headers()
                                    .get("etag")
                                    .or_else(|| response.headers().get("ETag"))
                                    .and_then(|value| value.to_str().ok())
                                    .map(|value| value.trim_matches('"').to_owned())
                                    .ok_or_else(|| {
                                        anyhow!("Internxt part {} returned no ETag", part + 1)
                                    })
                            });
                        if client.verbose {
                            eprintln!(
                                "[verbose] PUT part {}/{} finished: {}",
                                part + 1,
                                parts,
                                if result.is_ok() { "ok" } else { "error" }
                            );
                        }
                        let _ = sender.send((part, result));
                    });
                }
                drop(result_tx);

                let producer_result = (|| -> Result<()> {
                    for part in 0..parts {
                        let offset = part as u64 * part_size as u64;
                        let length = ((file_size - offset) as usize).min(part_size);
                        let mut encrypted = vec![0u8; length];
                        file.read_exact(&mut encrypted)
                            .with_context(|| format!("reading upload part {}", part + 1))?;
                        crypt_at(&mut encrypted, &mnemonic, &bucket, &index, offset)?;
                        sha.update(&encrypted);
                        if etags[part].is_none() {
                            job_tx
                                .send((part, urls[part].clone(), encrypted))
                                .with_context(|| format!("dispatching upload part {}", part + 1))?;
                            jobs += 1;
                        }
                    }
                    Ok(())
                })();
                drop(job_tx);

                let mut first_error = producer_result.err();
                for _ in 0..jobs {
                    let (part, result) = result_rx.recv().context("collecting upload part")?;
                    match result {
                        Ok(etag) => {
                            etags[part] = Some(etag);
                            if let Some(value) = checkpoint.as_mut() {
                                value.etags[part] = etags[part].clone();
                                save_checkpoint(cp_path, value)?;
                            }
                        }
                        Err(error) => {
                            if first_error.is_none() {
                                first_error = Some(error);
                            }
                        }
                    }
                }
                if let Some(error) = first_error {
                    return Err(error);
                }
                Ok(())
            })?;
        }

        let hash = hex::encode(<ripemd::Ripemd160 as RipemdDigest>::digest(sha.finalize()));
        let finish_url = format!(
            "{}/v2/buckets/{}/files/finish",
            session.network_url.trim_end_matches('/'),
            session.bucket_id
        );
        let mut shard = serde_json::json!({"hash": hash, "uuid": uuid});
        if parts > 1 {
            shard["UploadId"] = serde_json::json!(upload_id);
            shard["parts"] = serde_json::json!(etags
                .iter()
                .enumerate()
                .map(|(part, etag)| serde_json::json!({
                    "PartNumber": part + 1,
                    "ETag": etag.as_ref().expect("all upload parts completed")
                }))
                .collect::<Vec<_>>());
        }
        let finished = self.json_response(
            self.bridge_response(
                reqwest::Method::POST,
                &finish_url,
                session,
                Some(serde_json::to_vec(&serde_json::json!({
                    "index": hex::encode(index),
                    "shards": [shard]
                }))?),
            )?,
            &finish_url,
        )?;
        let network_file_id = finished
            .get("id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt upload finish returned no file id"))?;
        let create_url = format!("{}/files", self.base_url);
        self.json_response(
            self.bearer_request(
                reqwest::Method::POST,
                &create_url,
                serde_json::to_vec(&serde_json::json!({
                    "folderUuid": parent_folder_uuid,
                    "plainName": plain_name,
                    "type": file_type,
                    "size": file_size,
                    "bucket": session.bucket_id,
                    "fileId": network_file_id,
                    "encryptVersion": "Aes03",
                    "name": ""
                }))?,
            )?,
            &create_url,
        )?;
        remove_checkpoint(cp_path);
        Ok(())
    }

    /// Stream-decrypt a remote file directly to disk. The output is
    /// truncated only after metadata is validated, and the AES-CTR state is
    /// advanced incrementally so memory stays bounded by the read buffer.
    pub fn download_file_to_writer<W: Write>(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        mut writer: W,
    ) -> Result<u64> {
        let metadata = self.file_metadata(file_uuid)?;
        let bucket_id = metadata
            .get("bucket")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no bucket"))?;
        let network_id = metadata
            .get("fileId")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no network file id"))?;
        let (url, index_hex) = self.download_links(session, bucket_id, network_id)?;
        let index: [u8; 32] = hex::decode(index_hex)?
            .try_into()
            .map_err(|_| anyhow!("Internxt file index must contain 32 bytes"))?;
        let bucket: [u8; 12] = hex::decode(bucket_id)?
            .try_into()
            .map_err(|_| anyhow!("Internxt bucket must contain 12 bytes"))?;
        let expected = metadata
            .get("size")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .ok_or_else(|| anyhow!("Internxt file metadata has no size"))?;
        let mut response = self
            .http
            .get(url)
            .send()
            .context("streaming encrypted Internxt file")?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("Internxt shard download returned {status}"));
        }
        let key = file_key(&session.mnemonic, &bucket, &index);
        let mut cipher = Aes256Ctr::new((&key).into(), (&index[..16]).into());
        let mut buffer = vec![0u8; STREAM_BUFFER_SIZE];
        let mut total = 0u64;
        loop {
            let read = response
                .read(&mut buffer)
                .context("reading encrypted shard")?;
            if read == 0 {
                break;
            }
            cipher.apply_keystream(&mut buffer[..read]);
            let remaining = expected.saturating_sub(total) as usize;
            if read > remaining {
                return Err(anyhow!("Internxt download exceeds its metadata size"));
            }
            writer
                .write_all(&buffer[..read])
                .context("writing decrypted Internxt file")?;
            total += read as u64;
        }
        if total != expected {
            return Err(anyhow!(
                "Internxt download is incomplete: got {total} bytes, expected {expected}"
            ));
        }
        writer.flush().context("flushing decrypted Internxt file")?;
        Ok(total)
    }

    pub fn download_file_to_writer_verified<W: Write>(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        writer: W,
    ) -> Result<(u64, String)> {
        let mut hashing = HashingWriter {
            writer,
            hash: Sha256::new(),
        };
        let total = self.download_file_to_writer(session, file_uuid, &mut hashing)?;
        Ok((total, hex::encode(hashing.hash.finalize())))
    }

    /// Download to a writer and report byte-level plaintext progress after
    /// each successful write. The callback is invoked synchronously.
    pub fn download_file_to_writer_with_progress<W: Write>(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        writer: W,
        progress: ProgressCallback,
    ) -> Result<u64> {
        let metadata = self.file_metadata(file_uuid)?;
        let total = metadata
            .get("size")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .ok_or_else(|| anyhow!("Internxt file metadata has no size"))?;
        let writer = ProgressWriter::new(writer, file_uuid, total, progress);
        self.download_file_to_writer(session, file_uuid, writer)
    }

    pub fn download_file_to_path(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        path: &Path,
    ) -> Result<()> {
        let metadata = self.file_metadata(file_uuid)?;
        let bucket = metadata
            .get("bucket")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no bucket"))?;
        let network_id = metadata
            .get("fileId")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no network file id"))?;
        let (url, index_hex) = self.download_links(session, bucket, network_id)?;
        let index: [u8; 32] = hex::decode(index_hex)?
            .try_into()
            .map_err(|_| anyhow!("Internxt file index must contain 32 bytes"))?;
        let bucket: [u8; 12] = hex::decode(bucket)?
            .try_into()
            .map_err(|_| anyhow!("Internxt bucket must contain 12 bytes"))?;
        let expected = metadata
            .get("size")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .ok_or_else(|| anyhow!("Internxt file metadata has no size"))?;
        let mut response = self
            .http
            .get(url)
            .send()
            .context("streaming encrypted Internxt file")?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("Internxt shard download returned {status}"));
        }
        let temporary = path.with_extension("crispsorter-partial");
        let mut output = File::create(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        let key = file_key(&session.mnemonic, &bucket, &index);
        let mut cipher = Aes256Ctr::new((&key).into(), (&index[..16]).into());
        let mut buffer = vec![0u8; STREAM_BUFFER_SIZE];
        let mut total = 0u64;
        loop {
            let read = response
                .read(&mut buffer)
                .context("reading encrypted shard")?;
            if read == 0 {
                break;
            }
            cipher.apply_keystream(&mut buffer[..read]);
            output
                .write_all(&buffer[..read])
                .context("writing decrypted Internxt file")?;
            total = total.saturating_add(read as u64);
            if total > expected {
                return Err(anyhow!("Internxt download exceeds its metadata size"));
            }
        }
        if total != expected {
            return Err(anyhow!(
                "Internxt download is incomplete: got {total} bytes, expected {expected}"
            ));
        }
        output
            .sync_all()
            .context("flushing decrypted Internxt file")?;
        drop(output);
        if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&temporary, path)
            .with_context(|| format!("installing downloaded file {}", path.display()))?;
        Ok(())
    }

    /// Resume a ranged download using an explicit durable checkpoint. The
    /// checkpoint records completed ranges, while the temporary output file
    /// stores decrypted bytes at their final offsets.
    pub fn download_file_to_path_resumable(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        path: &Path,
        state_path: &Path,
    ) -> Result<()> {
        let metadata = self.file_metadata(file_uuid)?;
        let bucket_id = metadata
            .get("bucket")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no bucket"))?;
        let network_id = metadata
            .get("fileId")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no network file id"))?;
        let (url, index_hex) = self.download_links(session, bucket_id, network_id)?;
        let index: [u8; 32] = hex::decode(index_hex)?
            .try_into()
            .map_err(|_| anyhow!("Internxt file index must contain 32 bytes"))?;
        let bucket: [u8; 12] = hex::decode(bucket_id)?
            .try_into()
            .map_err(|_| anyhow!("Internxt bucket must contain 12 bytes"))?;
        let expected = metadata
            .get("size")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .ok_or_else(|| anyhow!("Internxt file metadata has no size"))?;
        let part_size = self.transfer_config.download_part_size;
        let parts = expected.div_ceil(part_size) as usize;
        if expected < part_size {
            self.download_file_to_path(session, file_uuid, path)?;
            remove_checkpoint(state_path);
            return Ok(());
        }
        let probe = self
            .http
            .get(&url)
            .header("range", "bytes=0-0")
            .send()
            .context("probing Internxt range support")?;
        if probe.status().as_u16() != 206 {
            self.download_file_to_path(session, file_uuid, path)?;
            remove_checkpoint(state_path);
            return Ok(());
        }
        let mut state = load_download_checkpoint(state_path)?.filter(|value| {
            value.version == 1
                && value.path == path.to_string_lossy()
                && value.file_uuid == file_uuid
                && value.file_size == expected
                && value.part_size == part_size
                && value.parts == parts
                && value.completed_parts.iter().all(|part| *part < parts)
        });
        if state.is_none() {
            remove_checkpoint(state_path);
            state = Some(DownloadResumeState {
                version: 1,
                path: path.to_string_lossy().into_owned(),
                file_uuid: file_uuid.to_owned(),
                file_size: expected,
                part_size,
                parts,
                completed_parts: Vec::new(),
                created: now_seconds(),
            });
            save_download_checkpoint(state_path, state.as_ref().unwrap())?;
        }
        let mut state = state.expect("download state initialized");
        if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("crispsorter-download-partial");
        let mut output = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&temporary)
            .with_context(|| format!("opening {}", temporary.display()))?;
        output
            .set_len(expected)
            .context("sizing resumable download")?;
        for part in 0..parts {
            if state.completed_parts.contains(&part) {
                continue;
            }
            let start = part as u64 * part_size;
            let end = (start + part_size).min(expected);
            let response = self
                .http
                .get(&url)
                .header("range", format!("bytes={start}-{}", end - 1))
                .send()
                .with_context(|| format!("requesting Internxt download range {part}"))?;
            if response.status().as_u16() != 206 {
                return Err(anyhow!(
                    "Internxt download range {part} returned {}",
                    response.status()
                ));
            }
            let mut encrypted = response.bytes()?.to_vec();
            if encrypted.len() != (end - start) as usize {
                return Err(anyhow!(
                    "Internxt download range {part} returned {} bytes, expected {}",
                    encrypted.len(),
                    end - start
                ));
            }
            crypt_at(&mut encrypted, &session.mnemonic, &bucket, &index, start)?;
            output.seek(SeekFrom::Start(start))?;
            output.write_all(&encrypted)?;
            output.sync_data()?;
            state.completed_parts.push(part);
            state.completed_parts.sort_unstable();
            save_download_checkpoint(state_path, &state)?;
        }
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary, path)
            .with_context(|| format!("installing resumable download {}", path.display()))?;
        remove_checkpoint(state_path);
        Ok(())
    }

    /// Download a large file through bounded, concurrent HTTP ranges. S3
    /// presigned URLs normally honor `Range`; when the probe returns a full
    /// object (HTTP 200), this falls back to the sequential streaming path.
    pub fn download_file_to_path_ranged(
        &self,
        session: &InternxtSession,
        file_uuid: &str,
        path: &Path,
    ) -> Result<()> {
        let metadata = self.file_metadata(file_uuid)?;
        let bucket_id = metadata
            .get("bucket")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no bucket"))?;
        let network_id = metadata
            .get("fileId")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("Internxt file metadata has no network file id"))?;
        let (url, index_hex) = self.download_links(session, bucket_id, network_id)?;
        let index: [u8; 32] = hex::decode(index_hex)?
            .try_into()
            .map_err(|_| anyhow!("Internxt file index must contain 32 bytes"))?;
        let bucket: [u8; 12] = hex::decode(bucket_id)?
            .try_into()
            .map_err(|_| anyhow!("Internxt bucket must contain 12 bytes"))?;
        let expected = metadata
            .get("size")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .ok_or_else(|| anyhow!("Internxt file metadata has no size"))?;
        if expected < self.transfer_config.download_part_size {
            return self.download_file_to_path(session, file_uuid, path);
        }

        let probe = self
            .http
            .get(&url)
            .header("range", "bytes=0-0")
            .send()
            .context("probing Internxt range support")?;
        if probe.status().as_u16() != 206 {
            return self.download_file_to_path(session, file_uuid, path);
        }

        let parts = expected.div_ceil(self.transfer_config.download_part_size) as usize;
        let workers = parts.min(self.transfer_config.download_workers);
        let temporary = path.with_extension("crispsorter-partial");
        if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let output = File::create(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        output.set_len(expected).context("sizing ranged download")?;
        let output = Arc::new(Mutex::new(output));
        let (job_tx, job_rx) = mpsc::sync_channel::<usize>(workers * 2);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (result_tx, result_rx) = mpsc::channel::<Result<()>>();
        let result = std::thread::scope(|scope| -> Result<()> {
            for _ in 0..workers {
                let receiver = Arc::clone(&job_rx);
                let sender = result_tx.clone();
                let client = self.clone();
                let url = url.clone();
                let output = Arc::clone(&output);
                scope.spawn(move || loop {
                    let part = receiver.lock().ok().and_then(|guard| guard.recv().ok());
                    let Some(part) = part else { break };
                    let start = part as u64 * client.transfer_config.download_part_size;
                    let end = (start + client.transfer_config.download_part_size).min(expected);
                    let result = (|| -> Result<()> {
                        let response = client
                            .http
                            .get(&url)
                            .header("range", format!("bytes={start}-{}", end - 1))
                            .send()
                            .with_context(|| format!("requesting Internxt range {part}"))?;
                        if response.status().as_u16() != 206 {
                            return Err(anyhow!(
                                "Internxt range {part} returned {}",
                                response.status()
                            ));
                        }
                        let mut encrypted = response.bytes()?.to_vec();
                        if encrypted.len() != (end - start) as usize {
                            return Err(anyhow!(
                                "Internxt range {part} returned {} bytes, expected {}",
                                encrypted.len(),
                                end - start
                            ));
                        }
                        crypt_at(&mut encrypted, &session.mnemonic, &bucket, &index, start)?;
                        let mut output = output
                            .lock()
                            .map_err(|_| anyhow!("ranged output mutex poisoned"))?;
                        output.seek(std::io::SeekFrom::Start(start))?;
                        output.write_all(&encrypted)?;
                        Ok(())
                    })();
                    let failed = result.is_err();
                    let _ = sender.send(result);
                    if failed {
                        // Other workers drain their already-dispatched ranges;
                        // the caller reports the first error after joining.
                    }
                });
            }
            drop(result_tx);
            for part in 0..parts {
                job_tx.send(part).context("dispatching download range")?;
            }
            drop(job_tx);
            let mut first_error = None;
            for _ in 0..parts {
                if let Err(error) = result_rx.recv().context("collecting download range")? {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
            Ok(())
        });
        drop(output);
        if let Err(error) = result {
            remove_checkpoint(&temporary);
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        fs::rename(&temporary, path)
            .with_context(|| format!("installing ranged download {}", path.display()))?;
        Ok(())
    }

    fn put_with_retry(
        &self,
        url: &str,
        body: Vec<u8>,
        part: usize,
        total: usize,
    ) -> Result<Response> {
        let mut last_error = None;
        for attempt in 0..self.transfer_config.max_upload_retries {
            match self
                .http
                .put(url)
                .header("content-type", "application/octet-stream")
                .body(body.clone())
                .send()
            {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) if response.status().as_u16() == 403 => {
                    return Err(anyhow!(
                        "Internxt part {part}/{total} upload URL expired (HTTP 403)"
                    ));
                }
                Ok(response) => {
                    last_error = Some(anyhow!(
                        "Internxt part {part}/{total} returned {}",
                        response.status()
                    ));
                }
                Err(error) => last_error = Some(error.into()),
            }
            if attempt + 1 < self.transfer_config.max_upload_retries {
                std::thread::sleep(Duration::from_millis(
                    self.transfer_config
                        .retry_backoff_ms
                        .saturating_mul(1u64 << attempt.min(10)),
                ));
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("Internxt part {part}/{total} failed")))
    }

    fn put_file_with_retry(
        &self,
        url: &str,
        path: &Path,
        file_size: u64,
        mnemonic: &str,
        bucket: &[u8; 12],
        index: &[u8; 32],
    ) -> Result<Sha256> {
        let mut last_error = None;
        for attempt in 0..self.transfer_config.max_upload_retries {
            let hash = Arc::new(Mutex::new(Sha256::new()));
            let key = file_key(mnemonic, bucket, index);
            let reader = EncryptReader {
                reader: File::open(path)
                    .with_context(|| format!("opening upload file {}", path.display()))?,
                cipher: Aes256Ctr::new((&key).into(), (&index[..16]).into()),
                remaining: file_size,
                hash: Arc::clone(&hash),
            };
            match self
                .http
                .put(url)
                .header("content-type", "application/octet-stream")
                .body(reqwest::blocking::Body::sized(reader, file_size))
                .send()
            {
                Ok(response) if response.status().is_success() => {
                    return Ok(hash
                        .lock()
                        .map_err(|_| anyhow!("upload hash mutex poisoned"))?
                        .clone());
                }
                Ok(response) if response.status().as_u16() == 403 => {
                    return Err(anyhow!("Internxt upload URL expired (HTTP 403)"));
                }
                Ok(response) => {
                    last_error = Some(anyhow!(
                        "Internxt shard upload returned {}",
                        response.status()
                    ));
                }
                Err(error) => last_error = Some(error.into()),
            }
            if attempt + 1 < self.transfer_config.max_upload_retries {
                std::thread::sleep(Duration::from_millis(
                    self.transfer_config
                        .retry_backoff_ms
                        .saturating_mul(1u64 << attempt.min(10)),
                ));
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("Internxt shard upload failed")))
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
            .header("accept", "application/json")
            .header("internxt-client", "internxt-cli")
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

    /// List a folder with a short-lived in-memory cache. Mutating operations
    /// clear this cache because the gateway does not expose reliable etags.
    pub fn list_folder_cached(&self, folder_uuid: &str) -> Result<Vec<NativeItem>> {
        if let Ok(mut cache) = self.listing_cache.lock() {
            if let Some(entry) = cache.get(folder_uuid) {
                if entry.expires_at > Instant::now() {
                    return Ok(entry.items.clone());
                }
                cache.remove(folder_uuid);
            }
        }
        let items = self.list_folder(folder_uuid)?;
        if let Ok(mut cache) = self.listing_cache.lock() {
            cache.insert(
                folder_uuid.to_owned(),
                CachedListing {
                    expires_at: Instant::now() + LISTING_CACHE_TTL,
                    items: items.clone(),
                },
            );
        }
        Ok(items)
    }

    pub fn clear_listing_cache(&self) {
        if let Ok(mut cache) = self.listing_cache.lock() {
            cache.clear();
        }
    }

    /// Find files recursively by a shell-style `*`/`?` pattern.
    pub fn search_files(
        &self,
        session: &InternxtSession,
        pattern: &str,
        case_sensitive: bool,
        max_depth: isize,
    ) -> Result<Vec<SearchResult>> {
        self.search_files_from(session, Path::new("."), pattern, case_sensitive, max_depth)
    }

    pub fn search_files_from(
        &self,
        session: &InternxtSession,
        folder_path: &Path,
        pattern: &str,
        case_sensitive: bool,
        max_depth: isize,
    ) -> Result<Vec<SearchResult>> {
        let folder = self.resolve_path(session, folder_path)?;
        if !folder.is_dir {
            return Err(anyhow!("search starting path is not a folder"));
        }
        let mut results = Vec::new();
        self.search_folder(
            &folder.uuid,
            if folder_path == Path::new(".") {
                Path::new("")
            } else {
                folder_path
            },
            pattern,
            case_sensitive,
            max_depth,
            0,
            &mut results,
        )?;
        results.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(results)
    }

    /// Return every item below a folder with its remote path, including
    /// directories. `max_depth` is relative to the requested folder.
    pub fn list_folder_with_paths(
        &self,
        session: &InternxtSession,
        folder_path: &Path,
        max_depth: isize,
    ) -> Result<Vec<PathListing>> {
        let folder = self.resolve_path(session, folder_path)?;
        if !folder.is_dir {
            return Err(anyhow!("listing starting path is not a folder"));
        }
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
        output: &mut Vec<PathListing>,
    ) -> Result<()> {
        for item in self.list_folder_cached(folder_uuid)? {
            let path = parent_path.join(&item.name);
            output.push(PathListing {
                path: path.clone(),
                item: item.clone(),
            });
            if item.is_dir && (max_depth < 0 || depth < max_depth) {
                self.list_paths_recursive(&item.uuid, &path, max_depth, depth + 1, output)?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn search_folder(
        &self,
        folder_uuid: &str,
        parent_path: &Path,
        pattern: &str,
        case_sensitive: bool,
        max_depth: isize,
        depth: isize,
        results: &mut Vec<SearchResult>,
    ) -> Result<()> {
        for item in self.list_folder_cached(folder_uuid)? {
            let path = parent_path.join(&item.name);
            if !item.is_dir && wildcard_matches(&item.name, pattern, case_sensitive) {
                results.push(SearchResult {
                    path: path.clone(),
                    item: item.clone(),
                });
            }
            if item.is_dir && (max_depth < 0 || depth < max_depth) {
                self.search_folder(
                    &item.uuid,
                    &path,
                    pattern,
                    case_sensitive,
                    max_depth,
                    depth + 1,
                    results,
                )?;
            }
        }
        Ok(())
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
            modified_at: None,
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
                .list_folder_cached(&current.uuid)?
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
        let result = value
            .get("uuid")
            .or_else(|| value.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Internxt folder creation returned no UUID"));
        if result.is_ok() {
            self.clear_listing_cache();
        }
        result
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
        self.clear_listing_cache();
        Ok(())
    }

    pub fn move_file(&self, uuid: &str, destination_folder_uuid: &str) -> Result<()> {
        self.move_item(uuid, destination_folder_uuid, "files")
    }

    pub fn move_folder(&self, uuid: &str, destination_folder_uuid: &str) -> Result<()> {
        self.move_item(uuid, destination_folder_uuid, "folders")
    }

    fn move_item(&self, uuid: &str, destination_folder_uuid: &str, kind: &str) -> Result<()> {
        let url = format!("{}/{}/{uuid}", self.base_url, kind);
        let body = serde_json::to_vec(&serde_json::json!({
            "destinationFolder": destination_folder_uuid
        }))?;
        let response = self.bearer_request(reqwest::Method::PATCH, &url, body)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("Internxt move endpoint returned {status}: {body}"));
        }
        self.clear_listing_cache();
        Ok(())
    }

    pub fn rename_file(&self, uuid: &str, plain_name: &str, file_type: &str) -> Result<()> {
        let url = format!("{}/files/{uuid}/meta", self.base_url);
        let body = serde_json::to_vec(&serde_json::json!({
            "plainName": plain_name,
            "type": file_type
        }))?;
        let response = self.bearer_request(reqwest::Method::PUT, &url, body)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("Internxt file rename returned {status}: {body}"));
        }
        self.clear_listing_cache();
        Ok(())
    }

    pub fn rename_folder(&self, uuid: &str, plain_name: &str) -> Result<()> {
        let url = format!("{}/folders/{uuid}/meta", self.base_url);
        let body = serde_json::to_vec(&serde_json::json!({ "plainName": plain_name }))?;
        let response = self.bearer_request(reqwest::Method::PUT, &url, body)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            return Err(anyhow!("Internxt folder rename returned {status}: {body}"));
        }
        self.clear_listing_cache();
        Ok(())
    }

    /// Move an item from trash back into a folder. The gateway's reliable
    /// restore mechanism is the same destination-folder PATCH used by move.
    pub fn restore_from_trash(
        &self,
        uuid: &str,
        kind: &str,
        destination_folder_uuid: &str,
    ) -> Result<()> {
        self.move_item(uuid, destination_folder_uuid, kind)
    }

    /// Permanently delete one item that is already in trash.
    pub fn permanently_delete(&self, uuid: &str, kind: &str) -> Result<()> {
        let url = format!("{}/storage/trash", self.base_url);
        let body = serde_json::to_vec(&serde_json::json!({
            "items": [{"uuid": uuid, "type": kind}]
        }))?;
        let response = self.bearer_request(reqwest::Method::DELETE, &url, body)?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!(
                "Internxt permanent-delete endpoint returned {status}: {}",
                response.text().unwrap_or_default()
            ));
        }
        self.clear_listing_cache();
        Ok(())
    }

    /// Permanently empty the account trash.
    pub fn clear_trash(&self) -> Result<()> {
        let url = format!("{}/storage/trash/all", self.base_url);
        let response = self
            .http
            .delete(&url)
            .bearer_auth(&self.bearer_token)
            .header("accept", "application/json")
            .header("internxt-client", "internxt-cli")
            .send()
            .with_context(|| format!("requesting Internxt trash clear: {url}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!(
                "Internxt trash-clear endpoint returned {status}: {}",
                response.text().unwrap_or_default()
            ));
        }
        self.clear_listing_cache();
        Ok(())
    }

    /// List paginated files and folders currently in trash.
    pub fn list_trash(&self, kind: Option<&str>, limit: usize) -> Result<Vec<NativeItem>> {
        let mut all = Vec::new();
        for requested_kind in kind
            .into_iter()
            .chain(["files", "folders"].into_iter().filter(|_| kind.is_none()))
        {
            let mut offset = 0usize;
            loop {
                let url = format!("{}/storage/trash/paginated", self.base_url);
                let mut parsed = reqwest::Url::parse(&url)?;
                parsed
                    .query_pairs_mut()
                    .append_pair("offset", &offset.to_string())
                    .append_pair("limit", &limit.max(1).to_string())
                    .append_pair("type", requested_kind);
                let url_text = parsed.to_string();
                let response = self
                    .http
                    .get(parsed)
                    .bearer_auth(&self.bearer_token)
                    .header("accept", "application/json")
                    .header("internxt-client", "internxt-cli")
                    .send()
                    .with_context(|| format!("requesting Internxt trash listing: {url_text}"))?;
                let status = response.status();
                let body = response.text()?;
                if !status.is_success() {
                    return Err(anyhow!("Internxt trash listing returned {status}: {body}"));
                }
                let value: serde_json::Value = serde_json::from_str(&body)?;
                let values = value
                    .get("result")
                    .or_else(|| value.get("items"))
                    .and_then(|item| item.as_array())
                    .cloned()
                    .unwrap_or_default();
                let count = values.len();
                for item in values {
                    all.push(NativeItem {
                        name: item
                            .get("plainName")
                            .or_else(|| item.get("name"))
                            .and_then(|value| value.as_str())
                            .unwrap_or("?")
                            .to_owned(),
                        uuid: item
                            .get("uuid")
                            .or_else(|| item.get("id"))
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_owned(),
                        modified_at: item
                            .get("modificationTime")
                            .or_else(|| item.get("updatedAt"))
                            .and_then(|value| value.as_str())
                            .map(str::to_owned),
                        is_dir: requested_kind == "folders",
                        size: item
                            .get("size")
                            .and_then(|value| {
                                value.as_u64().or_else(|| value.as_str()?.parse().ok())
                            })
                            .unwrap_or(0),
                    });
                }
                if count < limit.max(1) {
                    break;
                }
                offset += count;
            }
        }
        Ok(all)
    }

    /// Recursively upload a local directory into an existing remote folder.
    /// Traversal is deterministic and conflict handling is explicit.
    pub fn upload_directory(
        &self,
        session: &InternxtSession,
        local_root: &Path,
        remote_parent_uuid: &str,
        policy: ConflictPolicy,
    ) -> Result<TransferStats> {
        self.upload_directory_filtered(
            session,
            local_root,
            remote_parent_uuid,
            policy,
            &TransferFilter::default(),
        )
    }

    pub fn upload_directory_filtered(
        &self,
        session: &InternxtSession,
        local_root: &Path,
        remote_parent_uuid: &str,
        policy: ConflictPolicy,
        filter: &TransferFilter,
    ) -> Result<TransferStats> {
        self.upload_directory_with_options(
            session,
            local_root,
            remote_parent_uuid,
            policy,
            TransferOptions {
                filter: filter.clone(),
                ..TransferOptions::default()
            },
        )
    }

    pub fn upload_directory_with_options(
        &self,
        session: &InternxtSession,
        local_root: &Path,
        remote_parent_uuid: &str,
        policy: ConflictPolicy,
        options: TransferOptions,
    ) -> Result<TransferStats> {
        let local_summary = inspect_local_directory(local_root)?;
        check_cancelled(&options)?;
        let mut stats = TransferStats::default();
        self.upload_directory_contents(
            session,
            local_root,
            remote_parent_uuid,
            policy,
            &options,
            &mut stats,
        )?;
        report_progress(&options, local_root, &stats, local_summary.bytes);
        Ok(stats)
    }

    fn upload_directory_contents(
        &self,
        session: &InternxtSession,
        local_root: &Path,
        remote_parent_uuid: &str,
        policy: ConflictPolicy,
        options: &TransferOptions,
        stats: &mut TransferStats,
    ) -> Result<()> {
        check_cancelled(options)?;
        let mut entries = fs::read_dir(local_root)
            .with_context(|| format!("reading local directory {}", local_root.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            check_cancelled(options)?;
            let local = entry.path();
            let name = sanitize_filename(&entry.file_name().to_string_lossy());
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                let remote = self.existing_child(remote_parent_uuid, &name, true)?;
                let folder_was_created = remote.is_none();
                let folder_uuid = match remote {
                    Some(item) => match policy {
                        ConflictPolicy::Fail => {
                            return Err(anyhow!("remote folder already exists: {name}"))
                        }
                        ConflictPolicy::Skip | ConflictPolicy::Overwrite => item.uuid,
                    },
                    None => self.create_folder(remote_parent_uuid, &name)?,
                };
                if options.preserve_timestamps && folder_was_created {
                    if let Ok(timestamp) = local_timestamp(&metadata) {
                        self.set_folder_timestamp(&folder_uuid, &timestamp)?;
                    }
                }
                stats.folders += 1;
                report_progress(options, &local, stats, 0);
                self.upload_directory_contents(
                    session,
                    &local,
                    &folder_uuid,
                    policy,
                    options,
                    stats,
                )?;
            } else if metadata.is_file() {
                if !options.filter.accepts(&name) {
                    stats.skipped += 1;
                    continue;
                }
                let (stem, extension) = split_remote_name(&name);
                let remote = self.existing_child(
                    remote_parent_uuid,
                    &format_remote_name(stem, extension),
                    false,
                )?;
                if let Some(item) = remote {
                    if options.skip_unchanged
                        && policy != ConflictPolicy::Fail
                        && item.size == metadata.len()
                    {
                        stats.skipped += 1;
                        continue;
                    }
                    match policy {
                        ConflictPolicy::Fail => {
                            return Err(anyhow!("remote file already exists: {name}"))
                        }
                        ConflictPolicy::Skip => {
                            stats.skipped += 1;
                            continue;
                        }
                        ConflictPolicy::Overwrite => {
                            self.trash(&item.uuid, "file")?;
                        }
                    }
                }
                self.upload_path(session, remote_parent_uuid, stem, extension, &local)?;
                if options.preserve_timestamps {
                    if let Some(item) = self.wait_for_child(
                        remote_parent_uuid,
                        &format_remote_name(stem, extension),
                        false,
                    )? {
                        if let Ok(timestamp) = local_timestamp(&metadata) {
                            self.set_file_timestamp(&item.uuid, &timestamp)?;
                        }
                    }
                }
                stats.files += 1;
                stats.bytes += metadata.len();
                report_progress(options, &local, stats, 0);
            }
        }
        self.clear_listing_cache();
        Ok(())
    }

    /// Recursively download a remote folder into a local directory.
    pub fn download_directory(
        &self,
        session: &InternxtSession,
        remote_folder_uuid: &str,
        local_root: &Path,
        policy: ConflictPolicy,
    ) -> Result<TransferStats> {
        self.download_directory_filtered(
            session,
            remote_folder_uuid,
            local_root,
            policy,
            &TransferFilter::default(),
        )
    }

    pub fn download_directory_filtered(
        &self,
        session: &InternxtSession,
        remote_folder_uuid: &str,
        local_root: &Path,
        policy: ConflictPolicy,
        filter: &TransferFilter,
    ) -> Result<TransferStats> {
        self.download_directory_with_options(
            session,
            remote_folder_uuid,
            local_root,
            policy,
            TransferOptions {
                filter: filter.clone(),
                ..TransferOptions::default()
            },
        )
    }

    pub fn download_directory_with_options(
        &self,
        session: &InternxtSession,
        remote_folder_uuid: &str,
        local_root: &Path,
        policy: ConflictPolicy,
        options: TransferOptions,
    ) -> Result<TransferStats> {
        check_cancelled(&options)?;
        fs::create_dir_all(local_root)?;
        let mut stats = TransferStats::default();
        self.download_directory_contents(
            session,
            remote_folder_uuid,
            local_root,
            policy,
            &options,
            &mut stats,
        )?;
        Ok(stats)
    }

    fn download_directory_contents(
        &self,
        session: &InternxtSession,
        remote_folder_uuid: &str,
        local_root: &Path,
        policy: ConflictPolicy,
        options: &TransferOptions,
        stats: &mut TransferStats,
    ) -> Result<()> {
        check_cancelled(options)?;
        for item in self.list_folder_cached(remote_folder_uuid)? {
            check_cancelled(options)?;
            let local = local_root.join(sanitize_filename(&item.name));
            if item.is_dir {
                if local.exists() {
                    if policy == ConflictPolicy::Fail {
                        return Err(anyhow!("local folder already exists: {}", local.display()));
                    }
                    if policy == ConflictPolicy::Skip {
                        stats.skipped += 1;
                        continue;
                    }
                } else {
                    fs::create_dir_all(&local)?;
                }
                stats.folders += 1;
                self.download_directory_contents(
                    session, &item.uuid, &local, policy, options, stats,
                )?;
            } else {
                if !options.filter.accepts(&item.name) {
                    stats.skipped += 1;
                    continue;
                }
                if local.exists() {
                    match policy {
                        ConflictPolicy::Fail => {
                            return Err(anyhow!("local file already exists: {}", local.display()))
                        }
                        ConflictPolicy::Skip => {
                            stats.skipped += 1;
                            continue;
                        }
                        ConflictPolicy::Overwrite => {}
                    }
                }
                let state_path = local.with_extension("crispsorter-download.json");
                self.download_file_to_path_resumable(session, &item.uuid, &local, &state_path)?;
                if options.preserve_timestamps {
                    if let Some(timestamp) = item.modified_at.as_deref() {
                        set_local_timestamp(&local, timestamp)?;
                    }
                }
                stats.files += 1;
                stats.bytes += item.size;
                report_progress(options, &local, stats, 0);
            }
        }
        Ok(())
    }

    fn existing_child(
        &self,
        parent_uuid: &str,
        name: &str,
        is_dir: bool,
    ) -> Result<Option<NativeItem>> {
        Ok(self
            .list_folder_cached(parent_uuid)?
            .into_iter()
            .find(|item| item.is_dir == is_dir && item.name == name))
    }

    fn wait_for_child(
        &self,
        parent_uuid: &str,
        name: &str,
        is_dir: bool,
    ) -> Result<Option<NativeItem>> {
        for attempt in 0..10 {
            self.clear_listing_cache();
            if let Some(item) = self.existing_child(parent_uuid, name, is_dir)? {
                return Ok(Some(item));
            }
            if attempt < 9 {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        Ok(None)
    }
}

pub fn sanitize_filename(filename: &str) -> String {
    let mut sanitized = filename
        .chars()
        .map(|value| match value {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            value => value,
        })
        .collect::<String>();
    sanitized = sanitized.trim_matches([' ', '.']).to_owned();
    if sanitized.is_empty() {
        "unnamed_file".to_owned()
    } else {
        sanitized
    }
}

fn transfer_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("OS randomness unavailable");
    hex::encode(bytes)
}

fn wildcard_matches(value: &str, pattern: &str, case_sensitive: bool) -> bool {
    let value = if case_sensitive {
        value.to_owned()
    } else {
        value.to_ascii_lowercase()
    };
    let pattern = if case_sensitive {
        pattern.to_owned()
    } else {
        pattern.to_ascii_lowercase()
    };
    let value = value.as_bytes();
    let pattern = pattern.as_bytes();
    let mut previous = vec![false; pattern.len() + 1];
    previous[0] = true;
    for index in 0..pattern.len() {
        if pattern[index] == b'*' {
            previous[index + 1] = previous[index];
        }
    }
    for &character in value {
        let mut current = vec![false; pattern.len() + 1];
        for (index, &token) in pattern.iter().enumerate() {
            current[index + 1] = match token {
                b'*' => current[index] || previous[index + 1],
                b'?' => previous[index],
                literal => previous[index] && literal == character,
            };
        }
        previous = current;
    }
    previous[pattern.len()]
}

fn local_timestamp(metadata: &fs::Metadata) -> Result<String> {
    let modified = metadata
        .modified()
        .context("reading local modification time")?;
    Ok(chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339())
}

fn set_local_timestamp(path: &Path, timestamp: &str) -> Result<()> {
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp)
        .with_context(|| format!("parsing remote modification time {timestamp}"))?;
    let seconds = parsed.timestamp();
    let nanos = parsed.timestamp_subsec_nanos();
    if seconds < 0 {
        return Err(anyhow!("remote modification time is before the Unix epoch"));
    }
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(seconds, nanos))
        .with_context(|| format!("setting modification time on {}", path.display()))
}

fn split_remote_name(name: &str) -> (&str, &str) {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, extension),
        _ => (name, "file"),
    }
}

fn format_remote_name(stem: &str, extension: &str) -> String {
    if extension.is_empty() {
        stem.to_owned()
    } else {
        format!("{stem}.{extension}")
    }
}

#[cfg(test)]
fn multipart_part_count(size: usize) -> usize {
    multipart_part_count_with_config(size, &TransferConfig::default())
}

fn multipart_part_count_with_config(size: usize, config: &TransferConfig) -> usize {
    if size as u64 >= config.multipart_min_size {
        size.div_ceil(config.part_size)
    } else {
        1
    }
}

fn shard_hash(encrypted: &[u8]) -> String {
    let sha = sha2::Sha256::digest(encrypted);
    hex::encode(<ripemd::Ripemd160 as RipemdDigest>::digest(sha))
}

fn is_expired_token_error(error: &anyhow::Error) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    text.contains(" 401")
        || text.contains("http 401")
        || text.contains("unauthorized")
        || text.contains("token expired")
}

/// Return the lowercase SHA-256 digest commonly used by callers to verify
/// plaintext before or after a transfer.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Verify a plaintext SHA-256 digest and return a descriptive error on a
/// mismatch. Provider shard hashes remain an internal wire concern.
pub fn verify_sha256(data: &[u8], expected_hex: &str) -> Result<()> {
    let actual = sha256_hex(data);
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(expected_hex.trim()),
        "Internxt content hash mismatch: expected {}, got {}",
        expected_hex,
        actual
    );
    Ok(())
}

fn item_name(item: &serde_json::Value, kind: &str) -> String {
    let plain_name = item
        .get("plainName")
        .or_else(|| item.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    let file_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if kind == "files" && !file_type.is_empty() {
        format!("{plain_name}.{file_type}")
    } else {
        plain_name.to_owned()
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

    fn test_nonce() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

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
    fn transfer_config_defaults_are_serial_and_validates_bounds() {
        let config = TransferConfig::default();
        assert_eq!(config.download_workers, 1);
        assert_eq!(multipart_part_count_with_config(100, &config), 1);
        assert!(config.validate().is_ok());
        let mut invalid = config;
        invalid.part_size = 15;
        assert!(invalid.validate().is_err());
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
    fn active_token_prefers_refreshed_credential() {
        let mut session = InternxtSession {
            drive_api_url: String::new(),
            network_url: String::new(),
            email: String::new(),
            token: "old-token".into(),
            new_token: "refreshed-token".into(),
            mnemonic: String::new(),
            user_id: String::new(),
            root_folder_id: String::new(),
            bridge_user: String::new(),
            bucket_id: String::new(),
        };
        assert_eq!(session.active_token(), "refreshed-token");
        session.new_token.clear();
        assert_eq!(session.active_token(), "old-token");
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
    fn multipart_layout_matches_internxt_threshold() {
        assert_eq!(multipart_part_count(MULTIPART_MIN_SIZE - 1), 1);
        assert_eq!(multipart_part_count(MULTIPART_MIN_SIZE), 4);
        assert_eq!(multipart_part_count(MULTIPART_MIN_SIZE + 1), 4);
        assert_eq!(multipart_part_count(4 * UPLOAD_PART_SIZE), 4);
    }

    #[test]
    fn shard_hash_is_ripemd160_of_sha256() {
        assert_eq!(
            shard_hash(b"internxt shard"),
            "30b546838b1dfabb91afdde3cf09661657aee40d"
        );
    }

    #[test]
    fn plaintext_hash_verification_is_case_insensitive() {
        let digest = sha256_hex(b"verified content");
        verify_sha256(b"verified content", &digest.to_uppercase()).unwrap();
        assert!(verify_sha256(b"tampered content", &digest)
            .unwrap_err()
            .to_string()
            .contains("content hash mismatch"));
    }

    #[test]
    fn single_transfer_progress_wrappers_report_bytes() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let copy = Arc::clone(&seen);
        let callback: ProgressCallback = Arc::new(move |progress| {
            copy.lock().unwrap().push(progress);
        });
        let mut reader = ProgressReader::new(
            std::io::Cursor::new(b"reader bytes".to_vec()),
            "upload.bin",
            12,
            Arc::clone(&callback),
        );
        let mut buffer = Vec::new();
        reader.read_to_end(&mut buffer).unwrap();
        let mut writer = ProgressWriter::new(Vec::new(), "download.bin", 14, Arc::clone(&callback));
        writer.write_all(b"download bytes").unwrap();
        let progress = seen.lock().unwrap();
        assert_eq!(progress.first().unwrap().completed_bytes, 12);
        assert_eq!(progress.last().unwrap().completed_bytes, 14);
        assert_eq!(progress.last().unwrap().total_bytes, 14);
        assert_eq!(progress.last().unwrap().files_completed, 1);
    }

    #[test]
    fn listing_composes_file_extension_for_path_resolution() {
        let file = serde_json::json!({"plainName": "report", "type": "pdf"});
        let folder = serde_json::json!({"plainName": "Documents"});
        assert_eq!(item_name(&file, "files"), "report.pdf");
        assert_eq!(item_name(&folder, "folders"), "Documents");
    }

    #[test]
    fn filename_sanitization_matches_reference_safety_rules() {
        assert_eq!(sanitize_filename(" report?.txt "), "report_.txt");
        assert_eq!(sanitize_filename("..."), "unnamed_file");
        assert_eq!(sanitize_filename("nested/name"), "nested_name");
    }

    #[test]
    fn wildcard_matching_supports_stars_questions_and_case_folding() {
        assert!(wildcard_matches("Report-2026.pdf", "Report-????.pdf", true));
        assert!(wildcard_matches("Report-2026.pdf", "report-*.PDF", false));
        assert!(!wildcard_matches(
            "Report-2026.pdf",
            "report-????.txt",
            false
        ));
        assert!(wildcard_matches("abc", "a*c", true));
        assert!(wildcard_matches("abc", "*", true));
        assert!(wildcard_matches("abc", "***", true));
    }

    #[test]
    fn transfer_filter_applies_includes_and_excludes() {
        let empty = TransferFilter::default();
        assert!(empty.accepts("photo.JPG"));

        let include = TransferFilter {
            includes: vec!["*.jpg".into(), "*.png".into()],
            excludes: vec![],
        };
        assert!(include.accepts("photo.JPG"));
        assert!(!include.accepts("notes.txt"));

        let exclude = TransferFilter {
            includes: vec!["*".into()],
            excludes: vec!["*.tmp".into(), "secret*".into()],
        };
        assert!(exclude.accepts("photo.jpg"));
        assert!(!exclude.accepts("cache.tmp"));
        assert!(!exclude.accepts("secret-notes.txt"));
    }

    #[test]
    fn local_tree_inspection_counts_bytes_and_rejects_non_directories() {
        let root = std::env::temp_dir().join(format!("crispsorter-inspect-{}", now_seconds()));
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("a.txt"), b"1234").unwrap();
        fs::write(root.join("nested").join("b.txt"), b"12").unwrap();
        let stats = inspect_local_directory(&root).unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.folders, 1);
        assert_eq!(stats.bytes, 6);
        assert!(inspect_local_directory(&root.join("a.txt")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recursive_transfer_honors_cooperative_cancellation_before_network_io() {
        let root = std::env::temp_dir().join(format!("crispsorter-cancel-{}", now_seconds()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file.txt"), b"payload").unwrap();
        let cancellation = Arc::new(AtomicBool::new(true));
        let client = InternxtNativeClient::new("http://127.0.0.1:1", "token").unwrap();
        let session = InternxtSession {
            drive_api_url: String::new(),
            network_url: String::new(),
            email: String::new(),
            token: String::new(),
            new_token: String::new(),
            mnemonic: MNEMONIC.into(),
            user_id: String::new(),
            root_folder_id: String::new(),
            bridge_user: String::new(),
            bucket_id: "00".repeat(12),
        };
        let result = client.upload_directory_with_options(
            &session,
            &root,
            "remote-root",
            ConflictPolicy::Overwrite,
            TransferOptions {
                cancellation: Some(cancellation),
                ..TransferOptions::default()
            },
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn progress_callback_receives_completed_transfer_stats() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let copy = Arc::clone(&seen);
        let options = TransferOptions {
            progress: Some(Arc::new(move |progress| {
                copy.lock().unwrap().push(progress);
            })),
            ..TransferOptions::default()
        };
        let stats = TransferStats {
            files: 1,
            bytes: 42,
            ..TransferStats::default()
        };
        report_progress(&options, Path::new("file.txt"), &stats, 42);
        let progress = seen.lock().unwrap().pop().unwrap();
        assert_eq!(progress.path, PathBuf::from("file.txt"));
        assert_eq!(progress.completed_bytes, 42);
        assert_eq!(progress.total_bytes, 42);
        assert_eq!(progress.files_completed, 1);
    }

    #[test]
    fn ctr_at_and_streaming_reader_match_whole_file_encryption() {
        let plaintext = (0..131_071).map(|value| value as u8).collect::<Vec<_>>();
        let (index, expected) = encrypt(&plaintext, MNEMONIC, &BUCKET);
        let hash = Arc::new(Mutex::new(Sha256::new()));
        let key = file_key(MNEMONIC, &BUCKET, &index);
        let mut reader = EncryptReader {
            reader: std::io::Cursor::new(plaintext.clone()),
            cipher: Aes256Ctr::new((&key).into(), (&index[..16]).into()),
            remaining: plaintext.len() as u64,
            hash: Arc::clone(&hash),
        };
        let mut streamed = Vec::new();
        reader.read_to_end(&mut streamed).unwrap();
        assert_eq!(streamed, expected);
        assert_eq!(
            hex::encode(hash.lock().unwrap().clone().finalize()),
            hex::encode(Sha256::digest(&expected))
        );

        let mut split = plaintext[16..16 + 4096].to_vec();
        crypt_at(&mut split, MNEMONIC, &BUCKET, &index, 16).unwrap();
        assert_eq!(split, expected[16..16 + 4096]);
    }

    #[test]
    fn upload_checkpoint_round_trips_and_is_atomic_shape() {
        let path =
            std::env::temp_dir().join(format!("crispsorter-checkpoint-test-{}", now_seconds()));
        let checkpoint = checkpoint_path(&path, "00".repeat(12).as_str());
        let value = UploadCheckpoint {
            version: 1,
            path: path.to_string_lossy().into_owned(),
            bucket_id: "00".repeat(12),
            file_size: 123,
            modified_ns: 456,
            part_size: UPLOAD_PART_SIZE,
            parts: 1,
            index: "11".repeat(32),
            uuid: "uuid".to_owned(),
            upload_id: "upload".to_owned(),
            urls: vec!["https://part".to_owned()],
            etags: vec![Some("etag".to_owned())],
            created: now_seconds(),
        };
        save_checkpoint(&checkpoint, &value).unwrap();
        assert_eq!(
            load_checkpoint(&checkpoint).unwrap().unwrap().index,
            value.index
        );
        assert!(!checkpoint.with_extension("tmp").exists());
        remove_checkpoint(&checkpoint);
    }

    #[test]
    fn download_checkpoint_round_trips_completed_ranges() {
        let path = std::env::temp_dir().join(format!(
            "crispsorter-download-checkpoint-test-{}.json",
            test_nonce()
        ));
        let value = DownloadResumeState {
            version: 1,
            path: "/tmp/result.bin".into(),
            file_uuid: "file-uuid".into(),
            file_size: 128,
            part_size: 64,
            parts: 2,
            completed_parts: vec![0],
            created: now_seconds(),
        };
        save_download_checkpoint(&path, &value).unwrap();
        assert_eq!(load_download_checkpoint(&path).unwrap(), Some(value));
        assert!(!path.with_extension("tmp").exists());
        remove_checkpoint(&path);
    }

    #[test]
    fn download_checkpoint_round_trips_completed_parts() {
        let path = std::env::temp_dir().join(format!(
            "crispsorter-download-checkpoint-test-{}.json",
            test_nonce()
        ));
        let value = DownloadResumeState {
            version: 1,
            path: "/data/example.bin".into(),
            file_uuid: "file-uuid".into(),
            file_size: 64 * 1024 * 1024,
            part_size: DOWNLOAD_PART_SIZE,
            parts: 3,
            completed_parts: vec![0, 2],
            created: now_seconds(),
        };
        save_download_checkpoint(&path, &value).unwrap();
        assert_eq!(load_download_checkpoint(&path).unwrap(), Some(value));
        remove_checkpoint(&path);
    }

    #[test]
    fn public_resume_state_api_round_trips_explicit_state_files() {
        let client = InternxtNativeClient::new("http://127.0.0.1:1", "token").unwrap();
        let path = std::env::temp_dir().join(format!(
            "crispsorter-explicit-resume-state-{}.json",
            now_seconds()
        ));
        let state = UploadResumeState {
            version: 1,
            path: "/data/example.bin".into(),
            bucket_id: "00".repeat(12),
            file_size: 32 * 1024 * 1024,
            modified_ns: 123,
            part_size: UPLOAD_PART_SIZE,
            parts: 3,
            index: "11".repeat(32),
            uuid: "shard-uuid".into(),
            upload_id: "multipart-upload-id".into(),
            urls: vec![
                "https://one".into(),
                "https://two".into(),
                "https://three".into(),
            ],
            etags: vec![Some("etag-1".into()), None, Some("etag-3".into())],
            created: now_seconds(),
        };
        client.save_upload_resume_state(&path, &state).unwrap();
        assert_eq!(client.load_upload_resume_state(&path).unwrap(), Some(state));
        client.clear_upload_resume_state(&path);
        assert_eq!(client.load_upload_resume_state(&path).unwrap(), None);
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
