//! Crypto primitives ported 1:1 from og/cli crypto.service.ts, og/lib aes,
//! and og/inxt-js crypto utils. Byte-for-byte compatible with the node CLI.

use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit, StreamCipher, StreamCipherSeek};
use aes_gcm::aead::consts::U16;
use aes_gcm::aead::Aead;
use aes_gcm::{AesGcm, KeyInit, Nonce};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use md5::Md5;
use rand::RngExt;
use ripemd::Ripemd160;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::config;

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type Aes256Ctr = ctr::Ctr128BE<aes::Aes256>;
/// AES-256-GCM with a 16-byte IV (node `@internxt/lib` uses a non-96-bit IV,
/// so GCM derives the counter via GHASH rather than the 96-bit fast path).
type Aes256GcmIv16 = AesGcm<aes_gcm::aes::Aes256, U16>;

/// pbkdf2(password, salt, 10000, 32, sha1) -> (salt_hex, hash_hex)
pub fn pass_to_hash(password: &str, salt_hex: Option<&str>) -> Result<(String, String)> {
    let salt = match salt_hex {
        Some(s) => hex::decode(s)?,
        None => {
            let mut b = [0u8; 16];
            rand::rng().fill(&mut b);
            b.to_vec()
        }
    };
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha1>(password.as_bytes(), &salt, 10000, &mut out);
    Ok((hex::encode(&salt), hex::encode(out)))
}

/// OpenSSL EVP_BytesToKey (MD5, 1 iteration) used by CryptoJS for AES-256-CBC.
fn key_and_iv(secret: &str, salt: &[u8]) -> ([u8; 32], [u8; 16]) {
    let mut password = secret.as_bytes().to_vec();
    password.extend_from_slice(salt);

    let mut md5_hashes: Vec<[u8; 16]> = Vec::with_capacity(3);
    let mut digest = password.clone();
    for _ in 0..3 {
        let h: [u8; 16] = Md5::digest(&digest).into();
        md5_hashes.push(h);
        digest = h.to_vec();
        digest.extend_from_slice(&password);
    }

    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&md5_hashes[0]);
    key[16..].copy_from_slice(&md5_hashes[1]);
    (key, md5_hashes[2])
}

/// CryptoJS-compatible AES-256-CBC encrypt. Output hex of: "Salted__" + salt(8) + ciphertext.
pub fn encrypt_text_with_key(text: &str, secret: &str) -> String {
    let mut salt = [0u8; 8];
    rand::rng().fill(&mut salt);
    let (key, iv) = key_and_iv(secret, &salt);

    let ct = Aes256CbcEnc::new_from_slices(&key, &iv)
        .expect("valid AES-256-CBC key/iv length")
        .encrypt_padded_vec::<cbc::cipher::block_padding::Pkcs7>(text.as_bytes());

    let mut out = b"Salted__".to_vec();
    out.extend_from_slice(&salt);
    out.extend_from_slice(&ct);
    hex::encode(out)
}

/// CryptoJS-compatible AES-256-CBC decrypt of hex string.
pub fn decrypt_text_with_key(encrypted_hex: &str, secret: &str) -> Result<String> {
    let data = hex::decode(encrypted_hex)?;
    if data.len() < 16 {
        return Err(anyhow!("ciphertext too short"));
    }
    let salt = &data[8..16];
    let (key, iv) = key_and_iv(secret, salt);
    let pt = Aes256CbcDec::new_from_slices(&key, &iv)
        .expect("valid AES-256-CBC key/iv length")
        .decrypt_padded_vec::<cbc::cipher::block_padding::Pkcs7>(&data[16..])
        .map_err(|e| anyhow!("cbc decrypt failed: {e}"))?;
    Ok(String::from_utf8(pt)?)
}

pub fn encrypt_text(text: &str) -> String {
    encrypt_text_with_key(text, &config::app_crypto_secret())
}

pub fn decrypt_text(encrypted_hex: &str) -> Result<String> {
    decrypt_text_with_key(encrypted_hex, &config::app_crypto_secret())
}

/// og/lib aes.decrypt: base64 [salt64][iv16][tag16][ciphertext], pbkdf2-sha512 2145 rounds, AES-256-GCM.
pub fn decrypt_private_key(private_key_b64: &str, password: &str) -> Result<String> {
    const MIN_LEN: usize = 129;
    if private_key_b64.len() <= MIN_LEN {
        return Ok(String::new());
    }
    let data = B64.decode(private_key_b64)?;
    if data.len() < 96 {
        return Err(anyhow!("private key too short"));
    }
    let salt = &data[0..64];
    let iv = &data[64..80];
    let tag = &data[80..96];
    let ct = &data[96..];

    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha512>(password.as_bytes(), salt, 2145, &mut key);

    let cipher = Aes256GcmIv16::new_from_slice(&key).map_err(|e| anyhow!("gcm key: {e}"))?;
    let mut buf = ct.to_vec();
    buf.extend_from_slice(tag);
    let nonce = Nonce::<U16>::try_from(iv).map_err(|_| anyhow!("invalid iv length"))?;
    let pt = cipher
        .decrypt(&nonce, buf.as_ref())
        .map_err(|_| anyhow!("Private key is corrupted"))?;
    Ok(String::from_utf8(pt)?)
}

