//! OS-keychain credential storage for LLM provider API keys.
//!
//! Sister module to `images::crisplens::secret` — same pattern (keyring
//! crate, per-account entries) but generalised for any caller, not just
//! CrispLens sessions. Used by the LLM provider settings flow so that
//! API keys never land in `settings.json` (where they'd otherwise leak
//! through cloud-sync, backups, or bug-report tarballs).
//!
//! Account naming convention from the frontend:
//!   `llm-provider:<provider-id>`   e.g.  `llm-provider:openai`
//!                                         `llm-provider:groq`
//!
//! On macOS this becomes the visible row name in Keychain Access — the
//! user can audit and manually revoke any key. The SERVICE field is
//! `CrispSorter.LLM` so the rows group together visually.
//!
//! See [`tauri_commands`] for the Tauri surface; the storage primitives
//! live here and are sync (the keychain APIs are blocking on every
//! platform, but cheap — single-digit ms — so we don't bother with
//! async).

pub mod tauri_commands;

use keyring::Entry;

/// The OS-keychain service identifier under which all LLM-provider
/// keys live. Visible to the user in Keychain Access / Credential
/// Manager / Seahorse.
pub const SERVICE: &str = "CrispSorter.LLM";

/// Errors flowing out of the secret-store layer. Does not wrap
/// `keyring::Error` because its platform-specific detail is noisy at
/// the Tauri command boundary; we surface a short reason and log the
/// full underlying error.
#[derive(Debug)]
pub enum SecretError {
    /// OS keychain unreachable (locked vault, dbus down, no backend).
    Backend(String),
    /// Reachable but the entry doesn't exist.
    NotFound,
    /// Read/write failure that isn't "not found".
    Other(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Backend(s) => write!(f, "keychain backend unavailable: {s}"),
            SecretError::NotFound => write!(f, "no stored secret for this account"),
            SecretError::Other(s) => write!(f, "keychain error: {s}"),
        }
    }
}

impl std::error::Error for SecretError {}

fn entry_for(account: &str) -> Result<Entry, SecretError> {
    Entry::new(SERVICE, account).map_err(|e| SecretError::Backend(e.to_string()))
}

/// Store a secret under the given account name. Overwrites any
/// existing value.
pub fn set_secret(account: &str, value: &str) -> Result<(), SecretError> {
    let entry = entry_for(account)?;
    entry
        .set_password(value)
        .map_err(|e| SecretError::Other(e.to_string()))
}

/// Read a secret. Returns `Ok(None)` when nothing is stored — callers
/// pattern-match on the Option rather than inspecting an opaque error.
pub fn get_secret(account: &str) -> Result<Option<String>, SecretError> {
    let entry = entry_for(account)?;
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretError::Other(e.to_string())),
    }
}

/// Delete a stored secret. Idempotent: no-op when there's nothing
/// to delete.
pub fn delete_secret(account: &str) -> Result<(), SecretError> {
    let entry = entry_for(account)?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(SecretError::Other(e.to_string())),
    }
}

/// Convention: the frontend stores `@keyring/<account>` in
/// `settings.json` as a sentinel. This helper checks whether a string
/// is one of those sentinels and, if so, returns the account name.
///
/// ```ignore
/// assert_eq!(sentinel_account("@keyring/llm-provider:openai"),
///            Some("llm-provider:openai"));
/// assert_eq!(sentinel_account("sk-real-key-here"), None);
/// ```
pub fn sentinel_account(s: &str) -> Option<&str> {
    s.strip_prefix("@keyring/")
}

/// Make a sentinel for the given account — the inverse of
/// [`sentinel_account`]. Used by the migration when moving a
/// plain-text key into the keychain.
pub fn make_sentinel(account: &str) -> String {
    format!("@keyring/{account}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinel_round_trip() {
        let acct = "llm-provider:openai";
        let s = make_sentinel(acct);
        assert_eq!(s, "@keyring/llm-provider:openai");
        assert_eq!(sentinel_account(&s), Some(acct));
    }

    #[test]
    fn plain_text_is_not_a_sentinel() {
        assert!(sentinel_account("sk-test-1234").is_none());
        assert!(sentinel_account("").is_none());
    }
}
