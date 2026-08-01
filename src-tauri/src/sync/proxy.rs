//! P29.8 — HTTP / SOCKS5 proxy support.
//!
//! Builds a `reqwest::Client` (or `reqwest::blocking::Client`) with
//! proxy configuration.  Supports `http://`, `https://`, `socks5://`,
//! and `socks5h://` URL schemes.
//!
//! Explicit config takes priority over `HTTP_PROXY` / `HTTPS_PROXY` /
//! `NO_PROXY` env vars.  When no explicit config is given, `reqwest`
//! respects the env vars automatically.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ── Configuration ────────────────────────────────────────────────────────

/// Proxy configuration stored in IndexConfig / settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Proxy URL (e.g. `http://proxy.corp:8080`, `socks5://127.0.0.1:1080`).
    /// When `None`, no explicit proxy is set (env vars may still apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Optional basic-auth username for the proxy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// Optional basic-auth password for the proxy.
    /// Should be stored in the OS keychain, not in plaintext config.
    #[serde(default, skip_serializing, skip_deserializing)]
    pub password: Option<String>,
}

impl ProxyConfig {
    /// Returns `true` when no proxy is configured.
    pub fn is_empty(&self) -> bool {
        self.url.is_none()
    }

    pub fn validate(&self) -> Result<()> {
        if self.url.is_none() && (self.username.is_some() || self.password.is_some()) {
            anyhow::bail!("proxy credentials require a proxy URL");
        }
        Ok(())
    }
}

// ── Client builders ──────────────────────────────────────────────────────

/// Build an async `reqwest::Client` with the given proxy config.
///
/// If `config` has no proxy URL, returns a default client (which still
/// honours `HTTP_PROXY` / `HTTPS_PROXY` env vars via reqwest's built-in
/// support).
pub fn build_async_client(config: &ProxyConfig) -> Result<reqwest::Client> {
    config.validate()?;
    configure_async_builder(config)?.build().context("building proxied async client")
}

/// Build an async client with an explicit request timeout and proxy policy.
pub fn build_async_client_with_timeout(
    config: &ProxyConfig,
    timeout: std::time::Duration,
) -> Result<reqwest::Client> {
    config.validate()?;
    configure_async_builder(config)?
        .timeout(timeout)
        .build()
        .context("building proxied async client")
}

fn configure_async_builder(config: &ProxyConfig) -> Result<reqwest::ClientBuilder> {
    let mut builder = reqwest::ClientBuilder::new();

    if let Some(url) = &config.url {
        let mut proxy =
            reqwest::Proxy::all(url).with_context(|| format!("invalid proxy URL: {url}"))?;
        if let (Some(user), Some(pass)) = (&config.username, &config.password) {
            proxy = proxy.basic_auth(user, pass);
        }
        builder = builder.proxy(proxy);
    }

    Ok(builder)
}

/// Build a blocking `reqwest::blocking::Client` with the given proxy config.
///
/// Used by `CloudDrive` implementations which are synchronous.
pub fn build_blocking_client(config: &ProxyConfig) -> Result<reqwest::blocking::Client> {
    config.validate()?;
    let mut builder = reqwest::blocking::ClientBuilder::new();

    if let Some(url) = &config.url {
        let mut proxy =
            reqwest::Proxy::all(url).with_context(|| format!("invalid proxy URL: {url}"))?;
        if let (Some(user), Some(pass)) = (&config.username, &config.password) {
            proxy = proxy.basic_auth(user, pass);
        }
        builder = builder.proxy(proxy);
    }

    builder.build().context("building proxied blocking client")
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_builds_default_client() {
        let cfg = ProxyConfig::default();
        assert!(cfg.is_empty());
        let client = build_blocking_client(&cfg);
        assert!(client.is_ok());
    }

    #[test]
    fn http_proxy_builds_successfully() {
        let cfg = ProxyConfig {
            url: Some("http://proxy.example.com:8080".into()),
            username: None,
            password: None,
        };
        assert!(!cfg.is_empty());
        let client = build_blocking_client(&cfg);
        assert!(client.is_ok());
    }

    #[test]
    fn socks5_proxy_builds_successfully() {
        let cfg = ProxyConfig {
            url: Some("socks5://127.0.0.1:1080".into()),
            username: None,
            password: None,
        };
        let client = build_blocking_client(&cfg);
        assert!(client.is_ok());
    }

    #[test]
    fn proxy_with_auth_builds_successfully() {
        let cfg = ProxyConfig {
            url: Some("http://proxy.corp:3128".into()),
            username: Some("user".into()),
            password: Some("pass".into()),
        };
        let client = build_blocking_client(&cfg);
        assert!(client.is_ok());
    }

    #[test]
    fn invalid_proxy_url_errors() {
        let cfg = ProxyConfig {
            url: Some("not a valid url at all".into()),
            username: None,
            password: None,
        };
        let result = build_blocking_client(&cfg);
        assert!(result.is_err());
    }

    #[test]
    fn async_client_builds_with_proxy() {
        let cfg = ProxyConfig {
            url: Some("http://proxy.example.com:8080".into()),
            username: Some("u".into()),
            password: Some("p".into()),
        };
        let client = build_async_client(&cfg);
        assert!(client.is_ok());
    }

    #[test]
    fn async_client_with_timeout_builds_with_proxy() {
        let cfg = ProxyConfig { url: Some("http://proxy.example.com:8080".into()), ..Default::default() };
        let client = build_async_client_with_timeout(&cfg, std::time::Duration::from_secs(3));
        assert!(client.is_ok());
    }

    #[test]
    fn config_serde_round_trips() {
        let cfg = ProxyConfig {
            url: Some("socks5h://10.0.0.1:9050".into()),
            username: Some("tor".into()),
            password: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ProxyConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.url, cfg.url);
        assert_eq!(back.username, cfg.username);
        assert!(back.password.is_none());
    }

    #[test]
    fn empty_config_skips_optional_fields_in_json() {
        let cfg = ProxyConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(json, "{}");
    }

    #[test]
    fn proxy_password_never_serializes_and_credentials_need_url() {
        let cfg = ProxyConfig {
            url: Some("http://proxy.example.com:8080".into()),
            username: Some("user".into()), password: Some("secret".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("secret"));
        assert!(ProxyConfig { url: None, username: None, password: Some("secret".into()) }.validate().is_err());
    }
}
