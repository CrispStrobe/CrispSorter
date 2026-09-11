//! A named macOS keychain — anything but the login one.
//!
//! The login keychain is a bad home for an app's secrets when the app is
//! rebuilt or re-signed, because a keychain item remembers the exact
//! binary that made it. A new binary is not on the item's access list, so
//! macOS asks; saying "Always Allow" *edits* that access list, and
//! editing it requires the keychain's own password. A user whose login
//! keychain password has drifted from their account password — which an
//! Apple ID reset does silently — has no way to answer, and the dialog
//! comes back for every secret, every launch.
//!
//! Pointing the app at a different keychain breaks that loop in two
//! ways. A keychain **CrispSorter creates** ([`MacUnlock::AppManaged`])
//! comes with a password the app stores in a `0600` file and can present
//! on demand, so the dialog is answerable. A keychain the **user already
//! owns** ([`MacUnlock::Prompt`]) — a build keychain, say — has a
//! password they already know, so "Always Allow" works and the dialog
//! stops for good.
//!
//! What this backend does *not* do is make the dialog impossible: the
//! access list is still per-binary, and only a permissive ACL set at item
//! creation would avoid it entirely, which the `security-framework`
//! bindings do not expose. For a guaranteed prompt-free vault, use
//! [`super::file_vault`].

use super::{latch_denied, write_private, Error, MacUnlock, Vault};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};
use std::path::{Path, PathBuf};

/// Name CrispSorter gives a keychain of its own.
pub const DEFAULT_KEYCHAIN_NAME: &str = "CrispSorter";

/// `errSecItemNotFound` — "nothing stored", not a failure.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;
/// `errSecUserCanceled` — the user hit Cancel or Deny.
const ERR_SEC_USER_CANCELED: i32 = -128;
/// `errSecAuthFailed` — wrong keychain password.
const ERR_SEC_AUTH_FAILED: i32 = -25293;
/// `errSecInteractionNotAllowed` — locked, and nobody could be asked.
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

pub struct MacKeychainVault {
    path: PathBuf,
    /// `Some` when CrispSorter manages the password itself. The keychain
    /// is opened and unlocked per operation rather than held open: a
    /// `SecKeychain` is neither `Send` nor `Sync`, and reopening costs
    /// far less than the dialog this whole module exists to avoid.
    password: Option<String>,
}

/// Turn a Security-framework error into a vault error, latching the
/// denial flag when the user is the one saying no.
fn classify(e: security_framework::base::Error, what: &str) -> Error {
    match e.code() {
        ERR_SEC_USER_CANCELED | ERR_SEC_AUTH_FAILED | ERR_SEC_INTERACTION_NOT_ALLOWED => {
            latch_denied();
            Error::Denied(format!("{what}: {e}"))
        }
        _ => Error::Other(format!("{what}: {e}")),
    }
}

/// Where the password of an app-managed keychain is kept.
///
/// Beside the app data, not inside the keychain directory: the keychain
/// file is something the user may hand to Keychain Access, back up, or
/// copy between machines, and the password should not travel with it by
/// default.
pub fn password_file(data_dir: &Path, keychain: &Path) -> PathBuf {
    let name = keychain
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("keychain");
    data_dir.join("keychains").join(format!("{name}.pw"))
}

/// A fresh keychain password: 24 random bytes, URL-safe base64, so it
/// survives being copied out of the UI and pasted into a macOS dialog.
pub fn generate_password() -> Result<String, Error> {
    let mut raw = [0u8; 24];
    getrandom::getrandom(&mut raw)
        .map_err(|e| Error::Backend(format!("no system randomness: {e}")))?;
    Ok(B64URL.encode(raw))
}

/// Create a keychain at `path`.
///
/// `password` of `None` mints one and stores it under
/// [`password_file`]; the generated password comes back so the UI can
/// show it to the user once. An existing file at `path` is left alone —
/// replacing a keychain would silently destroy whatever is in it.
pub fn create_keychain(
    path: &Path,
    data_dir: &Path,
    password: Option<&str>,
) -> Result<String, Error> {
    if path.exists() {
        return Err(Error::Backend(format!(
            "{} already exists — choose it instead of creating it",
            path.display()
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Backend(format!("creating {}: {e}", parent.display())))?;
    }
    let pw = match password {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => generate_password()?,
    };
    CreateOptions::new()
        .password(&pw)
        .create(path)
        .map_err(|e| classify(e, "creating the keychain"))?;

    let pw_file = password_file(data_dir, path);
    if let Some(parent) = pw_file.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Backend(format!("creating {}: {e}", parent.display())))?;
    }
    write_private(&pw_file, pw.as_bytes())?;
    Ok(pw)
}

