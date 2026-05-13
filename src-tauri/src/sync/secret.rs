//! P13.7 Step 5 — keychain storage for the cloud-backup API bearer token.
//!
//! Mirrors `src-tauri/src/images/crisplens/secret.rs` exactly — same
//! risk-register requirement (never write the credential to
//! `tauri-plugin-store` JSON), same per-URL keying so a user with
//! both a corporate and a home cloud-backup VPS keeps the credentials
//! separate.
//!
//! Different SERVICE name ("CrispSorter.CloudBackup") so the macOS
//! Keychain Access UI shows the rows separately from CrispLens
//! entries, and an OS-managed delete on one doesn't accidentally
//! wipe the other.

use keyring::Entry;

/// Service identifier for cloud-backup tokens.  Distinct from the
/// `CrispSorter.CrispLens` service so the OS keychain row labels
/// + per-service revoke operations stay distinct.
pub const SERVICE: &str = "CrispSorter.CloudBackup";

#[derive(Debug)]
pub enum SecretError {
    Backend(String),
    NotFound,
    Other(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Backend(s) => write!(f, "keychain backend unavailable: {s}"),
            SecretError::NotFound   => write!(f, "no stored cloud-backup token for this URL"),
            SecretError::Other(s)   => write!(f, "keychain error: {s}"),
        }
    }
}

impl std::error::Error for SecretError {}

pub fn entry_for(url: &str) -> Result<Entry, SecretError> {
    Entry::new(SERVICE, url).map_err(|e| SecretError::Backend(e.to_string()))
}

pub fn set_token(entry: &Entry, raw_token: &str) -> Result<(), SecretError> {
    entry
        .set_password(raw_token)
        .map_err(|e| SecretError::Other(e.to_string()))
}

pub fn get_token(entry: &Entry) -> Result<Option<String>, SecretError> {
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretError::Other(e.to_string())),
    }
}

pub fn clear_token(entry: &Entry) -> Result<(), SecretError> {
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(SecretError::Other(e.to_string())),
    }
}

pub fn set_token_for_url(url: &str, raw_token: &str) -> Result<(), SecretError> {
    let entry = entry_for(url)?;
    set_token(&entry, raw_token)
}

pub fn get_token_for_url(url: &str) -> Result<Option<String>, SecretError> {
    let entry = entry_for(url)?;
    get_token(&entry)
}

pub fn clear_token_for_url(url: &str) -> Result<(), SecretError> {
    let entry = entry_for(url)?;
    clear_token(&entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring::mock::default_credential_builder;
    use std::sync::Once;

    fn install_mock_keyring() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            keyring::set_default_credential_builder(default_credential_builder());
        });
    }

    fn mock_entry() -> keyring::Entry {
        install_mock_keyring();
        keyring::Entry::new(SERVICE, "test-fixture").unwrap()
    }

    #[test]
    fn set_then_get_round_trips_the_token() {
        let e = mock_entry();
        set_token(&e, "cbk_abc").unwrap();
        assert_eq!(get_token(&e).unwrap().as_deref(), Some("cbk_abc"));
    }

    #[test]
    fn clear_is_idempotent_when_nothing_stored() {
        let e = mock_entry();
        clear_token(&e).unwrap();
        // Second call also OK.
        clear_token(&e).unwrap();
    }
}
