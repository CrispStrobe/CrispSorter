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
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha512};

type Aes256Ctr = Ctr128BE<Aes256>;
type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;

const OPENSSL_MAGIC: &[u8; 8] = b"Salted__";

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
    getrandom::getrandom(&mut salt).context("generating Internxt crypto salt")?;
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

/// Derive the 64-byte BIP-39 seed for a mnemonic and optional passphrase.
///
/// BIP-39 specifies PBKDF2-HMAC-SHA512 with 2048 rounds and the salt prefix
/// `mnemonic`. The clients pass an empty passphrase for Internxt accounts.
pub fn mnemonic_seed(mnemonic: &str, passphrase: &str) -> [u8; 64] {
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
}
