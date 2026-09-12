//! An encrypted file under the app data dir — the vault that never asks
//! the OS for anything.
//!
//! One AES-256-GCM envelope holds every secret the app has. Reads and
//! writes decrypt and re-encrypt the whole thing; with a handful of
//! entries that costs microseconds and buys a format with no partial
//! states to reason about.
//!
//! # What the key is, and what that buys
//!
//! [`FileKeySource::Device`] keeps a random 32-byte key in `0600`
//! `secrets.key` beside the envelope. Be clear-eyed about the boundary
//! this draws: any process running as this user can read the key file
//! as easily as the envelope, so it is not protection against local
//! code. What it *is* protection against is everything that moves files
//! without moving the whole directory and its permissions — cloud sync,
//! a Time Machine restore onto a different account, a support tarball,
//! a settings export. Those are the leaks the secret store existed to
//! prevent in the first place, and the plaintext `settings.json` this
//! replaced stopped none of them.
//!
//! [`FileKeySource::Passphrase`] derives the key with Argon2id from
//! something only the user knows. Nothing on disk opens the vault, so a
//! stolen copy of the directory is worthless — at the price of typing
//! the passphrase once per run.
//!
//! # Format
//!
//! `secrets.vault` is JSON so it stays inspectable and greppable for
//! *shape* while telling you nothing about content:
//!
//! ```json
//! { "version": 1, "cipher": "aes-256-gcm", "kdf": "argon2id",
//!   "salt": "…", "mCost": 19456, "tCost": 2, "pCost": 1,
//!   "nonce": "…", "ciphertext": "…" }
//! ```
//!
//! The plaintext inside is a JSON array of `{service, account, value}` —
//! an array rather than a map so no separator has to be reserved out of
//! the service and account namespaces.

use super::{write_private, Error, FileKeySource, Vault};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use zeroize::Zeroizing;

/// Basename of the envelope.
pub const VAULT_FILE: &str = "secrets.vault";
/// Basename of the device key, used only by [`FileKeySource::Device`].
pub const KEY_FILE: &str = "secrets.key";

/// Argon2id cost parameters. OWASP's 2024 baseline for interactive use:
/// 19 MiB, two passes, one lane. Recorded *in* the envelope so raising
/// them later does not orphan existing vaults.
const M_COST: u32 = 19_456;
const T_COST: u32 = 2;
const P_COST: u32 = 1;

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    version: u32,
    cipher: String,
    kdf: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    salt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    m_cost: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    t_cost: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    p_cost: Option<u32>,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Item {
    service: String,
    account: String,
    value: String,
}

pub struct FileVault {
    vault_path: PathBuf,
    key_path: PathBuf,
    source: FileKeySource,
    /// `None` for a passphrase vault that has not been unlocked yet.
    key: Mutex<Option<Zeroizing<[u8; KEY_LEN]>>>,
    /// Serialises the read-modify-write cycle. Two threads setting
    /// different secrets at once would otherwise each write back a
    /// snapshot taken before the other's change.
    io: Mutex<()>,
}

fn random(buf: &mut [u8]) -> Result<(), Error> {
    getrandom::getrandom(buf).map_err(|e| Error::Backend(format!("no system randomness: {e}")))
}

