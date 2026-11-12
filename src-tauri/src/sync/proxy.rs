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
    super::cert_pins::validate_builtin_pin_sets()?;
    configure_async_builder(config)?
        .build()
        .context("building proxied async client")
}

/// Build an async client with an explicit request timeout and proxy policy.
pub fn build_async_client_with_timeout(
    config: &ProxyConfig,
    timeout: std::time::Duration,
) -> Result<reqwest::Client> {
    config.validate()?;
    super::cert_pins::validate_builtin_pin_sets()?;
    configure_async_builder(config)?
        .timeout(timeout)
        .build()
        .context("building proxied async client")
}

/// A malformed proxy URL can contain credentials anywhere, including strings
/// which cannot be parsed safely enough to redact. Replace the parser error
/// entirely: keeping it as an anyhow source could leak through {:#} or Debug.
fn configured_proxy(config: &ProxyConfig) -> Result<Option<reqwest::Proxy>> {
    let Some(url) = &config.url else {
        return Ok(None);
    };
    let mut proxy = reqwest::Proxy::all(url).map_err(|_| {
        anyhow::anyhow!(
            "invalid proxy URL: check the proxy scheme, hostname and port (HTTP, HTTPS or SOCKS5)"
        )
    })?;
    if let (Some(user), Some(pass)) = (&config.username, &config.password) {
        proxy = proxy.basic_auth(user, pass);
    }
    Ok(Some(proxy))
}

fn configure_async_builder(config: &ProxyConfig) -> Result<reqwest::ClientBuilder> {
    let mut builder = reqwest::ClientBuilder::new();

    if let Some(proxy) = configured_proxy(config)? {
        builder = builder.proxy(proxy);
    }
    Ok(builder)
}

/// Build a blocking `reqwest::blocking::Client` with the given proxy config.
///
/// Used by `CloudDrive` implementations which are synchronous.
pub fn build_blocking_client(config: &ProxyConfig) -> Result<reqwest::blocking::Client> {
    build_blocking_client_with_options(config, None, false)
}

/// Build a blocking client with an optional request timeout and explicit
/// opt-out for TLS certificate verification.  The latter is only intended
/// for local/self-signed WebDAV endpoints; callers should leave it disabled
/// for public cloud providers.
pub fn build_blocking_client_with_options(
    config: &ProxyConfig,
    timeout: Option<std::time::Duration>,
    insecure_tls: bool,
) -> Result<reqwest::blocking::Client> {
    configure_blocking_builder(config, timeout, insecure_tls)?
        .build()
        .context("building proxied blocking client")
}

/// Shared policy for specialized transports; caller must build the returned builder.
pub(crate) fn configure_blocking_builder(
    config: &ProxyConfig,
    timeout: Option<std::time::Duration>,
    insecure_tls: bool,
) -> Result<reqwest::blocking::ClientBuilder> {
    config.validate()?;
    super::cert_pins::validate_builtin_pin_sets()?;
    let mut builder = reqwest::blocking::ClientBuilder::new();

    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    if insecure_tls {
        builder = builder.danger_accept_invalid_certs(true);
    }

    if let Some(proxy) = configured_proxy(config)? {
        builder = builder.proxy(proxy);
    }

    Ok(builder)
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
    fn malformed_proxy_urls_never_expose_secrets_in_any_error_format() {
        for url in [
            "http://inline-user:inline-password@proxy.example.test:99999/private-path?token=query-secret#fragment-secret",
            "https://inline-user:inline-password@[invalid-host]/private-path?token=query-secret#fragment-secret",
            "http://encoded%40user:encoded%3Apassword@proxy.example.test:invalid-port/private-path?token=query-secret#fragment-secret",
            "not a proxy URL inline-user inline-password query-secret",
        ] {
            let config = ProxyConfig {
                url: Some(url.into()),
                username: Some("configured-user".into()),
                password: Some("configured-password".into()),
            };
            let results = [
                build_blocking_client(&config).map(|_| ()),
                build_async_client(&config).map(|_| ()),
                build_async_client_with_timeout(&config, std::time::Duration::from_secs(3)).map(|_| ()),
                // The opt-in Google transport uses this actual shared builder.
                configure_blocking_builder(&config, None, false).map(|_| ()),
            ];
            for result in results {
                let error = result.expect_err("malformed proxy URL must fail construction");
                assert!(error.to_string().contains("invalid proxy URL"));
                assert!(error.to_string().contains("scheme, hostname and port"));
                assert_eq!(error.chain().count(), 1, "discard the unsafe parser error source");
                for rendered in [format!("{error}"), format!("{error:#}"), format!("{error:?}")] {
                    for secret in [url, "inline-user", "inline-password", "encoded%40user", "encoded%3Apassword",
                        "query-secret", "fragment-secret", "private-path", "configured-user", "configured-password"] {
                        assert!(!rendered.contains(secret), "proxy secret leaked through error formatting");
                    }
                }
            }
        }
    }

    #[test]
    fn valid_secret_bearing_proxy_urls_preserve_construction() {
        for url in [
            "http://inline-user:inline-password@127.0.0.1:8080/private-path?token=query-secret#fragment-secret",
            "https://inline-user:inline-password@127.0.0.1:8080",
            "socks5://inline-user:inline-password@127.0.0.1:1080",
            "socks5h://inline-user:inline-password@127.0.0.1:1080",
        ] {
            let config = ProxyConfig { url: Some(url.into()), ..Default::default() };
            assert!(build_blocking_client(&config).is_ok());
            assert!(build_async_client(&config).is_ok());
        }
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
        let cfg = ProxyConfig {
            url: Some("http://proxy.example.com:8080".into()),
            ..Default::default()
        };
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
            username: Some("user".into()),
            password: Some("secret".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("secret"));
        assert!(ProxyConfig {
            url: None,
            username: None,
            password: Some("secret".into())
        }
        .validate()
        .is_err());
    }
}
