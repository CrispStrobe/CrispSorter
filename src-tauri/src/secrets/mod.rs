//! Credential storage for LLM provider API keys.
//!
//! Sister module to `images::crisplens::secret` — same pattern (vault
//! entries, keyed per account) but generalised for any caller, not just
//! CrispLens sessions. Used by the LLM provider settings flow so that
//! API keys never land in `settings.json` (where they'd otherwise leak
//! through cloud-sync, backups, or bug-report tarballs).
//!
//! Account naming convention from the frontend:
//!   `llm-provider:<provider-id>`   e.g.  `llm-provider:openai`
//!                                         `llm-provider:groq`
//!
//! Where those entries physically live is the user's choice, not a
//! constant — see [`vault`]. On the default (the OS credential store)
//! this becomes the visible row name in Keychain Access; the SERVICE
//! field is `CrispSorter.LLM` so the rows group together visually.
//!
//! See [`tauri_commands`] for the Tauri surface; the storage primitives
//! live here and are sync (the keychain APIs are blocking on every
//! platform, but cheap — single-digit ms — so we don't bother with
//! async).

pub mod tauri_commands;
pub mod vault;
pub mod vault_commands;

use vault::Entry;

/// The service identifier under which all LLM-provider keys live. On
/// the OS-credential-store vault it is visible to the user in Keychain
/// Access / Credential Manager / Seahorse.
pub const SERVICE: &str = "CrispSorter.LLM";

/// Errors flowing out of the secret-store layer. Does not wrap
/// [`vault::Error`] because its platform-specific detail is noisy at
/// the Tauri command boundary; we surface a short reason and log the
/// full underlying error. [`SecretError::Denied`] is the exception —
/// the UI has to tell those apart to offer the "pick another store"
/// escape hatch.
#[derive(Debug)]
pub enum SecretError {
    /// OS keychain unreachable (locked vault, dbus down, no backend).
    Backend(String),
    /// Reachable but the entry doesn't exist.
    NotFound,
    /// Read/write failure that isn't "not found".
    Other(String),
    /// The credential store asked the user and the answer was no.
    /// Distinct from [`SecretError::Other`] because it is the one
    /// failure a different vault would fix.
    Denied(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Backend(s) => write!(f, "keychain backend unavailable: {s}"),
            SecretError::NotFound => write!(f, "no stored secret for this account"),
            SecretError::Other(s) => write!(f, "keychain error: {s}"),
            SecretError::Denied(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Build a fresh Entry for the given account. Cheap — an `Entry` is
/// just a (service, account) pair that resolves the active vault on
/// use, so production code constructs one per call.
pub fn entry_for(account: &str) -> Result<Entry, SecretError> {
    Entry::new(SERVICE, account).map_err(from_vault)
}

/// Carry [`vault::Error::Denied`] across as [`SecretError::Denied`] so
/// the UI can distinguish "your store said no" from "your store broke".
fn from_vault(e: vault::Error) -> SecretError {
    match e {
        vault::Error::NoEntry => SecretError::NotFound,
        vault::Error::Denied(_) | vault::Error::Locked(_) => SecretError::Denied(e.to_string()),
        vault::Error::Backend(_) => SecretError::Backend(e.to_string()),
        vault::Error::Other(_) => SecretError::Other(e.to_string()),
    }
}

// ── Entry-taking primitives ─────────────────────────────────────────
//
// The functions below take a `&Entry`. They predate the vault layer,
// when `keyring::mock` forced tests to share one `Entry` instance
// across set/get/delete. That constraint is gone — an `Entry` binds
// nothing — but the signatures stayed, because the high-level wrappers
// below are what production calls anyway.

pub(crate) fn set_secret_at(entry: &Entry, value: &str) -> Result<(), SecretError> {
    entry.set_password(value).map_err(from_vault)
}

pub(crate) fn get_secret_at(entry: &Entry) -> Result<Option<String>, SecretError> {
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(vault::Error::NoEntry) => Ok(None),
        Err(e) => Err(from_vault(e)),
    }
}

pub(crate) fn delete_secret_at(entry: &Entry) -> Result<(), SecretError> {
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(vault::Error::NoEntry) => Ok(()),
        Err(e) => Err(from_vault(e)),
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
    use vault::memory::MemoryVault;
    use vault::Vault;

    /// The vault these tests run against. An in-memory vault needs no
    /// global installation and no `Once` guard — unlike `keyring::mock`,
    /// which was per-`Entry` and forced every case to thread one
    /// instance through set/get/delete.
    fn mock_entry(_account: &str) -> MemoryVault {
        MemoryVault::new()
    }

    fn set_secret_at(v: &MemoryVault, value: &str) -> Result<(), SecretError> {
        v.set(SERVICE, "test", value).map_err(from_vault)
    }

    fn get_secret_at(v: &MemoryVault) -> Result<Option<String>, SecretError> {
        v.get(SERVICE, "test").map_err(from_vault)
    }

    fn delete_secret_at(v: &MemoryVault) -> Result<(), SecretError> {
        v.delete(SERVICE, "test").map_err(from_vault)
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
