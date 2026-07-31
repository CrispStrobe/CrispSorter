//! OS-keychain storage for native cloud-drive sessions.
//!
//! A native Internxt session contains more than a bearer token: the mnemonic,
//! bucket id, and bridge credentials are required to decrypt and transfer
//! files. None of that belongs in `drives.json` or a synced settings file.

use anyhow::{Context, Result};
use keyring::Entry;

const SERVICE: &str = "CrispSorter.CloudDrive";

fn entry(drive_id: &str) -> Result<Entry> {
    Entry::new(SERVICE, drive_id).context("creating cloud-drive keychain entry")
}

pub fn set_session(drive_id: &str, serialized_session: &str) -> Result<()> {
    entry(drive_id)?
        .set_password(serialized_session)
        .context("storing cloud-drive session in keychain")
}

pub fn get_session(drive_id: &str) -> Result<Option<String>> {
    match entry(drive_id)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("reading cloud-drive session from keychain"),
    }
}

pub fn delete_session(drive_id: &str) -> Result<()> {
    match entry(drive_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error).context("deleting cloud-drive session from keychain"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring::mock::default_credential_builder;
    use std::sync::Once;

    fn install_mock() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| keyring::set_default_credential_builder(default_credential_builder()));
    }

    #[test]
    fn session_entry_round_trips() {
        install_mock();
        set_session("test-drive-session", "{\"token\":\"test\"}").unwrap();
        assert_eq!(
            get_session("test-drive-session").unwrap().as_deref(),
            Some("{\"token\":\"test\"}")
        );
        delete_session("test-drive-session").unwrap();
        assert_eq!(get_session("test-drive-session").unwrap(), None);
    }
}
