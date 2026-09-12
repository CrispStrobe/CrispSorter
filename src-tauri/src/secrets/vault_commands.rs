//! Tauri surface for choosing where secrets live.
//!
//! The Settings pane uses these to render the picker, switch vaults,
//! unlock a passphrase vault, and move existing secrets across. Nothing
//! here returns a secret *value* — the only command that reads one is
//! `secret_get`, which is unchanged and goes through whichever vault
//! these commands selected.

use super::vault::{
    self, BackendChoice, FileKeySource, KeychainInfo, MacUnlock, Status,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Everything the picker needs in one round trip.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendOptions {
    /// The vault in force, and why.
    pub status: Status,
    /// `~/Library/Keychains` contents. Empty off macOS.
    pub keychains: Vec<KeychainInfo>,
    /// Where "create a keychain for CrispSorter" would put one.
    pub suggested_keychain: Option<PathBuf>,
    /// `true` where named keychains are a real option at all.
    pub supports_named_keychains: bool,
    /// The services the migration helper knows about.
    pub services: Vec<String>,
}

#[tauri::command]
pub async fn secret_backend_options() -> Result<BackendOptions, String> {
    Ok(BackendOptions {
        status: vault::status(),
        keychains: vault::list_keychains(),
        suggested_keychain: vault::suggested_keychain_path(),
        supports_named_keychains: cfg!(target_os = "macos"),
        services: vault::SERVICES.iter().map(|s| s.to_string()).collect(),
    })
}

#[tauri::command]
pub async fn secret_backend_status() -> Result<Status, String> {
    Ok(vault::status())
}

/// Switch vaults and remember the decision.
///
/// Takes effect immediately — every secret module resolves the active
/// vault per call rather than caching a handle — so the UI does not need
/// to ask for a restart. Secrets already in the old vault stay there;
/// see [`secret_backend_migrate`].
#[tauri::command]
pub async fn secret_backend_select(choice: SimpleChoice) -> Result<Status, String> {
    vault::select(choice.into_choice()?).map_err(|e| e.to_string())?;
    Ok(vault::status())
}

/// Unlock a passphrase vault for the rest of this run.
#[tauri::command]
pub async fn secret_backend_unlock(passphrase: String) -> Result<Status, String> {
    vault::unlock(&passphrase).map_err(|e| e.to_string())?;
    Ok(vault::status())
}

/// Lower the "the user already said no" latch, so the next read is
/// allowed to raise a system dialog again.
#[tauri::command]
pub async fn secret_backend_retry() -> Result<Status, String> {
    vault::clear_denied();
    Ok(vault::status())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    pub copied: usize,
    /// Entries the source vault simply did not have.
    pub skipped: usize,
    /// `[service, account, reason]` per entry that could not be moved.
    pub failures: Vec<(String, String, String)>,
}

/// Copy `accounts` out of the vault `from` names and into the active one.
///
/// The candidate list comes from the caller because no credential store
/// offers a reliable "list everything under this service" — on macOS an
/// enumeration would raise one dialog per row, which is the problem this
/// whole feature exists to end.
#[tauri::command]
pub async fn secret_backend_migrate(
    from: SimpleChoice,
    accounts: Vec<(String, String)>,
) -> Result<MigrationReport, String> {
    let from = from.into_choice()?;
    let (copied, skipped, failures) =
        vault::migrate(&from, &accounts).map_err(|e| e.to_string())?;
    Ok(MigrationReport {
        copied,
        skipped,
        failures,
    })
}

/// What [`secret_backend_create_keychain`] gives back.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedKeychain {
    pub path: PathBuf,
    /// The keychain's password. Shown to the user **once**, so they can
    /// write it down: macOS may ask for it later (an item's access list
    /// is per-binary, so a rebuilt or re-signed app is asked to prove
    /// itself), and a password nobody knows is a keychain nobody can
    /// answer for.
    pub password: String,
    /// `true` when CrispSorter generated the password rather than taking
    /// one from the user.
    pub generated: bool,
}

/// Create a keychain for CrispSorter to use.
#[tauri::command]
#[allow(unused_variables)]
pub async fn secret_backend_create_keychain(
    path: Option<PathBuf>,
    password: Option<String>,
) -> Result<CreatedKeychain, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Err("named keychains exist only on macOS".to_string())
    }
    #[cfg(target_os = "macos")]
    {
        use super::vault::mac_keychain;
        let path = path
            .or_else(vault::suggested_keychain_path)
            .ok_or_else(|| "could not work out where to put the keychain".to_string())?;
        let data_dir = vault::default_app_data_dir();
        let generated = password.as_deref().unwrap_or_default().is_empty();
        let pw = mac_keychain::create_keychain(&path, &data_dir, password.as_deref())
            .map_err(|e| e.to_string())?;
        Ok(CreatedKeychain {
            path,
            password: pw,
            generated,
        })
    }
}

/// The stored password of an app-managed keychain, so the UI can show it
/// when macOS asks the user for it.
#[tauri::command]
#[allow(unused_variables)]
pub async fn secret_backend_keychain_password(path: PathBuf) -> Result<Option<String>, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
    #[cfg(target_os = "macos")]
    {
        Ok(super::vault::mac_keychain::stored_password(
            &vault::default_app_data_dir(),
            &path,
        ))
    }
}

/// Convenience shapes so the frontend can post a plain string instead of
/// hand-assembling the tagged enum.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimpleChoice {
    pub kind: String,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub unlock: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

impl SimpleChoice {
    pub fn into_choice(self) -> Result<BackendChoice, String> {
        match self.kind.as_str() {
            "os-default" => Ok(BackendChoice::OsDefault),
            "session" => Ok(BackendChoice::Session),
            "file" => Ok(BackendChoice::File {
                key: match self.key.as_deref() {
                    Some("passphrase") => FileKeySource::Passphrase,
                    _ => FileKeySource::Device,
                },
            }),
            "mac-keychain" => Ok(BackendChoice::MacKeychain {
                path: self
                    .path
                    .ok_or_else(|| "a named keychain needs a path".to_string())?,
                unlock: match self.unlock.as_deref() {
                    Some("prompt") => MacUnlock::Prompt,
                    _ => MacUnlock::AppManaged,
                },
            }),
            other => Err(format!("unknown secret store {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_choices_map_onto_the_tagged_enum() {
        let f = SimpleChoice {
            kind: "file".into(),
            path: None,
            unlock: None,
            key: Some("passphrase".into()),
        };
        assert_eq!(
            f.into_choice().unwrap(),
            BackendChoice::File {
                key: FileKeySource::Passphrase
            }
        );
    }

    #[test]
    fn a_named_keychain_without_a_path_is_rejected() {
        let c = SimpleChoice {
            kind: "mac-keychain".into(),
            path: None,
            unlock: None,
            key: None,
        };
        assert!(c.into_choice().is_err());
    }

    #[test]
    fn an_unknown_store_is_rejected_rather_than_defaulted() {
        // Silently falling back to the OS keychain would put a user who
        // asked to leave it right back on it.
        let c = SimpleChoice {
            kind: "nope".into(),
            path: None,
            unlock: None,
            key: None,
        };
        assert!(c.into_choice().is_err());
    }
}