/// 'HybridMode' in base64 — prefix marking a post-quantum (ecc+kyber) ciphertext.
const HYBRID_MODE_PREFIX: &str = "SHlicmlkTW9kZQ==";

/// Raw Kyber512 secret key length (Dashlane PQClean).
const KYBER512_SK_LEN: usize = 1632;

/// Kyber512 KEM decapsulation. `ct`/`sk` are the raw Dashlane PQClean bytes
/// (ciphertext 768B, secret key 1632B); returns the 32-byte shared secret.
pub fn kyber512_decapsulate(ct: &[u8], sk: &[u8]) -> Result<[u8; 32]> {
    safe_pqc_kyber::decapsulate(ct, sk).map_err(|e| anyhow!("kyber decapsulate failed: {e:?}"))
}

/// Coerce a stored Kyber private key to the raw 1632-byte secret key.
/// Accepts either base64(raw) (our clean form) or the node CLI's literal
/// `base64(utf8(base64(raw)))` form, decoding twice when needed.
fn kyber_secret_key_raw(stored_b64: &str) -> Result<Vec<u8>> {
    let once = B64.decode(stored_b64)?;
    if once.len() == KYBER512_SK_LEN {
        return Ok(once);
    }
    if let Ok(twice) = B64.decode(&once) {
        if twice.len() == KYBER512_SK_LEN {
            return Ok(twice);
        }
    }
    Err(anyhow!(
        "kyber secret key has unexpected length {} (want {KYBER512_SK_LEN})",
        once.len()
    ))
}

/// blake3 XOF: extend `secret` to `out_bytes` bytes, returned as a hex string.
/// Mirrors node CryptoUtils.extendSecret (hash-wasm `blake3(secret, bits)`).
pub fn blake3_extend(secret: &[u8], out_bytes: usize) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(secret);
    let mut out = vec![0u8; out_bytes];
    hasher.finalize_xof().fill(&mut out);
    hex::encode(out)
}

/// XOR two equal-length hex strings, returning a hex string (CryptoUtils.XORhex).
fn xor_hex(a: &str, b: &str) -> Result<String> {
    if a.len() != b.len() {
        return Err(anyhow!("Can XOR only strings with identical length"));
    }
    let ab = hex::decode(a)?;
    let bb = hex::decode(b)?;
    let xored: Vec<u8> = ab.iter().zip(bb.iter()).map(|(x, y)| x ^ y).collect();
    Ok(hex::encode(xored))
}

/// Decrypt an armored OpenPGP message with an armored (unprotected) private key.
/// The private key is already password-decrypted, so no passphrase is needed.
fn pgp_decrypt(armored_message: &str, armored_private_key: &str) -> Result<String> {
    use pgp::composed::{Deserializable, Message, SignedSecretKey};
    use pgp::types::Password;
    use std::io::Read;

    let (secret_key, _) = SignedSecretKey::from_string(armored_private_key)
        .map_err(|e| anyhow!("invalid pgp private key: {e}"))?;
    let (message, _) = Message::from_string(armored_message)
        .map_err(|e| anyhow!("invalid pgp message: {e}"))?;

    let mut decrypted = message
        .decrypt(&Password::empty(), &secret_key)
        .map_err(|e| anyhow!("pgp decrypt failed: {e}"))?;
    while decrypted.is_compressed() {
        decrypted = decrypted
            .decompress()
            .map_err(|e| anyhow!("pgp decompress failed: {e}"))?;
    }
    let mut out = String::new();
    decrypted
        .read_to_string(&mut out)
        .map_err(|e| anyhow!("pgp read failed: {e}"))?;
    Ok(out)
}

