//! Pure Internxt file crypto used by the native drive client (P33.1).
//!
//! The formulas mirror the protocol implementation in Internxt's clients:
//! BIP-39 seed derivation, SHA-512 key derivation, and AES-256-CTR with the
//! first 16 bytes of the random file index as the IV.  This module deliberately
//! has no HTTP or credential state, which makes the wire-critical part easy to
//! test before the authenticated drive wrapper is added.

use aes::Aes256;
use cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use sha2::{Digest, Sha512};

type Aes256Ctr = Ctr128BE<Aes256>;

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
}