/// Read back the stored password of an app-managed keychain, so the UI
/// can show it when macOS asks the user for it.
pub fn stored_password(data_dir: &Path, keychain: &Path) -> Option<String> {
    std::fs::read_to_string(password_file(data_dir, keychain))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

impl MacKeychainVault {
    pub fn open(path: &Path, unlock: MacUnlock, data_dir: &Path) -> Result<Self, Error> {
        if !path.exists() {
            return Err(Error::Backend(format!(
                "no keychain at {} — create it first",
                path.display()
            )));
        }
        let password = match unlock {
            MacUnlock::AppManaged => {
                let pw = stored_password(data_dir, path);
                if pw.is_none() {
                    // Not fatal: the keychain may simply be unlocked
                    // already, or the user may be happy to answer the
                    // dialog. Degrading to prompt-on-demand beats
                    // refusing to open a vault that holds their keys.
                    log_missing_password(path);
                }
                pw
            }
            MacUnlock::Prompt => None,
        };
        Ok(MacKeychainVault {
            path: path.to_path_buf(),
            password,
        })
    }

    fn keychain(&self) -> Result<SecKeychain, Error> {
        let mut kc = SecKeychain::open(&self.path)
            .map_err(|e| classify(e, &format!("opening {}", self.path.display())))?;
        if let Some(pw) = &self.password {
            // An already-unlocked keychain reports an error on some macOS
            // versions; the operation that follows is the real test, so
            // this one is advisory.
            let _ = kc.unlock(Some(pw.as_str()));
        }
        Ok(kc)
    }
}

fn log_missing_password(path: &Path) {
    eprintln!(
        "secrets: no stored password for keychain {} — macOS will ask for it when needed",
        path.display()
    );
}

impl Vault for MacKeychainVault {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, Error> {
        if super::is_denied() {
            return Err(Error::Denied("already refused this session".into()));
        }
        let kc = self.keychain()?;
        match kc.find_generic_password(service, account) {
            Ok((password, _item)) => Ok(Some(String::from_utf8_lossy(&password).into_owned())),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(classify(e, "reading the keychain")),
        }
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), Error> {
        let kc = self.keychain()?;
        kc.set_generic_password(service, account, value.as_bytes())
            .map_err(|e| classify(e, "writing to the keychain"))
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), Error> {
        let kc = self.keychain()?;
        match kc.find_generic_password(service, account) {
            Ok((_password, item)) => {
                // `SecKeychainItem::delete` consumes the item; discard the
                // result so this compiles whether it yields `()` or a
                // `Result`, and stay idempotent either way.
                let _ = item.delete();
                Ok(())
            }
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(e) => Err(classify(e, "deleting from the keychain")),
        }
    }

    fn kind(&self) -> &'static str {
        "mac-keychain"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_passwords_are_long_and_distinct() {
        let a = generate_password().unwrap();
        let b = generate_password().unwrap();
        assert_ne!(a, b);
        assert!(a.len() >= 32, "got {} chars", a.len());
    }

    #[test]
    fn the_password_file_is_named_after_the_keychain() {
        let p = password_file(
            Path::new("/data"),
            Path::new("/Users/x/Library/Keychains/CrispSorter.keychain-db"),
        );
        assert_eq!(p, Path::new("/data/keychains/CrispSorter.pw"));
    }

    #[test]
    fn opening_a_keychain_that_is_not_there_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let err = MacKeychainVault::open(
            &dir.path().join("nope.keychain-db"),
            MacUnlock::AppManaged,
            dir.path(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Backend(_)));
    }

    #[test]
    fn creating_over_an_existing_keychain_is_refused() {
        // Overwriting would destroy whatever the user already keeps there.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.keychain-db");
        std::fs::write(&path, b"not really a keychain").unwrap();
        assert!(matches!(
            create_keychain(&path, dir.path(), None),
            Err(Error::Backend(_))
        ));
    }

    #[test]
    fn a_stored_password_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let kc = dir.path().join("Test.keychain-db");
        assert_eq!(stored_password(dir.path(), &kc), None);
        let f = password_file(dir.path(), &kc);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        write_private(&f, b"hunter2\n").unwrap();
        assert_eq!(stored_password(dir.path(), &kc).as_deref(), Some("hunter2"));
    }

    /// Creating a real keychain writes into `~/Library/Keychains` and is
    /// meaningless on a headless runner, so it is opt-in.
    #[test]
    #[ignore = "touches the real macOS keychain; run with --ignored locally"]
    fn round_trips_against_a_real_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CrispSorterTest.keychain-db");
        create_keychain(&path, dir.path(), Some("test-password")).unwrap();
        let v = MacKeychainVault::open(&path, MacUnlock::AppManaged, dir.path()).unwrap();
        v.set("CrispSorter.Test", "account", "value").unwrap();
        assert_eq!(
            v.get("CrispSorter.Test", "account").unwrap().as_deref(),
            Some("value")
        );
        v.delete("CrispSorter.Test", "account").unwrap();
        assert_eq!(v.get("CrispSorter.Test", "account").unwrap(), None);
    }
}
