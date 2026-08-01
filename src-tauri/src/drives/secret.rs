//! OS-keychain storage for native cloud-drive sessions.
//!
//! A native Internxt session contains more than a bearer token: the mnemonic,
//! bucket id, and bridge credentials are required to decrypt and transfer
//! files. None of that belongs in `drives.json` or a synced settings file.

use anyhow::{Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

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

#[cfg(test)]
fn test_store() -> &'static Mutex<HashMap<(String, String), String>> {
    static STORE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn credentials_entry(drive_id: &str) -> Result<Entry> {
    Entry::new(CREDENTIALS_SERVICE, drive_id)
        .context("creating cloud-drive credentials keychain entry")
}

pub fn set_credentials(drive_id: &str, credentials: &DriveCredentials) -> Result<()> {
    let serialized =
        serde_json::to_string(credentials).context("serializing cloud-drive credentials")?;
    #[cfg(test)]
    {
        test_store()
            .lock()
            .expect("test keychain store")
            .insert((CREDENTIALS_SERVICE.into(), drive_id.into()), serialized);
        return Ok(());
    }
    #[cfg(not(test))]
    credentials_entry(drive_id)?
        .set_password(&serialized)
        .context("storing cloud-drive credentials in keychain")
}

pub fn get_credentials(drive_id: &str) -> Result<Option<DriveCredentials>> {
    #[cfg(test)]
    if let Some(value) = test_store()
        .lock()
        .expect("test keychain store")
        .get(&(CREDENTIALS_SERVICE.into(), drive_id.into()))
        .cloned()
    {
        return serde_json::from_str(&value)
            .context("parsing cloud-drive credentials from keychain")
            .map(Some);
    }
    #[cfg(test)]
    return Ok(None);
    #[cfg(not(test))]
    match credentials_entry(drive_id)?.get_password() {
        Ok(value) => serde_json::from_str(&value)
            .context("parsing cloud-drive credentials from keychain")
            .map(Some),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("reading cloud-drive credentials from keychain"),
    }
}

pub fn delete_credentials(drive_id: &str) -> Result<()> {
    #[cfg(test)]
    {
        test_store()
            .lock()
            .expect("test keychain store")
            .remove(&(CREDENTIALS_SERVICE.into(), drive_id.into()));
        return Ok(());
    }
    #[cfg(not(test))]
    match credentials_entry(drive_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error).context("deleting cloud-drive credentials from keychain"),
    }
}

pub fn set_session(drive_id: &str, serialized_session: &str) -> Result<()> {
    #[cfg(test)]
    {
        test_store()
            .lock()
            .expect("test keychain store")
            .insert((SERVICE.into(), drive_id.into()), serialized_session.into());
        return Ok(());
    }
    #[cfg(not(test))]
    entry(drive_id)?
        .set_password(serialized_session)
        .context("storing cloud-drive session in keychain")
}

pub fn get_session(drive_id: &str) -> Result<Option<String>> {
    #[cfg(test)]
    return Ok(test_store()
        .lock()
        .expect("test keychain store")
        .get(&(SERVICE.into(), drive_id.into()))
        .cloned());
    #[cfg(not(test))]
    match entry(drive_id)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("reading cloud-drive session from keychain"),
    }
}

pub fn delete_session(drive_id: &str) -> Result<()> {
    #[cfg(test)]
    {
        test_store()
            .lock()
            .expect("test keychain store")
            .remove(&(SERVICE.into(), drive_id.into()));
        return Ok(());
    }
    #[cfg(not(test))]
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
    fn provider_credentials_payload_round_trip_without_plaintext_config() {
        install_mock_for_tests();
        let id = "test-drive-credentials";
        let credentials = DriveCredentials {
            username: Some("alice".into()),
            password: Some("not-for-config".into()),
            access_token: Some("access".into()),
            refresh_token: Some("refresh".into()),
            client_id: Some("public-client".into()),
        };
        // keyring::mock is intentionally EntryOnly: it cannot model the
        // persistence boundary between the separate Entry values used by
        // set_credentials/get_credentials. Test the exact serialized payload
        // through one entry; production persistence is supplied by the OS
        // keychain backend.
        let serialized = serde_json::to_string(&credentials).unwrap();
        let stored = credentials_entry(id).unwrap();
        stored.set_password(&serialized).unwrap();
        let loaded: DriveCredentials =
            serde_json::from_str(&stored.get_password().unwrap()).unwrap();
        assert_eq!(loaded, credentials);
        stored.delete_credential().unwrap();
        assert!(matches!(stored.get_password(), Err(keyring::Error::NoEntry)));
    }

    #[test]
    fn public_credential_and_session_api_round_trips_and_deletes() {
        install_mock_for_tests();
        let id = "test-public-secret-api";
        let credentials = DriveCredentials {
            username: Some("user".into()), password: Some("secret".into()),
            access_token: Some("access".into()), refresh_token: Some("refresh".into()),
            client_id: Some("public".into()),
        };
        set_credentials(id, &credentials).unwrap();
        assert_eq!(get_credentials(id).unwrap(), Some(credentials));
        delete_credentials(id).unwrap();
        assert_eq!(get_credentials(id).unwrap(), None);

        set_session(id, "encrypted-session").unwrap();
        assert_eq!(get_session(id).unwrap().as_deref(), Some("encrypted-session"));
        delete_session(id).unwrap();
        assert_eq!(get_session(id).unwrap(), None);
    }
}
