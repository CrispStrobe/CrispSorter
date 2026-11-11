//! Persisted drive settings shared by the GUI and CLI.
use super::DriveType;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

// ── DriveConfig + Registry ────────────────────────────────────────────────

/// Serialisable config for one drive entry in the registry.
///
/// For backwards compatibility, the auth fields are `#[serde(default)]` so
/// Auth is never serialized.  Provider credentials and OAuth tokens live in
/// the OS keychain, keyed by `id`; this struct only contains non-secret drive
/// metadata and the in-memory compatibility fields used by constructors and
/// tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveConfig {
    pub id: String,
    pub label: String,
    pub kind: DriveType,
    /// For `Local` drives: the root path.
    /// For CLI-based drives: the mount-point path (if OS-mounted) or the
    /// base path used by the CLI tool.
    /// For `WebDav`: the base URL ending in `/` (e.g.
    /// `https://webdav.example.com/dav/`).
    pub path: String,
    /// In-memory compatibility field; never read from or written to JSON.
    #[serde(skip)]
    pub username: Option<String>,
    /// In-memory compatibility field; never read from or written to JSON.
    #[serde(skip)]
    pub password: Option<String>,
    /// WebDAV-only: skip TLS certificate verification.  Off by default;
    /// flip on for self-signed servers like the local one started by
    /// `internxt-cli webdav-start` (HTTPS to 127.0.0.1 with a generated
    /// cert).  Has no effect on non-WebDAV drives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insecure_tls: Option<bool>,
    /// SMB-only transport selection; credentials remain in the secret vault.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smb: Option<crisp_smb_source::Settings>,
    /// In-memory compatibility field; never read from or written to JSON.
    #[serde(skip)]
    pub access_token: Option<String>,
    /// In-memory compatibility field; never read from or written to JSON.
    #[serde(skip)]
    pub refresh_token: Option<String>,
    /// Public OAuth2 client ID; retained in memory only until keychain-backed
    /// OAuth setup is complete.
    #[serde(skip)]
    pub client_id: Option<String>,
    /// Deprecated and intentionally never accepted or persisted.  Public
    /// desktop OAuth clients must use PKCE instead of a bundled secret.
    #[serde(skip)]
    pub client_secret: Option<String>,
}

/// Loads and saves the list of configured drives.
pub struct DriveRegistry {
    path: PathBuf,
    pub drives: Vec<DriveConfig>,
}

