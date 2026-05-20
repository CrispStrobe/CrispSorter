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

/// Build a fresh Entry for the given account. Cheap on real OS
/// keychains; production code constructs one per call.
pub fn entry_for(account: &str) -> Result<Entry, SecretError> {
    Entry::new(SERVICE, account).map_err(|e| SecretError::Backend(e.to_string()))
}

// ── Entry-taking primitives (testable with keyring::mock) ────────────
//
// The functions below take a `&Entry`. They're what the mock-based
// unit tests exercise — `keyring::mock` is per-Entry, so set/get/
// delete only round-trip when they share the same Entry instance.
// The high-level wrappers further down construct a fresh Entry each
// call (the normal production path).

pub(crate) fn set_secret_at(entry: &Entry, value: &str) -> Result<(), SecretError> {
    entry
        .set_password(value)
        .map_err(|e| SecretError::Other(e.to_string()))
}

pub(crate) fn get_secret_at(entry: &Entry) -> Result<Option<String>, SecretError> {
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretError::Other(e.to_string())),
    }
}

pub(crate) fn delete_secret_at(entry: &Entry) -> Result<(), SecretError> {
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(SecretError::Other(e.to_string())),
    }
}

// ── High-level wrappers (build a fresh Entry per call) ──────────────

/// Store a secret under the given account name. Overwrites any
/// existing value.
pub fn set_secret(account: &str, value: &str) -> Result<(), SecretError> {
    set_secret_at(&entry_for(account)?, value)
}

/// Read a secret. Returns `Ok(None)` when nothing is stored — callers
/// pattern-match on the Option rather than inspecting an opaque error.
pub fn get_secret(account: &str) -> Result<Option<String>, SecretError> {
    get_secret_at(&entry_for(account)?)
}

/// Delete a stored secret. Idempotent: no-op when there's nothing
/// to delete.
pub fn delete_secret(account: &str) -> Result<(), SecretError> {
    delete_secret_at(&entry_for(account)?)
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
    use keyring::mock::default_credential_builder;
    use std::sync::Once;

    /// Switch keyring's global credential builder to the in-memory
    /// mock. Same idempotent install pattern as
    /// `src/images/crisplens/secret.rs` — `keyring` uses an internal
    /// OnceLock, so we guard our own with a `Once`.
    fn install_mock_keyring() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            keyring::set_default_credential_builder(default_credential_builder());
        });
    }

    /// `keyring::mock` is per-Entry — two independent `Entry::new`
    /// calls don't share state. Tests hold one Entry across all
    /// operations in a case.
    fn mock_entry(account: &str) -> Entry {
        install_mock_keyring();
        Entry::new(SERVICE, account).unwrap()
    }

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

    #[test]
    fn set_then_get_round_trips_a_value() {
        let e = mock_entry("test-round-trip");
        set_secret_at(&e, "sk-the-key").unwrap();
        let stored = get_secret_at(&e).unwrap();
        assert_eq!(stored.as_deref(), Some("sk-the-key"));
    }

    #[test]
    fn get_returns_none_when_no_entry_exists() {
        let e = mock_entry("test-empty");
        let stored = get_secret_at(&e).unwrap();
        assert!(stored.is_none(), "fresh mock should be NoEntry-shaped");
    }

    #[test]
    fn set_overwrites_existing_value() {
        let e = mock_entry("test-overwrite");
        set_secret_at(&e, "first").unwrap();
        set_secret_at(&e, "second").unwrap();
        let stored = get_secret_at(&e).unwrap();
        assert_eq!(stored.as_deref(), Some("second"));
    }

    #[test]
    fn delete_removes_the_value() {
        let e = mock_entry("test-delete");
        set_secret_at(&e, "to-delete").unwrap();
        delete_secret_at(&e).unwrap();
        let stored = get_secret_at(&e).unwrap();
        assert!(stored.is_none(), "post-delete read should return None");
    }

    #[test]
    fn delete_is_idempotent() {
        let e = mock_entry("test-delete-idempotent");
        // No prior set — delete should still succeed.
        delete_secret_at(&e).unwrap();
        delete_secret_at(&e).unwrap();
    }

    #[test]
    fn unicode_values_round_trip() {
        // Some LLM tokens carry punctuation / non-ASCII. Make sure
        // we don't accidentally truncate or normalise.
        let e = mock_entry("test-unicode");
        let value = "sk-aBc_-?2 !€/한국어/🦀";
        set_secret_at(&e, value).unwrap();
        assert_eq!(get_secret_at(&e).unwrap().as_deref(), Some(value));
    }
}
