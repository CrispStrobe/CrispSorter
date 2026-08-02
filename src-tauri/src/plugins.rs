//! Validated manifest primitives for runtime remote-provider extensions.
//!
//! This is deliberately only the manifest boundary. A generic request command
//! must not be added until installation has persisted an explicit host
//! allowlist and user consent.

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::IpAddr;
use reqwest::Url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginAuthKind {
    None,
    Bearer,
    OAuthPkce,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginTab {
    pub id: String,
    pub label: String,
    pub component: String,
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteProviderManifest {
    pub id: String,
    pub display_name: String,
    pub base_url: String,
    pub auth: PluginAuthKind,
    pub capabilities: Vec<String>,
    pub tabs: Vec<PluginTab>,
}

impl RemoteProviderManifest {
    /// Validate the install-time security boundary before persistence/use.
    pub fn validate(&self) -> Result<Url> {
        validate_identifier("plugin id", &self.id)?;
        ensure!(
            !self.display_name.trim().is_empty(),
            "plugin display name is empty"
        );
        let url = Url::parse(&self.base_url)
            .map_err(|e| anyhow::anyhow!("invalid plugin base URL: {e}"))?;
        ensure!(url.scheme() == "https", "plugin base URL must use HTTPS");
        ensure!(
            url.username().is_empty() && url.password().is_none(),
            "plugin URL must not contain credentials"
        );
        ensure!(
            url.query().is_none() && url.fragment().is_none(),
            "plugin base URL must not contain query or fragment"
        );
        let host = url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("plugin base URL has no host"))?;
        ensure!(
            !is_blocked_host(host),
            "plugin base URL host is local or private"
        );
        ensure!(
            !self.capabilities.is_empty(),
            "plugin must advertise at least one capability"
        );
        ensure_unique("capability", self.capabilities.iter().map(String::as_str))?;
        ensure_unique("tab", self.tabs.iter().map(|tab| tab.id.as_str()))?;
        for tab in &self.tabs {
            validate_identifier("tab id", &tab.id)?;
            ensure!(!tab.label.trim().is_empty(), "plugin tab label is empty");
            ensure!(
                !tab.component.trim().is_empty(),
                "plugin tab component is empty"
            );
            for required in &tab.requires {
                ensure!(
                    self.capabilities.iter().any(|cap| cap == required),
                    "tab requires unadvertised capability {required:?}"
                );
            }
        }
        Ok(url)
    }
}

fn validate_identifier(kind: &str, value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 64,
        "{kind} must be 1-64 characters"
    );
    ensure!(
        value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'),
        "{kind} contains invalid characters"
    );
    Ok(())
}

fn ensure_unique<'a>(kind: &str, values: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = HashSet::new();
    for value in values {
        ensure!(seen.insert(value), "duplicate {kind} id {value:?}");
    }
    Ok(())
}

fn is_blocked_host(host: &str) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    if normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
    {
        return true;
    }
    let Ok(ip) = normalized.parse::<IpAddr>() else {
        return false;
    };
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest(base_url: &str) -> RemoteProviderManifest {
        RemoteProviderManifest {
            id: "example.provider".into(),
            display_name: "Example Provider".into(),
            base_url: base_url.into(),
            auth: PluginAuthKind::Bearer,
            capabilities: vec!["list".into(), "read".into()],
            tabs: vec![PluginTab {
                id: "browser".into(),
                label: "Browser".into(),
                component: "ProviderBrowser".into(),
                requires: vec!["list".into()],
            }],
        }
    }
    #[test]
    fn accepts_public_https_manifest() {
        assert_eq!(
            manifest("https://files.example.test/api")
                .validate()
                .unwrap()
                .host_str(),
            Some("files.example.test")
        );
    }
    #[test]
    fn rejects_non_https_and_credentials() {
        for url in [
            "http://files.example.test",
            "https://user:pw@files.example.test",
        ] {
            assert!(manifest(url).validate().is_err());
        }
    }
    #[test]
    fn rejects_local_and_private_hosts() {
        for url in [
            "https://localhost",
            "https://127.0.0.1",
            "https://10.0.0.8",
            "https://service.local",
        ] {
            assert!(manifest(url).validate().is_err());
        }
    }
    #[test]
    fn rejects_duplicate_capabilities_and_tabs() {
        let mut c = manifest("https://files.example.test");
        c.capabilities.push("list".into());
        assert!(c.validate().is_err());
        let mut t = manifest("https://files.example.test");
        t.tabs.push(t.tabs[0].clone());
        assert!(t.validate().is_err());
    }
    #[test]
    fn rejects_tabs_that_escape_advertised_capabilities() {
        let mut value = manifest("https://files.example.test");
        value.tabs[0].requires = vec!["write".into()];
        assert!(value.validate().is_err());
    }
    #[test]
    fn serde_round_trip_preserves_auth_kind() {
        let value = manifest("https://files.example.test");
        let back: RemoteProviderManifest =
            serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        assert_eq!(back.auth, PluginAuthKind::Bearer);
    }
}
