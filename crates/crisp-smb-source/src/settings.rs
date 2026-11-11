use crate::{Config, Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Mounted,
    Rust,
}
impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Mounted => "mounted",
            Self::Rust => "rust",
        }
    }
}
fn encryption_default() -> bool {
    true
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub backend: Backend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_path: Option<String>,
    #[serde(default = "encryption_default")]
    pub require_encryption: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            backend: Backend::Mounted,
            mount_path: None,
            require_encryption: true,
        }
    }
}
/// Both surfaces validate this before persisting; no network or credential IO.
impl Settings {
    pub fn validate(&self, url: &str) -> Result<()> {
        parse_url(url)?;
        if self.backend == Backend::Mounted
            && !self
                .mount_path
                .as_deref()
                .is_some_and(|p| !p.contains('\0') && Path::new(p).is_absolute())
        {
            return Err(Error::new("MountPathRequired"));
        }
        Ok(())
    }
    pub fn config(&self, url: &str, username: &str, password: &str) -> Result<Config> {
        self.validate(url)?;
        let (address, share, root) = parse_url(url)?;
        let (domain, username) = username.split_once('\\').unwrap_or(("", username));
        Ok(Config {
            address,
            share,
            root,
            username: username.into(),
            password: password.into(),
            domain: domain.into(),
            require_encryption: self.require_encryption,
        })
    }
}
fn parse_url(text: &str) -> Result<(String, String, String)> {
    let bad = || Error::new("InvalidUrl");
    // Reject traversal before URL's dot-segment normalization can hide it.
    let raw = text.strip_prefix("smb://").ok_or_else(bad)?;
    let raw_path = raw.split_once('/').ok_or_else(bad)?.1;
    for segment in raw_path.split('/') {
        let decoded = percent_encoding::percent_decode_str(segment)
            .decode_utf8()
            .map_err(|_| bad())?;
        if decoded == "." || decoded == ".." || decoded.contains(['/', '\\', '\0']) {
            return Err(bad());
        }
    }
    let url = url::Url::parse(text).map_err(|_| bad())?;
    if url.scheme() != "smb"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(bad());
    }
    let host = url.host_str().filter(|s| !s.is_empty()).ok_or_else(bad)?;
    let address = format!("{}:{}", host, url.port().unwrap_or(445));
    let parts: Vec<_> = raw_path.trim_end_matches('/').split('/').collect();
    if parts.is_empty() || parts.iter().any(|s| s.is_empty()) {
        return Err(bad());
    }
    let decoded: Vec<String> = parts
        .iter()
        .map(|p| {
            percent_encoding::percent_decode_str(p)
                .decode_utf8()
                .map(|x| x.into_owned())
                .map_err(|_| bad())
        })
        .collect::<Result<_>>()?;
    Ok((address, decoded[0].clone(), decoded[1..].join("/")))
}
/// CloudDrive paths are relative to the selected root. '/' is the root on both
/// surfaces, never the host's filesystem root. Backslashes are separators only
/// on Windows, where Path::join produces them; UNC/prefix paths remain invalid.
pub fn relative_path(path: &Path) -> Result<String> {
    let raw = path.to_str().ok_or_else(|| Error::new("InvalidPath"))?;
    #[cfg(windows)]
    let raw = raw.replace('\\', "/");
    let text = raw.trim_start_matches('/');
    if raw.starts_with("//")
        || text.contains(['\\', '\0', ':'])
        || text.split('/').any(|x| x == ".." || x == ".")
    {
        return Err(Error::new("InvalidPath"));
    }
    Ok(text.trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_roundtrip_defaults_and_strict_choices() {
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
        for backend in [Backend::Mounted, Backend::Rust] {
            let s = Settings {
                backend,
                mount_path: Some("/mnt/books".into()),
                require_encryption: true,
            };
            assert_eq!(
                serde_json::from_str::<Settings>(&serde_json::to_string(&s).unwrap()).unwrap(),
                s
            );
        }
        assert!(serde_json::from_str::<Settings>(r#"{"backend":"auto"}"#).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"password":"secret"}"#).is_err());
    }
    #[test]
    fn url_and_mount_validation_never_echo_secrets() {
        let s = Settings {
            backend: Backend::Rust,
            ..Default::default()
        };
        for url in [
            "smb://user:secret@nas/share",
            "smb://nas/share/../x",
            "smb://nas/share/%2e%2e/x",
            "smb://nas/share/a%2fb",
            "smb://nas/share?secret=x",
            "smb://nas",
            "smb://nas//x",
        ] {
            let error = s.validate(url).unwrap_err().to_string();
            assert!(!error.contains("secret"));
        }
        let c = s
            .config(
                "smb://nas:1445/books/Audio%20Books",
                "DOMAIN\\reader",
                "secret",
            )
            .unwrap();
        assert_eq!(
            (
                c.address.as_str(),
                c.share.as_str(),
                c.root.as_str(),
                c.domain.as_str()
            ),
            ("nas:1445", "books", "Audio Books", "DOMAIN")
        );
        assert!(c.require_encryption);
        assert!(Settings::default().validate("smb://nas/books").is_err());
    }
    #[test]
    fn both_surfaces_use_drive_relative_paths() {
        for (input, expected) in [
            ("/", ""),
            ("", ""),
            ("/books/a", "books/a"),
            ("books/", "books"),
        ] {
            assert_eq!(relative_path(Path::new(input)).unwrap(), expected);
        }
        for input in ["../a", "/a/../b", "C:/a", "//nas/books", "a\0b"] {
            assert!(relative_path(Path::new(input)).is_err());
        }
    }
}
