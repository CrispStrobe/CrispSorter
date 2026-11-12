//! Experimental Linux Google Drive API/upload transport, compiled only on opt-in.
//! No changes to OAuth, SDK-owned clients, or the default application transport.

use super::{
    cert_pins::PinSet,
    proxy::{configure_blocking_builder, ProxyConfig},
    root_pin_verifier::RootPinVerifier,
};
use anyhow::{Context, Result};
use std::{sync::Arc, time::Duration};

/// No bundled-root fallback: pin candidates must already be trusted by Linux's
/// configured native certificate bundle (including SSL_CERT_FILE / SSL_CERT_DIR).
/// Native trust-setting semantics on macOS/Windows are not qualified here.
pub(crate) fn build_google_client(proxy: &ProxyConfig) -> Result<reqwest::blocking::Client> {
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("experimental Google root pinning is currently supported only on Linux");
    #[cfg(target_os = "linux")]
    {
        let loaded = rustls_native_certs::load_native_certs();
        // Do not expose certificate paths or OS error payloads in app logs.
        anyhow::ensure!(
            loaded.errors.is_empty(),
            "loading Google pinning native roots failed"
        );
        let mut roots = rustls::RootCertStore::empty();
        for cert in loaded.certs {
            roots
                .add(cert)
                .context("invalid native root certificate for Google pinning")?;
        }
        anyhow::ensure!(
            !roots.is_empty(),
            "no native roots available for Google pinning"
        );
        let mut policy = super::cert_pins::builtin_pin_sets()
            .into_iter()
            .find(|policy| policy.provider == "Google")
            .context("missing Google pin policy")?;
        // This transport serves only this fixed API/upload origin, not OAuth.
        policy.domains = vec!["www.googleapis.com".into()];
        pinned_builder(proxy, &roots, policy)?
            .build()
            .context("building pinned Google client")
    }
}

/// Kept separate so deterministic fixtures exercise the actual builder and TLS
/// verifier without mutating process-wide native trust or contacting Google.
pub(crate) fn pinned_builder(
    proxy: &ProxyConfig,
    roots: &rustls::RootCertStore,
    policy: PinSet,
) -> Result<reqwest::blocking::ClientBuilder> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = RootPinVerifier::new(
        roots,
        policy,
        provider.clone(),
        Some(Arc::new(|| {
            log::warn!(
                "experimental Google root pinning accepted backup-root chain; review pin rotation"
            );
        })),
    )?;
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(
        configure_blocking_builder(proxy, Some(Duration::from_secs(60)), false)?
            .tls_backend_preconfigured(tls)
            .https_only(true)
            // No same-host or cross-origin replay; 3xx is an error, never a success body.
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                attempt.error("redirect refused by pinned Google transport")
            }))
            .retry(reqwest::retry::never()),
    )
}
