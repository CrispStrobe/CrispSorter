//! OS-keychain storage for native cloud-drive sessions.
//!
//! A native Internxt session contains more than a bearer token: the mnemonic,
//! bucket id, and bridge credentials are required to decrypt and transfer
//! files. None of that belongs in `drives.json` or a synced settings file.

use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};

const SERVICE: &str = "CrispSorter.CloudDrive";
const CREDENTIALS_SERVICE: &str = "CrispSorter.CloudDrive.Auth";
// Unit tests use an isolated memory store; production always uses the OS keychain.

fn entry(drive_id: &str) -> Result<Entry> {
    Entry::new(SERVICE, drive_id).context("creating cloud-drive keychain entry")
}

/// Secrets used by a provider connection.  This is deliberately separate
/// from `DriveConfig`: neither access tokens nor WebDAV passwords belong in
/// `drives.json`, the frontend settings store, or a synced app config.
/// OAuth client IDs are public identifiers and may be stored here, but client
/// secrets are intentionally not represented by this type at all.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriveCredentials {
    pub username: Option<String>,
    pub password: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub client_id: Option<String>,
}

fn credentials_entry(drive_id: &str) -> Result<Entry> {
    Entry::new(CREDENTIALS_SERVICE, drive_id)
        .context("creating cloud-drive credentials keychain entry")
}

pub fn set_credentials(drive_id: &str, credentials: &DriveCredentials) -> Result<()> {
    let serialized =
        serde_json::to_string(credentials).context("serializing cloud-drive credentials")?;
    credentials_entry(drive_id)?
        .set_password(&serialized)
        .context("storing cloud-drive credentials in keychain")
}

pub fn get_credentials(drive_id: &str) -> Result<Option<DriveCredentials>> {
    match credentials_entry(drive_id)?.get_password() {
        Ok(value) => serde_json::from_str(&value)
            .context("parsing cloud-drive credentials from keychain")
            .map(Some),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("reading cloud-drive credentials from keychain"),
    }
}

pub fn delete_credentials(drive_id: &str) -> Result<()> {
    match credentials_entry(drive_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error).context("deleting cloud-drive credentials from keychain"),
    }
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
pub(crate) fn install_mock_for_tests() {
    tests::install_mock_for_tests();
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring::mock::default_credential_builder;
    use std::sync::Once;

    pub(crate) fn install_mock_for_tests() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| keyring::set_default_credential_builder(default_credential_builder()));
    }

    #[test]
    fn session_entry_round_trips() {
        install_mock_for_tests();
        let session = entry("test-drive-session").unwrap();
        session.set_password("{\"token\":\"test\"}").unwrap();
        assert_eq!(
            session.get_password().unwrap().as_str(),
            "{\"token\":\"test\"}"
        );
        session.delete_credential().unwrap();
        assert!(matches!(
            session.get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }

    #[test]
    fn provider_credentials_round_trip_without_plaintext_config() {
        install_mock_for_tests();
        let id = "test-drive-credentials";
        let credentials = DriveCredentials {
            username: Some("alice".into()),
            password: Some("not-for-config".into()),
            access_token: Some("access".into()),
            refresh_token: Some("refresh".into()),
            client_id: Some("public-client".into()),
        };
        set_credentials(id, &credentials).unwrap();
        assert_eq!(get_credentials(id).unwrap(), Some(credentials));
        delete_credentials(id).unwrap();
        assert_eq!(get_credentials(id).unwrap(), None);
    }
}