fn derive(passphrase: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<Zeroizing<[u8; KEY_LEN]>, Error> {
    let params = Params::new(m, t, p, Some(KEY_LEN))
        .map_err(|e| Error::Other(format!("bad Argon2 parameters: {e}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut raw = [0u8; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut raw)
        .map_err(|e| Error::Other(format!("key derivation failed: {e}")))?;
    Ok(Zeroizing::new(raw))
}

impl FileVault {
    /// Open (or prepare to open) the vault in `data_dir`.
    ///
    /// A device-keyed vault is usable immediately; a passphrase vault
    /// comes back locked. Construction deliberately does not fail on a
    /// missing passphrase — a locked vault the user can unlock from the
    /// UI is a better state than a startup error.
    pub fn open(data_dir: &Path, source: FileKeySource) -> Result<Self, Error> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| Error::Backend(format!("creating {}: {e}", data_dir.display())))?;
        let v = FileVault {
            vault_path: data_dir.join(VAULT_FILE),
            key_path: data_dir.join(KEY_FILE),
            source,
            key: Mutex::new(None),
            io: Mutex::new(()),
        };
        match source {
            FileKeySource::Device => {
                let key = v.load_or_create_device_key()?;
                *v.key.lock().map_err(|_| poisoned())? = Some(key);
            }
            FileKeySource::Passphrase => {
                // Locked until `unlock`. `CRISPSORTER_VAULT_PASSPHRASE`
                // is how a headless CLI run gets in without a TTY.
                //
                // A wrong one leaves the vault locked rather than failing
                // the open: an unopenable vault is a vault the user can
                // still unlock from the UI, whereas an error here would
                // make the whole store unbuildable and send the app to its
                // fallback — away from the secrets that are sitting right
                // there on disk.
                if let Ok(p) = std::env::var(super::ENV_PASSPHRASE) {
                    if !p.is_empty() {
                        if let Err(e) = v.unlock(&p) {
                            eprintln!("secrets: ${} did not open the vault: {e}", super::ENV_PASSPHRASE);
                        }
                    }
                }
            }
        }
        Ok(v)
    }

    fn load_or_create_device_key(&self) -> Result<Zeroizing<[u8; KEY_LEN]>, Error> {
        if let Ok(raw) = std::fs::read_to_string(&self.key_path) {
            let bytes = B64
                .decode(raw.trim())
                .map_err(|e| Error::Backend(format!("{} is not valid base64: {e}", self.key_path.display())))?;
            if bytes.len() == KEY_LEN {
                let mut raw = [0u8; KEY_LEN];
                raw.copy_from_slice(&bytes);
                return Ok(Zeroizing::new(raw));
            }
            // A truncated key file and a *missing* one are different
            // situations: silently minting a new key over the first
            // would destroy a readable vault.
            return Err(Error::Backend(format!(
                "{} is {} bytes, expected {KEY_LEN} — refusing to overwrite it",
                self.key_path.display(),
                bytes.len()
            )));
        }
        let mut raw = [0u8; KEY_LEN];
        random(&mut raw)?;
        write_private(&self.key_path, B64.encode(raw).as_bytes())?;
        Ok(Zeroizing::new(raw))
    }

    fn key(&self) -> Result<Zeroizing<[u8; KEY_LEN]>, Error> {
        self.key
            .lock()
            .map_err(|_| poisoned())?
            .clone()
            .ok_or_else(|| {
                Error::Locked("enter the vault passphrase to unlock your stored secrets".into())
            })
    }

    fn read_envelope(&self) -> Result<Option<Envelope>, Error> {
        match std::fs::read_to_string(&self.vault_path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map(Some)
                .map_err(|e| Error::Other(format!("{} is corrupt: {e}", self.vault_path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Backend(format!(
                "reading {}: {e}",
                self.vault_path.display()
            ))),
        }
    }

    fn decrypt(&self, env: &Envelope, key: &[u8]) -> Result<Vec<Item>, Error> {
        if env.version != 1 {
            return Err(Error::Other(format!(
                "vault format v{} is newer than this build understands",
                env.version
            )));
        }
        let nonce = B64
            .decode(&env.nonce)
            .map_err(|e| Error::Other(format!("bad nonce: {e}")))?;
        let ct = B64
            .decode(&env.ciphertext)
            .map_err(|e| Error::Other(format!("bad ciphertext: {e}")))?;
        if nonce.len() != NONCE_LEN {
            return Err(Error::Other("bad nonce length".into()));
        }
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| Error::Other(format!("bad key length: {e}")))?;
        let plain = cipher
            .decrypt(Nonce::from_slice(&nonce), &ct[..])
            // GCM's tag is what makes a wrong passphrase detectable at
            // all, so this arm is the passphrase check as much as it is
            // the tamper check.
            .map_err(|_| Error::Locked("the vault did not decrypt — wrong passphrase?".into()))?;
        serde_json::from_slice(&plain)
            .map_err(|e| Error::Other(format!("vault contents are not readable: {e}")))
    }

    fn load(&self) -> Result<Vec<Item>, Error> {
        let key = self.key()?;
        match self.read_envelope()? {
            Some(env) => self.decrypt(&env, key.as_ref()),
            None => Ok(Vec::new()),
        }
    }

    fn store(&self, items: &[Item]) -> Result<(), Error> {
        let key = self.key()?;
        let plain = serde_json::to_vec(items).map_err(|e| Error::Other(e.to_string()))?;
        let mut nonce = [0u8; NONCE_LEN];
        random(&mut nonce)?;
        let cipher = Aes256Gcm::new_from_slice(key.as_ref())
            .map_err(|e| Error::Other(format!("bad key length: {e}")))?;
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), &plain[..])
            .map_err(|e| Error::Other(format!("encryption failed: {e}")))?;

        // Preserve the KDF header: re-deriving a passphrase key needs the
        // original salt, so a write must never drop it.
        let (kdf, salt, m, t, p) = match (self.source, self.read_envelope()?) {
            (FileKeySource::Passphrase, Some(prev)) => (
                "argon2id".to_string(),
                prev.salt,
                prev.m_cost,
                prev.t_cost,
                prev.p_cost,
            ),
            (FileKeySource::Passphrase, None) => {
                // Should not happen: `unlock` writes the header before
                // any secret exists. Refusing beats writing a vault that
                // can never be reopened.
                return Err(Error::Other(
                    "passphrase vault has no salt header; unlock it first".into(),
                ));
            }
            (FileKeySource::Device, _) => ("none".to_string(), None, None, None, None),
        };

        let env = Envelope {
            version: 1,
            cipher: "aes-256-gcm".into(),
            kdf,
            salt,
            m_cost: m,
            t_cost: t,
            p_cost: p,
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(&ct),
        };
        let body = serde_json::to_vec_pretty(&env).map_err(|e| Error::Other(e.to_string()))?;
        write_private(&self.vault_path, &body)
    }

    /// Read-modify-write under [`FileVault::io`].
    fn mutate<F: FnOnce(&mut Vec<Item>)>(&self, f: F) -> Result<(), Error> {
        let _guard = self.io.lock().map_err(|_| poisoned())?;
        let mut items = self.load()?;
        f(&mut items);
        self.store(&items)
    }
}

fn poisoned() -> Error {
    Error::Other("file vault lock poisoned".into())
}

impl Vault for FileVault {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, Error> {
        let _guard = self.io.lock().map_err(|_| poisoned())?;
        Ok(self
            .load()?
            .into_iter()
            .find(|i| i.service == service && i.account == account)
            .map(|i| i.value))
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), Error> {
        self.mutate(|items| {
            items.retain(|i| !(i.service == service && i.account == account));
            items.push(Item {
                service: service.to_string(),
                account: account.to_string(),
                value: value.to_string(),
            });
        })
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), Error> {
        self.mutate(|items| {
            items.retain(|i| !(i.service == service && i.account == account));
        })
    }

    fn kind(&self) -> &'static str {
        "file"
    }

    fn is_unlocked(&self) -> bool {
        self.key
            .lock()
            .map(|k| k.is_some())
            .unwrap_or(false)
    }

    fn unlock(&self, passphrase: &str) -> Result<(), Error> {
        if self.source == FileKeySource::Device {
            return Ok(());
        }
        if passphrase.is_empty() {
            return Err(Error::Locked("passphrase must not be empty".into()));
        }
        let _guard = self.io.lock().map_err(|_| poisoned())?;

        match self.read_envelope()? {
            Some(env) => {
                let salt = B64
                    .decode(env.salt.as_deref().unwrap_or_default())
                    .map_err(|e| Error::Other(format!("bad salt: {e}")))?;
                let key = derive(
                    passphrase,
                    &salt,
                    env.m_cost.unwrap_or(M_COST),
                    env.t_cost.unwrap_or(T_COST),
                    env.p_cost.unwrap_or(P_COST),
                )?;
                // Proving the passphrase before accepting it keeps a typo
                // from silently becoming the key to an empty vault that
                // then overwrites the real one on the next write.
                self.decrypt(&env, key.as_ref())?;
                *self.key.lock().map_err(|_| poisoned())? = Some(key);
                Ok(())
            }
            None => {
                // First unlock ever: mint a salt, derive, and commit an
                // empty vault so the header exists before any secret does.
                let mut salt = [0u8; SALT_LEN];
                random(&mut salt)?;
                let key = derive(passphrase, &salt, M_COST, T_COST, P_COST)?;
                let mut nonce = [0u8; NONCE_LEN];
                random(&mut nonce)?;
                let cipher = Aes256Gcm::new_from_slice(key.as_ref())
                    .map_err(|e| Error::Other(format!("bad key length: {e}")))?;
                let ct = cipher
                    .encrypt(Nonce::from_slice(&nonce), &b"[]"[..])
                    .map_err(|e| Error::Other(format!("encryption failed: {e}")))?;
                let env = Envelope {
                    version: 1,
                    cipher: "aes-256-gcm".into(),
                    kdf: "argon2id".into(),
                    salt: Some(B64.encode(salt)),
                    m_cost: Some(M_COST),
                    t_cost: Some(T_COST),
                    p_cost: Some(P_COST),
                    nonce: B64.encode(nonce),
                    ciphertext: B64.encode(&ct),
                };
                let body =
                    serde_json::to_vec_pretty(&env).map_err(|e| Error::Other(e.to_string()))?;
                write_private(&self.vault_path, &body)?;
                *self.key.lock().map_err(|_| poisoned())? = Some(key);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_vault_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path(), FileKeySource::Device).unwrap();
        assert!(v.is_unlocked(), "a device vault needs no unlocking");
        v.set("CrispSorter.LLM", "llm-provider:openai", "sk-abc").unwrap();
        assert_eq!(
            v.get("CrispSorter.LLM", "llm-provider:openai").unwrap().as_deref(),
            Some("sk-abc")
        );
        v.delete("CrispSorter.LLM", "llm-provider:openai").unwrap();
        assert_eq!(v.get("CrispSorter.LLM", "llm-provider:openai").unwrap(), None);
    }

    #[test]
    fn secrets_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        {
            let v = FileVault::open(dir.path(), FileKeySource::Device).unwrap();
            v.set("s", "a", "value").unwrap();
        }
        let v2 = FileVault::open(dir.path(), FileKeySource::Device).unwrap();
        assert_eq!(v2.get("s", "a").unwrap().as_deref(), Some("value"));
    }

    #[test]
    fn nothing_readable_is_written_to_disk() {
        // The whole point of the backend: the envelope must not carry the
        // secret, nor the account it belongs to.
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path(), FileKeySource::Device).unwrap();
        v.set("CrispSorter.LLM", "llm-provider:openai", "sk-supersecret").unwrap();
        let raw = std::fs::read_to_string(dir.path().join(VAULT_FILE)).unwrap();
        assert!(!raw.contains("sk-supersecret"));
        assert!(!raw.contains("llm-provider:openai"));
        assert!(raw.contains("aes-256-gcm"));
    }

    #[test]
    fn setting_the_same_account_twice_overwrites_rather_than_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path(), FileKeySource::Device).unwrap();
        v.set("s", "a", "first").unwrap();
        v.set("s", "a", "second").unwrap();
        assert_eq!(v.get("s", "a").unwrap().as_deref(), Some("second"));
    }

    #[test]
    fn a_passphrase_vault_starts_locked_and_opens_with_the_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path(), FileKeySource::Passphrase).unwrap();
        assert!(!v.is_unlocked());
        assert!(matches!(v.get("s", "a"), Err(Error::Locked(_))));

        v.unlock("correct horse battery staple").unwrap();
        assert!(v.is_unlocked());
        v.set("s", "a", "hidden").unwrap();
        assert_eq!(v.get("s", "a").unwrap().as_deref(), Some("hidden"));
    }

    #[test]
    fn the_wrong_passphrase_is_rejected_rather_than_silently_opening_an_empty_vault() {
        // The dangerous failure is accepting a typo, showing no secrets,
        // and then overwriting the real vault on the next write.
        let dir = tempfile::tempdir().unwrap();
        {
            let v = FileVault::open(dir.path(), FileKeySource::Passphrase).unwrap();
            v.unlock("right").unwrap();
            v.set("s", "a", "hidden").unwrap();
        }
        let v2 = FileVault::open(dir.path(), FileKeySource::Passphrase).unwrap();
        assert!(matches!(v2.unlock("wrong"), Err(Error::Locked(_))));
        assert!(!v2.is_unlocked());

        v2.unlock("right").unwrap();
        assert_eq!(v2.get("s", "a").unwrap().as_deref(), Some("hidden"));
    }

    #[test]
    fn a_passphrase_vault_keeps_its_salt_across_writes() {
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path(), FileKeySource::Passphrase).unwrap();
        v.unlock("pass").unwrap();
        let salt_before = {
            let raw = std::fs::read_to_string(dir.path().join(VAULT_FILE)).unwrap();
            serde_json::from_str::<Envelope>(&raw).unwrap().salt
        };
        v.set("s", "a", "1").unwrap();
        v.set("s", "b", "2").unwrap();
        let salt_after = {
            let raw = std::fs::read_to_string(dir.path().join(VAULT_FILE)).unwrap();
            serde_json::from_str::<Envelope>(&raw).unwrap().salt
        };
        assert_eq!(salt_before, salt_after, "a rotated salt orphans the vault");
    }

    #[test]
    fn each_write_uses_a_fresh_nonce() {
        // Reusing a nonce under one key is the classic GCM break.
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path(), FileKeySource::Device).unwrap();
        v.set("s", "a", "1").unwrap();
        let first = serde_json::from_str::<Envelope>(
            &std::fs::read_to_string(dir.path().join(VAULT_FILE)).unwrap(),
        )
        .unwrap()
        .nonce;
        v.set("s", "b", "2").unwrap();
        let second = serde_json::from_str::<Envelope>(
            &std::fs::read_to_string(dir.path().join(VAULT_FILE)).unwrap(),
        )
        .unwrap()
        .nonce;
        assert_ne!(first, second);
    }

    #[test]
    fn a_truncated_key_file_is_an_error_not_a_silent_rekey() {
        // Minting a fresh key here would make an existing vault
        // permanently unreadable, which is worse than refusing to start.
        let dir = tempfile::tempdir().unwrap();
        FileVault::open(dir.path(), FileKeySource::Device).unwrap();
        std::fs::write(dir.path().join(KEY_FILE), B64.encode([0u8; 8])).unwrap();
        assert!(matches!(
            FileVault::open(dir.path(), FileKeySource::Device),
            Err(Error::Backend(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn vault_and_key_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let v = FileVault::open(dir.path(), FileKeySource::Device).unwrap();
        v.set("s", "a", "x").unwrap();
        for f in [VAULT_FILE, KEY_FILE] {
            let mode = std::fs::metadata(dir.path().join(f))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{f} is readable by someone else");
        }
    }
}