impl DriveRegistry {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("drives.json");
        let _lock = Self::lock(&path)?;
        Self::load_locked(path)
    }

    // A separate lock file survives atomic replacement of drives.json. Fail
    // promptly when another process is saving, rather than blocking the GUI.
    fn lock(path: &Path) -> Result<std::fs::File> {
        let parent = path.parent().context("drive registry parent")?;
        std::fs::create_dir_all(parent).context("creating drive registry directory")?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(parent.join("drives.lock"))
            .context("opening drive settings lock")?;
        file.try_lock()
            .context("Drive settings are busy; retry after the other save finishes")?;
        Ok(file)
    }

    fn load_locked(path: PathBuf) -> Result<Self> {
        let mut migrated = false;
        let drives = if path.exists() {
            let json = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let mut raw: serde_json::Value = serde_json::from_str(&json)
                .with_context(|| format!("parsing {}", path.display()))?;
            // One-time migration for pre-keychain files.  Read legacy fields
            // only to move them into the OS keychain, then rewrite the file
            // through the redacted DriveConfig serializer.
            if let Some(items) = raw.as_array_mut() {
                for item in items {
                    let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let credentials = crate::drives::secret::DriveCredentials {
                        username: item
                            .get("username")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        password: item
                            .get("password")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        access_token: item
                            .get("access_token")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        refresh_token: item
                            .get("refresh_token")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                        client_id: item
                            .get("client_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_owned),
                    };
                    let had_legacy = credentials.username.is_some()
                        || credentials.password.is_some()
                        || credentials.access_token.is_some()
                        || credentials.refresh_token.is_some()
                        || credentials.client_id.is_some()
                        || item.get("client_secret").is_some();
                    if had_legacy {
                        crate::drives::secret::set_credentials(id, &credentials)
                            .context("migrating drive credentials to the OS keychain")?;
                        for key in [
                            "username",
                            "password",
                            "access_token",
                            "refresh_token",
                            "client_id",
                            "client_secret",
                        ] {
                            if let Some(object) = item.as_object_mut() {
                                object.remove(key);
                            }
                        }
                        migrated = true;
                    }
                }
            }
            let drives = serde_json::from_value::<Vec<DriveConfig>>(raw)
                .with_context(|| format!("parsing {}", path.display()))?;
            drives
        } else {
            Vec::new()
        };
        let registry = Self { path, drives };
        if migrated {
            registry.save_locked()?;
        }
        Ok(registry)
    }

    fn save_locked(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.drives).context("serialising drives")?;
        // Keep the previous registry intact if writing or flushing fails. The
        // temporary file is on the same filesystem, and persist replaces it
        // atomically on supported desktop platforms, including Windows.
        let parent = self.path.parent().context("drive registry parent")?;
        std::fs::create_dir_all(parent).context("creating drive registry directory")?;
        let mut staged =
            tempfile::NamedTempFile::new_in(parent).context("staging drive registry")?;
        staged
            .write_all(json.as_bytes())
            .context("writing drive registry")?;
        staged
            .as_file()
            .sync_all()
            .context("flushing drive registry")?;
        staged
            .persist(&self.path)
            .map_err(|e| e.error)
            .context("publishing drive registry")?;
        Ok(())
    }

    // Merge unrelated changes from other GUI/CLI instances, but refuse a
    // stale edit of this same drive. Never silently overwrite a newer setting.
    fn refreshed_locked(&self, id: &str) -> Result<Self> {
        let fresh = Self::load_locked(self.path.clone())?;
        let snapshot = serde_json::to_value(self.drives.iter().find(|d| d.id == id))?;
        let current = serde_json::to_value(fresh.drives.iter().find(|d| d.id == id))?;
        anyhow::ensure!(
            snapshot == current,
            "Drive settings changed; reload before saving"
        );
        Ok(fresh)
    }

    fn add_locked(&mut self, config: DriveConfig) -> Result<()> {
        let mut updated = self.drives.clone();
        updated.retain(|d| d.id != config.id);
        updated.push(config);
        let staged = Self {
            path: self.path.clone(),
            drives: updated,
        };
        staged.save_locked()?;
        self.drives = staged.drives;
        Ok(())
    }

    pub fn add(&mut self, config: DriveConfig) -> Result<()> {
        let _lock = Self::lock(&self.path)?;
        let mut fresh = self.refreshed_locked(&config.id)?;
        fresh.add_locked(config)?;
        self.drives = fresh.drives;
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<bool> {
        let _lock = Self::lock(&self.path)?;
        let mut fresh = self.refreshed_locked(id)?;
        let before = fresh.drives.len();
        fresh.drives.retain(|d| d.id != id);
        let removed = fresh.drives.len() < before;
        if removed {
            fresh.save_locked()?;
        }
        self.drives = fresh.drives;
        Ok(removed)
    }

    /// Shared GUI/CLI save path. Only non-secret metadata enters drives.json.
    pub fn save_smb(
        &mut self,
        mut config: DriveConfig,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<DriveConfig> {
        anyhow::ensure!(config.kind == DriveType::Smb, "Choose an SMB drive");
        let settings = config.smb.get_or_insert_with(Default::default);
        settings.validate(&config.path)?;
        let _lock = Self::lock(&self.path)?;
        let mut fresh = self.refreshed_locked(&config.id)?;
        let update_credentials = username.is_some() || password.is_some();
        let previous = if update_credentials || settings.backend == crisp_smb_source::Backend::Rust
        {
            crate::drives::secret::get_credentials(&config.id)
                .map_err(|_| anyhow::anyhow!("Cannot read SMB credentials"))?
        } else {
            None
        };
        let mut credentials = previous.clone().unwrap_or_default();
        if let Some(value) = username {
            credentials.username = Some(value);
        }
        if let Some(value) = password {
            credentials.password = Some(value);
        }
        if settings.backend == crisp_smb_source::Backend::Rust {
            // Session validation is local: no login or network connection on save.
            let cfg = settings.config(
                &config.path,
                credentials.username.as_deref().unwrap_or(""),
                credentials.password.as_deref().unwrap_or(""),
            )?;
            let _ = crisp_smb_source::Session::new(cfg)?;
        }
        if update_credentials {
            crate::drives::secret::set_credentials(&config.id, &credentials)
                .map_err(|_| anyhow::anyhow!("Cannot save SMB credentials"))?;
        }
        if let Err(error) = fresh.add_locked(config.clone()) {
            if update_credentials {
                let rollback = if let Some(old) = previous {
                    crate::drives::secret::set_credentials(&config.id, &old)
                } else {
                    crate::drives::secret::delete_credentials(&config.id)
                };
                if rollback.is_err() {
                    crate::app_log(
                        "error",
                        "SMB credential rollback failed after settings save failure".into(),
                    );
                }
            }
            return Err(error);
        }
        self.drives = fresh.drives;
        crate::app_log(
            "info",
            format!(
                "SMB settings saved: backend={}",
                config.smb.as_ref().unwrap().backend.name()
            ),
        );
        Ok(config)
    }
}