/// Decrypts an Internxt workspace mnemonic blob (`workspaceUser.key`).
/// Mirrors og/cli keys.service hybridDecryptMessageWithPrivateKey: ecc-only when
/// the blob is a single base64 PGP message, hybrid (ecc+kyber) when it is
/// `HybridMode$kyberCt$eccCt`. `ecc_private_key_b64`/`kyber_private_key_b64` are
/// the (already password-decrypted) keys, base64-encoded as stored at login.
pub fn decrypt_workspace_key(
    blob: &str,
    ecc_private_key_b64: &str,
    kyber_private_key_b64: Option<&str>,
) -> Result<String> {
    let ecc_armored = String::from_utf8(B64.decode(ecc_private_key_b64)?)?;
    let parts: Vec<&str> = blob.split('$').collect();

    if parts.first() == Some(&HYBRID_MODE_PREFIX) {
        if parts.len() != 3 {
            return Err(anyhow!("malformed hybrid workspace key"));
        }
        let kyber_b64 = kyber_private_key_b64
            .ok_or_else(|| anyhow!("hybrid workspace key requires a Kyber private key"))?;
        let kyber_ct = B64.decode(parts[1])?;
        let kyber_sk = kyber_secret_key_raw(kyber_b64)?;
        let kyber_secret = kyber512_decapsulate(&kyber_ct, &kyber_sk)?;

        let ecc_message = String::from_utf8(B64.decode(parts[2])?)?;
        let ecc_plaintext = pgp_decrypt(&ecc_message, &ecc_armored)?;

        // ecc_plaintext is a hex string; XOR it with the blake3-extended kyber secret.
        let secret_hex = blake3_extend(&kyber_secret, ecc_plaintext.len() / 2);
        let xored = xor_hex(&ecc_plaintext, &secret_hex)?;
        Ok(String::from_utf8(hex::decode(xored)?)?)
    } else {
        let ecc_message = String::from_utf8(B64.decode(blob)?)?;
        pgp_decrypt(&ecc_message, &ecc_armored)
    }
}

pub fn sha256(input: &[u8]) -> Vec<u8> {
    Sha256::digest(input).to_vec()
}

pub fn ripemd160(input: &[u8]) -> Vec<u8> {
    Ripemd160::digest(input).to_vec()
}

/// Network basic-auth password = sha256(userId).hex
pub fn network_password(user_id: &str) -> String {
    hex::encode(sha256(user_id.as_bytes()))
}

fn mnemonic_to_seed(mnemonic: &str) -> Result<[u8; 64]> {
    let m = bip39::Mnemonic::parse_normalized(mnemonic.trim())
        .map_err(|e| anyhow!("invalid mnemonic: {e}"))?;
    Ok(m.to_seed(""))
}

pub fn validate_mnemonic(mnemonic: &str) -> bool {
    bip39::Mnemonic::parse_normalized(mnemonic.trim()).is_ok()
}

/// GenerateFileKey(mnemonic, bucketId, index) -> 32-byte AES key.
pub fn generate_file_key(mnemonic: &str, bucket_id: &str, index: &[u8]) -> Result<[u8; 32]> {
    let seed = mnemonic_to_seed(mnemonic)?;
    let bucket_id_bytes = hex::decode(bucket_id)?;

    // bucketKey = sha512(seed || bucketIdBytes)
    let mut h = Sha512::new();
    h.update(seed);
    h.update(&bucket_id_bytes);
    let bucket_key = h.finalize();

    // fileKey = sha512(bucketKey[0..32] || index)[0..32]
    let mut h2 = Sha512::new();
    h2.update(&bucket_key[0..32]);
    h2.update(index);
    let file_key = h2.finalize();

    let mut key = [0u8; 32];
    key.copy_from_slice(&file_key[0..32]);
    Ok(key)
}

/// In-place AES-256-CTR (encrypt == decrypt). iv must be 16 bytes.
pub fn aes256ctr_apply(key: &[u8; 32], iv: &[u8], data: &mut [u8]) {
    let mut cipher =
        Aes256Ctr::new_from_slices(key, iv).expect("valid AES-256-CTR key/iv length");
    cipher.apply_keystream(data);
}

/// Generate a 6-digit TOTP code from a base32 secret (RFC 6238, SHA-1, 30s
/// period). Mirrors otpauth's `new OTPAuth.TOTP({ secret, digits: 6 })` used by
/// the node CLI's `--twofactortoken` flag.
pub fn totp_now(secret: &str) -> Result<String> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow!("system clock before unix epoch: {e}"))?
        .as_secs();
    totp_at(secret, timestamp, 30, 6)
}

