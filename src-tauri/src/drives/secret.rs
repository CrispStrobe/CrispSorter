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
    use std::collections::HashMap;
    use std::sync::{Once, OnceLock};

    pub(crate) fn install_mock_for_tests() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| keyring::set_default_credential_builder(shared_store_builder()));
    }

    // ── A test keychain that survives across `Entry` values ──────────────────
    //
    // `keyring::mock` keeps the secret *inside* the `Credential` object, so two
    // `Entry::new(service, id)` calls for the same id get two unrelated stores.
    // Every public function here builds its own `Entry` — `set_credentials`
    // stores through one, `get_credentials` reads through another — so against
    // the stock mock a round-trip through the public API always reads back
    // `None`, no matter how correct the code is.
    //
    // `public_credential_and_session_api_round_trips_and_deletes` asserted
    // exactly that round-trip and had therefore never passed; it went unnoticed
    // because the lib test target did not compile (see docs/ai-act.md § 5).
    // Marking it `#[ignore]` would have made the suite green while deleting the
    // only coverage of the API the app actually calls, so instead the store is
    // keyed by (target, service, user) and shared between credentials — which is
    // how a real keychain behaves, and is what the assertions were written for.
    #[derive(Debug)]
    struct SharedStoreCredential {
        key: (String, String, String),
    }

    fn store() -> &'static std::sync::Mutex<HashMap<(String, String, String), Vec<u8>>> {
        static STORE: OnceLock<std::sync::Mutex<HashMap<(String, String, String), Vec<u8>>>> =
            OnceLock::new();
        STORE.get_or_init(Default::default)
    }

    impl keyring::credential::CredentialApi for SharedStoreCredential {
        fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
            store().lock().unwrap().insert(self.key.clone(), secret.to_vec());
            Ok(())
        }

        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            store()
                .lock()
                .unwrap()
                .get(&self.key)
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }

        fn delete_credential(&self) -> keyring::Result<()> {
            match store().lock().unwrap().remove(&self.key) {
                Some(_) => Ok(()),
                None => Err(keyring::Error::NoEntry),
            }
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[derive(Debug)]
    struct SharedStoreBuilder;

    impl keyring::credential::CredentialBuilderApi for SharedStoreBuilder {
        fn build(
            &self,
            target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<keyring::credential::Credential>> {
            Ok(Box::new(SharedStoreCredential {
                key: (
                    target.unwrap_or_default().to_owned(),
                    service.to_owned(),
                    user.to_owned(),
                ),
            }))
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn shared_store_builder() -> Box<keyring::credential::CredentialBuilder> {
        Box::new(SharedStoreBuilder)
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
        // Goes through one entry on purpose: this test is about the exact
        // serialized payload on the wire to the keychain, not about persistence.
        // (The round-trip across separate entries is covered by
        // `public_credential_and_session_api_round_trips_and_deletes`, which the
        // shared-store test backend above finally makes possible.)
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
