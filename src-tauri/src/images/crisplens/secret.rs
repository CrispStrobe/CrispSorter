//! P13/B1 — credential storage backed by the OS-native secret store.
//!
//! Per the spec's risk register:
//!
//! > Token storage — JSON config leaks credentials on backup /
//! > cloud-sync.  Use Keychain / DPAPI / secret-service
//! > (`keyring-rs` crate); never write token to `tauri-plugin-store`
//! > JSON.  Settings UI only stores the URL there.
//!
//! What we actually store: the `session=<value>` cookie issued by
//! CrispLens at login.  This is the auth credential — same shape
//! the browser would persist, just on disk in the OS-managed
//! credential vault (Keychain on macOS, secret-service / kwallet
//! on Linux, Credential Manager / DPAPI on Windows).
//!
//! The CrispLens URL is the second half of the identity tuple — a
//! user might have one cookie for their corporate CrispLens and a
//! different one for their home CrispLens, so per-URL storage keeps
//! them straight.  Username isn't part of the key because the cookie
//! IS the proof-of-identity at this layer; CrispLens's `/auth/me`
//! resolves cookie → user when the session is loaded.

use keyring::Entry;

/// The service identifier we register with the OS keychain.  All
/// CrispLens cookies live under this service; the per-instance
/// account is the URL.  On macOS this becomes the visible row name
/// in Keychain Access (so the user can audit / revoke manually if
/// they want).
pub const SERVICE: &str = "CrispSorter.CrispLens";

/// Errors flowing out of the secret-store layer.  Specifically does
/// NOT wrap `keyring::Error` — the underlying error type carries
/// platform-specific detail that's noisy for the Tauri command
/// boundary.  We surface a short reason; the platform error goes to
/// the application log.
#[derive(Debug)]
pub enum SecretError {
    /// The OS keychain isn't reachable at all (locked vault, broken
    /// dbus, no platform backend compiled in).
    Backend(String),
    /// The keychain is reachable but the entry doesn't exist.
    NotFound,
    /// Generic catch-all for read/write failures that aren't
    /// distinguishable as "not found".
    Other(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Backend(s) => write!(f, "keychain backend unavailable: {s}"),
            SecretError::NotFound   => write!(f, "no stored session for this CrispLens URL"),
            SecretError::Other(s)   => write!(f, "keychain error: {s}"),
        }
    }
}

impl std::error::Error for SecretError {}

/// Build a fresh keychain Entry for the given URL.  Production
/// callers create one per operation (cheap on the real OS
/// keychain because lookups go through the system credential
/// API).  Tests construct one once per case and pass it to the
/// `set/get/clear` helpers below, because `keyring::mock` is
/// per-Entry — two independent `Entry::new` calls don't share
/// state.
pub fn entry_for(url: &str) -> Result<Entry, SecretError> {
    Entry::new(SERVICE, url).map_err(|e| SecretError::Backend(e.to_string()))
}

/// Store the CrispLens session cookie.  Overwrites any existing
/// entry — there's only ever one active session per URL.
pub fn set_session(entry: &Entry, cookie_value: &str) -> Result<(), SecretError> {
    entry
        .set_password(cookie_value)
        .map_err(|e| SecretError::Other(e.to_string()))
}

/// Read the stored cookie.  Returns `Ok(None)` for not-found (the
/// more common "nothing stored yet" case) so callers can
/// pattern-match without inspecting an opaque error.
pub fn get_session(entry: &Entry) -> Result<Option<String>, SecretError> {
    match entry.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretError::Other(e.to_string())),
    }
}

/// Delete the stored cookie.  No-op when there's nothing to delete
/// — keeps `images_logout` idempotent.
pub fn clear_session(entry: &Entry) -> Result<(), SecretError> {
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(SecretError::Other(e.to_string())),
    }
}

// ── URL-based convenience wrappers ────────────────────────────────────────
//
// These are the high-level calls the Tauri command + CLI surface
// uses.  Each constructs a fresh OS-keychain Entry; against the
// real keychain, persistence is across calls because the OS owns
// the store.  Tests bypass these and use the `&Entry`-taking
// primitives above so they can share a mock Entry.

pub fn set_session_for_url(url: &str, cookie_value: &str) -> Result<(), SecretError> {
    let entry = entry_for(url)?;
    set_session(&entry, cookie_value)
}

pub fn get_session_for_url(url: &str) -> Result<Option<String>, SecretError> {
    let entry = entry_for(url)?;
    get_session(&entry)
}

pub fn clear_session_for_url(url: &str) -> Result<(), SecretError> {
    let entry = entry_for(url)?;
    clear_session(&entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring::mock::default_credential_builder;
    use std::sync::Once;

    /// Switch keyring's global credential builder to the in-memory
    /// mock once per process.  The library uses a static
    /// `OnceLock<dyn CredentialBuilder>` so calling this more than
    /// once is safe but only the first call wins — that's why a
    /// `Once` guards it.
    fn install_mock_keyring() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            keyring::set_default_credential_builder(default_credential_builder());
        });
    }

    /// `keyring::mock` is per-Entry — two independent `Entry::new`
    /// calls don't share state.  To round-trip set→get we hold one
    /// Entry across both calls.
    fn mock_entry() -> keyring::Entry {
        install_mock_keyring();
        keyring::Entry::new(SERVICE, "test-fixture").unwrap()
    }

    #[test]
    fn set_then_get_round_trips_the_cookie() {
        let e = mock_entry();
        set_session(&e, "session=abc123").unwrap();
        let stored = get_session(&e).unwrap();
        assert_eq!(stored.as_deref(), Some("session=abc123"));
    }

    #[test]
    fn get_returns_none_when_no_entry_exists() {
        let e = mock_entry();
        let stored = get_session(&e).unwrap();
        assert!(stored.is_none(), "fresh mock entry should be NoEntry-shaped");
    }

    #[test]
    fn set_overwrites_existing_entry() {
        let e = mock_entry();
        set_session(&e, "session=first").unwrap();
        set_session(&e, "session=second").unwrap();
        let stored = get_session(&e).unwrap();
        assert_eq!(stored.as_deref(), Some("session=second"));
    }

    #[test]
    fn clear_removes_the_entry() {
        let e = mock_entry();
        set_session(&e, "session=xyz").unwrap();
        clear_session(&e).unwrap();
        let stored = get_session(&e).unwrap();
        assert!(stored.is_none());
    }

    #[test]
    fn clear_on_nonexistent_entry_is_noop() {
        // Idempotency invariant: logout twice in a row must succeed.
        let e = mock_entry();
        clear_session(&e).unwrap();
        clear_session(&e).unwrap();
    }
}
