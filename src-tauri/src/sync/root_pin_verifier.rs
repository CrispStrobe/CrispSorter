//! Experimental rustls foundation; intentionally not registered in `sync/mod.rs`
//! or installed in any application/provider client. See the source-bound contract.
//!
//! Trust anchors come only from the caller's trusted root store. Restricting the
//! ordinary WebPKI verifier to matching anchors preserves its chain, name, time,
//! constraints and handshake-signature checks without trusting server-supplied
//! roots. Default WebPKI revocation behavior is unchanged (no configured CRLs).

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, TrustAnchor, UnixTime};
use rustls::{DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use super::cert_pins::{find_pin_set, PinSet};

/// Standard SHA-256/base64 of complete DER SubjectPublicKeyInfo. Rustls trust
/// anchors contain the SEQUENCE *contents*, so restore its canonical tag/length.
/// This is digest arithmetic, not certificate authentication.
pub fn trust_anchor_pin(anchor: &TrustAnchor<'_>) -> String {
    let contents = anchor.subject_public_key_info.as_ref();
    let mut hash = Sha256::new();
    hash.update([0x30]);
    if contents.len() < 128 {
        hash.update([contents.len() as u8]);
    } else {
        let bytes = contents.len().to_be_bytes();
        let first = bytes.iter().position(|byte| *byte != 0).unwrap();
        hash.update([0x80 | (bytes.len() - first) as u8]);
        hash.update(&bytes[first..]);
    }
    hash.update(contents);
    STANDARD.encode(hash.finalize())
}

pub struct RootPinVerifier {
    policy: PinSet,
    primary: Option<Arc<WebPkiServerVerifier>>,
    backup: Option<Arc<WebPkiServerVerifier>>,
    backup_observer: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for RootPinVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootPinVerifier")
            .field("primary_available", &self.primary.is_some())
            .field("backup_available", &self.backup.is_some())
            .finish_non_exhaustive()
    }
}

impl RootPinVerifier {
    /// Build a host-scoped verifier from already-trusted anchors and validated
    /// primary/backup pins. No roots are imported from the server's chain.
    /// Unmatched hostnames and absence of a trusted matching root fail closed.
    /// The optional observer receives no certificate, hostname or credentials;
    /// it signals acceptance of a backup-root chain, not handshake completion.
    pub fn new(
        trusted_roots: &RootCertStore,
        policy: PinSet,
        provider: Arc<CryptoProvider>,
        backup_observer: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Self> {
        policy.validate()?;
        let verifier_for = |pin: &str| -> Result<Option<Arc<WebPkiServerVerifier>>> {
            let roots = RootCertStore {
                roots: trusted_roots
                    .roots
                    .iter()
                    .filter(|anchor| trust_anchor_pin(anchor) == pin)
                    .cloned()
                    .collect(),
            };
            if roots.is_empty() {
                return Ok(None);
            }
            Ok(Some(
                WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                    .build()
                    .context("build pinned WebPKI verifier")?,
            ))
        };
        let primary = verifier_for(&policy.pins[0])?;
        let backup = verifier_for(&policy.pins[1])?;
        anyhow::ensure!(
            primary.is_some() || backup.is_some(),
            "no trusted root matches the pin policy"
        );
        Ok(Self {
            policy,
            primary,
            backup,
            backup_observer,
        })
    }

    fn signature_verifier(&self) -> &WebPkiServerVerifier {
        self.primary
            .as_ref()
            .or(self.backup.as_ref())
            .expect("constructor requires a verifier")
    }
}

impl ServerCertVerifier for RootPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, Error> {
        let ServerName::DnsName(name) = server_name else {
            return Err(Error::General(
                "TLS server name is outside the pinned DNS policy".into(),
            ));
        };
        if find_pin_set(name.as_ref(), std::slice::from_ref(&self.policy)).is_none() {
            return Err(Error::General(
                "TLS server name is outside the pinned DNS policy".into(),
            ));
        }
        let mut failure = None;
        if let Some(primary) = &self.primary {
            match primary.verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response,
                now,
            ) {
                Ok(verified) => return Ok(verified),
                Err(error) => failure = Some(error),
            }
        }
        if let Some(backup) = &self.backup {
            match backup.verify_server_cert(
                end_entity,
                intermediates,
                server_name,
                ocsp_response,
                now,
            ) {
                Ok(verified) => {
                    if let Some(observer) = &self.backup_observer {
                        observer();
                    }
                    return Ok(verified);
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        Err(failure.expect("constructor requires a verifier"))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        self.signature_verifier()
            .verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        self.signature_verifier()
            .verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.signature_verifier().supported_verify_schemes()
    }
}
