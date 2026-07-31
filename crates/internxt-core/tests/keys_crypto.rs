//! Cross-checks the workspace key crypto (PGP + Kyber512 + blake3) against
//! reference vectors produced by node (`scripts/ref-keys.js`).
//!
//! Regenerate vectors with:  NODE_PATH=og/node_modules node scripts/ref-keys.js > scripts/ref-keys.json
//! The test is skipped (passes) when the vector file is absent, so `cargo test`
//! works without `og/` fetched. Exercised through the public `internxt_core` API.

use internxt_core::crypto;
use std::path::Path;

fn load_vectors() -> Option<serde_json::Value> {
    // Vectors live at the repo root (../../ from this crate's manifest dir).
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ref-keys.json");
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn s<'a>(v: &'a serde_json::Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_else(|| panic!("missing vector field {k}"))
}

#[test]
fn workspace_key_crypto_matches_node() {
    let Some(v) = load_vectors() else {
        eprintln!("scripts/ref-keys.json absent; skipping workspace key crypto cross-check");
        return;
    };

    let password = s(&v, "password");
    let ecc_b64 = s(&v, "ecc_private_key_b64");
    let kyber_b64 = s(&v, "kyber_private_key_b64");
    let expected = s(&v, "expected_mnemonic");

    // lib AES-GCM private-key decryption -> base64(armored), matching login.
    let decrypted_armored = crypto::decrypt_private_key(s(&v, "ecc_private_key_encrypted"), password)
        .expect("decrypt_private_key");
    use base64::Engine;
    let decrypted_b64 = base64::engine::general_purpose::STANDARD.encode(decrypted_armored.as_bytes());
    assert_eq!(decrypted_b64, ecc_b64, "decrypt_private_key should reproduce stored ecc key");

    // standalone Kyber512 decapsulation.
    let ct = base64::engine::general_purpose::STANDARD
        .decode(s(&v, "kyber_ciphertext_b64"))
        .unwrap();
    let sk = base64::engine::general_purpose::STANDARD.decode(kyber_b64).unwrap();
    let secret = crypto::kyber512_decapsulate(&ct, &sk).expect("decapsulate");
    let secret_b64 = base64::engine::general_purpose::STANDARD.encode(secret);
    assert_eq!(secret_b64, s(&v, "kyber_shared_secret_b64"), "kyber shared secret");

    // blake3 XOF extend.
    let extended = crypto::blake3_extend(&secret, s(&v, "expected_mnemonic").len());
    assert_eq!(extended, s(&v, "secret_hex_blake3"), "blake3 extended secret");

    // ecc-only workspace key.
    let ecc_only = crypto::decrypt_workspace_key(s(&v, "ecc_only_blob"), ecc_b64, None)
        .expect("ecc-only decrypt");
    assert_eq!(ecc_only, expected, "ecc-only workspace mnemonic");

    // hybrid (ecc + kyber) workspace key.
    let hybrid = crypto::decrypt_workspace_key(s(&v, "hybrid_blob"), ecc_b64, Some(kyber_b64))
        .expect("hybrid decrypt");
    assert_eq!(hybrid, expected, "hybrid workspace mnemonic");
}