fn totp_at(secret: &str, timestamp: u64, period: u64, digits: u32) -> Result<String> {
    use hmac::digest::KeyInit as HmacKeyInit;
    use hmac::{Mac, SimpleHmac};
    // otpauth normalizes the secret: uppercase and drop non-base32 chars.
    let cleaned: String = secret
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    let key = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &cleaned)
        .ok_or_else(|| anyhow!("invalid base32 TOTP secret"))?;
    if key.is_empty() {
        return Err(anyhow!("invalid base32 TOTP secret"));
    }

    let counter = timestamp / period;
    let mut mac = <SimpleHmac<Sha1> as HmacKeyInit>::new_from_slice(&key)
        .map_err(|e| anyhow!("totp hmac key: {e}"))?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();

    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let bin = (u32::from(digest[offset] & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    let code = bin % 10u32.pow(digits);
    Ok(format!("{code:0width$}", width = digits as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "6KYQBP847D4ATSFA";
    const MNEMONIC: &str =
        "legal winner thank year wave sausage worth useful legal winner thank yellow";
    const BUCKET: &str = "0123456789abcdef0123456789abcdef";
    const INDEX_HEX: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    // Values produced by scripts/ref.js (node, identical algorithm).
    #[test]
    fn matches_node_decrypt() {
        let enc = "53616c7465645f5f00112233445566774556e47b00ca7ba4959bea3b6abdf0be";
        assert_eq!(decrypt_text_with_key(enc, SECRET).unwrap(), "hello world");
    }

    // RFC 6238 Appendix B test vector (SHA-1, secret "12345678901234567890").
    #[test]
    fn matches_rfc6238_totp() {
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        assert_eq!(totp_at(secret, 59, 30, 8).unwrap(), "94287082");
        assert_eq!(totp_at(secret, 1111111109, 30, 8).unwrap(), "07081804");
        // 6-digit truncation (what the CLI actually uses).
        assert_eq!(totp_at(secret, 59, 30, 6).unwrap(), "287082");
    }

    #[test]
    fn matches_node_pass_hash() {
        let (_, hash) = pass_to_hash("mypassword", Some("deadbeef")).unwrap();
        assert_eq!(
            hash,
            "c949a136c21c1b44b76e0c1d7e7f7178b7beb595d4fc18add5e4c6d01f980306"
        );
    }

    #[test]
    fn matches_node_file_key() {
        let index = hex::decode(INDEX_HEX).unwrap();
        let key = generate_file_key(MNEMONIC, BUCKET, &index).unwrap();
        assert_eq!(
            hex::encode(key),
            "ef27f0d3bbfe4b6d013c890b102af2df8c7cf3dcebe5683c54c71647f426e8cb"
        );
    }

    #[test]
    fn matches_node_ctr() {
        let index = hex::decode(INDEX_HEX).unwrap();
        let key = generate_file_key(MNEMONIC, BUCKET, &index).unwrap();
        let mut data = b"the quick brown fox".to_vec();
        aes256ctr_apply(&key, &index[0..16], &mut data);
        assert_eq!(hex::encode(&data), "9373448f118b254274c2ad6d5eccd424afc228");
    }

    #[test]
    fn matches_node_shard_hash() {
        let h = ripemd160(&sha256(b"encrypted-shard-content"));
        assert_eq!(hex::encode(h), "e1c941ff3e3e9f79d932941f88e8d3c833bc8e5d");
    }

    #[test]
    fn cbc_roundtrip() {
        let enc = encrypt_text_with_key("round trip me", SECRET);
        assert_eq!(decrypt_text_with_key(&enc, SECRET).unwrap(), "round trip me");
    }

    /// Ctr::seek must reproduce the exact keystream position of a continuous
    /// decrypt — this is what lets a range download skip the prefix. Encrypt a
    /// buffer as one stream, then for a spread of offsets (incl. mid-AES-block,
    /// which exercises the sub-block skip) decrypt from that offset and compare
    /// to the plaintext tail.
    #[test]
    fn ctr_seek_matches_continuous() {
        let index = hex::decode(INDEX_HEX).unwrap();
        let iv = &index[0..16];
        let key = generate_file_key(MNEMONIC, BUCKET, &index).unwrap();

        let plain: Vec<u8> = (0..5000u32).map(|i| (i * 31 + 7) as u8).collect();
        let mut cipher = plain.clone();
        Ctr::new(&key, iv).apply(&mut cipher); // continuous encrypt

        // 0 and 16 = block-aligned; 1/15/17/1234/4999 = mid-block.
        for &off in &[0usize, 1, 15, 16, 17, 1234, 4999] {
            let mut window = cipher[off..].to_vec();
            let mut ctr = Ctr::new(&key, iv);
            ctr.seek(off as u64);
            ctr.apply(&mut window);
            assert_eq!(window, plain[off..], "seek mismatch at offset {off}");
        }
    }
}

/// Streaming AES-256-CTR state for chunked encrypt/decrypt.
pub struct Ctr(Aes256Ctr);

impl Ctr {
    pub fn new(key: &[u8; 32], iv: &[u8]) -> Self {
        Ctr(Aes256Ctr::new_from_slices(key, iv).expect("valid AES-256-CTR key/iv length"))
    }
    pub fn apply(&mut self, data: &mut [u8]) {
        self.0.apply_keystream(data);
    }
    /// Seek the keystream to byte `pos` (CTR is seekable: block = pos/16,
    /// counter = iv + block). The next `apply` decrypts as if `pos` bytes had
    /// already been processed — lets a range download skip the prefix entirely.
    pub fn seek(&mut self, pos: u64) {
        self.0.seek(pos);
    }
}

