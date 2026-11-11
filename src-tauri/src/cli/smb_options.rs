//! SMB command input used by the real CLI and tested without Tauri startup.
use crate::drives::{DriveConfig, DriveRegistry, DriveType};
use clap::Args;
use crisp_smb_source::{Backend, Settings};
use std::io::Read;

#[derive(Args, Debug)]
pub struct SmbOptions {
    /// Update/show this existing drive. Omit to create a drive.
    #[arg(long)]
    pub id: Option<String>,
    #[arg(long)]
    pub label: Option<String>,
    /// SMB URL without credentials: smb://host/share/optional-root
    #[arg(long)]
    pub url: Option<String>,
    #[arg(long, value_parser = ["mounted", "rust"])]
    pub backend: Option<String>,
    /// Existing OS mount of the same share/root (mounted path only).
    #[arg(long)]
    pub mount_path: Option<String>,
    /// Direct Rust path: true by default. OS mounts use OS security policy.
    #[arg(long, action = clap::ArgAction::Set)]
    pub require_encryption: Option<bool>,
    #[arg(long)]
    pub username: Option<String>,
    /// Read one password line from stdin, never from a command-line argument.
    #[arg(long)]
    pub password_stdin: bool,
}
impl SmbOptions {
    pub fn apply(
        self,
        registry: &mut DriveRegistry,
        stdin: &mut impl Read,
    ) -> Result<DriveConfig, String> {
        let changing = self.label.is_some()
            || self.url.is_some()
            || self.backend.is_some()
            || self.mount_path.is_some()
            || self.require_encryption.is_some()
            || self.username.is_some()
            || self.password_stdin;
        let mut config = if let Some(id) = self.id {
            let config = registry
                .drives
                .iter()
                .find(|d| d.id == id)
                .filter(|d| d.kind == DriveType::Smb)
                .ok_or("SMB drive was not found")?
                .clone();
            if !changing {
                return Ok(config);
            }
            config
        } else {
            DriveConfig {
                id: uuid::Uuid::new_v4().to_string(),
                label: String::new(),
                kind: DriveType::Smb,
                path: String::new(),
                smb: Some(Settings::default()),
                insecure_tls: None,
                username: None,
                password: None,
                access_token: None,
                refresh_token: None,
                client_id: None,
                client_secret: None,
            }
        };
        if let Some(label) = self.label {
            config.label = label;
        }
        if let Some(url) = self.url {
            config.path = url;
        }
        if config.label.trim().is_empty() || config.path.trim().is_empty() {
            return Err("SMB label and URL are required".into());
        }
        let settings = config.smb.get_or_insert_with(Default::default);
        if let Some(backend) = self.backend {
            settings.backend = match backend.as_str() {
                "mounted" => Backend::Mounted,
                "rust" => Backend::Rust,
                _ => return Err("SMB backend must be mounted or rust".into()),
            };
        }
        if let Some(path) = self.mount_path {
            settings.mount_path = Some(path);
        }
        if let Some(required) = self.require_encryption {
            settings.require_encryption = required;
        }
        settings.validate(&config.path).map_err(|e| e.to_string())?;
        let password = if self.password_stdin {
            let mut bytes = Vec::new();
            stdin
                .take(8193)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read SMB password")?;
            if bytes.len() > 8192 {
                return Err("SMB password input is too long".into());
            }
            let mut text = String::from_utf8(bytes).map_err(|_| "SMB password must be UTF-8")?;
            if text.ends_with('\n') {
                text.pop();
                if text.ends_with('\r') {
                    text.pop();
                }
            }
            if text.contains(['\n', '\r', '\0']) || text.is_empty() {
                return Err("Supply one non-empty password line".into());
            }
            Some(text)
        } else {
            None
        };
        registry
            .save_smb(config, self.username, password)
            .map_err(|e| e.to_string())
    }
}
