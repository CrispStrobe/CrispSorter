//! The platform's own credential store, via the `keyring` crate.
//!
//! macOS login keychain, Windows Credential Manager, Linux Secret
//! Service. This is what CrispSorter used unconditionally before the
//! vault became a choice, and it stays the default so existing installs
//! keep reading the secrets they already have.
//!
//! The one thing this wrapper adds is [`classify`]: turning the
//! platform's refusal into [`Error::Denied`] and latching it, so a
//! dialog the user dismissed is not re-raised once per secret for the
//! rest of the session.

use super::{latch_denied, Error, Vault};

/// Zero-sized: `keyring::Entry` is constructed per call and the store
/// itself is the OS.
pub struct OsKeyring;

/// Map a `keyring` error onto the vault's error type, latching the
/// denial flag when the platform is telling us the *user* said no.
///
/// The classification is substring-based because `keyring` erases the
/// platform error into a `Box<dyn Error>` — there is no typed code to
/// match on. Getting it wrong in the conservative direction (not
/// recognising a denial) costs an extra dialog; getting it wrong the
/// other way would suppress a real error, so only unambiguous markers
/// are listed.
fn classify(e: keyring::Error) -> Error {
    if matches!(e, keyring::Error::NoEntry) {
        return Error::NoEntry;
    }
    let text = e.to_string();
    let lower = text.to_ascii_lowercase();

    // macOS `OSStatus` values, as rendered by security-framework:
    //   -128    errSecUserCanceled          — the user hit Deny / Cancel
    //   -25293  errSecAuthFailed            — wrong keychain password
    //   -25308  errSecInteractionNotAllowed — locked, and nobody can be asked
    //   -25315  errSecInteractionRequired   — a prompt was needed, none shown
    const DENIAL_CODES: [&str; 4] = ["-128", "-25293", "-25308", "-25315"];
    const DENIAL_WORDS: [&str; 6] = [
        "user canceled",
        "user cancelled",
        "interaction is not allowed",
        "interaction not allowed",
        "not correct",   // "The user name or passphrase you entered is not correct."
        "prompt dismissed", // Secret Service, when the user closes the unlock dialog
    ];

    let denied = DENIAL_CODES.iter().any(|c| text.contains(c))
        || DENIAL_WORDS.iter().any(|w| lower.contains(w));

    if denied {
        latch_denied();
        return Error::Denied(text);
    }
    Error::Other(text)
}

impl OsKeyring {
    fn entry(service: &str, account: &str) -> Result<keyring::Entry, Error> {
        keyring::Entry::new(service, account).map_err(|e| Error::Backend(e.to_string()))
    }
}

impl Vault for OsKeyring {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, Error> {
        // The latch: once the user has refused, every further read this
        // run would raise the same dialog. Refuse locally instead.
        if super::is_denied() {
            return Err(Error::Denied("already refused this session".into()));
        }
        match Self::entry(service, account)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(classify(e)),
        }
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), Error> {
        // Writes are deliberately *not* latched. Creating an item does
        // not consult an ACL — it makes one — so a write can succeed on a
        // keychain whose existing rows we cannot read, and refusing it
        // here would strand a user who is trying to re-enter their key.
        Self::entry(service, account)?
            .set_password(value)
            .map_err(classify)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), Error> {
        match Self::entry(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(classify(e)),
        }
    }

    fn kind(&self) -> &'static str {
        "os-default"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_dialog_is_a_denial_and_latches() {
        super::super::clear_denied();
        let e = classify(keyring::Error::PlatformFailure(
            "SecKeychain error (-128)".into(),
        ));
        assert!(matches!(e, Error::Denied(_)));
        assert!(super::super::is_denied(), "the latch must be up");
        super::super::clear_denied();
    }

    #[test]
    fn a_wrong_keychain_password_is_a_denial() {
        super::super::clear_denied();
        let e = classify(keyring::Error::PlatformFailure(
            "The user name or passphrase you entered is not correct.".into(),
        ));
        assert!(matches!(e, Error::Denied(_)));
        super::super::clear_denied();
    }

    #[test]
    fn an_ordinary_failure_is_not_a_denial() {
        // Misclassifying here would hide real breakage behind "switch
        // your secret store", so the test pins the conservative side.
        super::super::clear_denied();
        let e = classify(keyring::Error::PlatformFailure("disk I/O error".into()));
        assert!(matches!(e, Error::Other(_)));
        assert!(!super::super::is_denied());
    }

    #[test]
    fn no_entry_stays_no_entry() {
        assert!(matches!(classify(keyring::Error::NoEntry), Error::NoEntry));
    }
}
