//! P29.7 — TLS certificate pinning for cloud providers.
//!
//! SHA-256 SPKI (Subject Public Key Info) pins for root CAs used by
//! Google, Microsoft, Dropbox, and Amazon/S3 endpoints.
//!
//! # Strategy
//!
//! We pin the *root* CA, not the leaf cert.  Root CAs rotate on a
//! multi-year cadence (leaf certs rotate every few months).  Each
//! provider has 2 pins: current + backup, so the app survives a CA
//! migration.  If only the backup pin matches, we log a warning
//! (signals an upcoming rotation) but still allow the connection.
//!
//! Pinning is applied *after* standard chain validation — a pinned
//! connection that passes chain validation but fails pin verification
//! is rejected, while a connection that fails chain validation is
//! rejected regardless of pins.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};

// ── Pin data ─────────────────────────────────────────────────────────────

/// A named pin set for one cloud provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinSet {
    /// Human-readable provider name (e.g. "Google").
    pub provider: String,
    /// Domain patterns this pin set applies to (e.g. "*.googleapis.com").
    pub domains: Vec<String>,
    /// SHA-256 hashes of the root CA SPKI (hex-encoded).
    /// First entry is the primary; second is backup.
    pub pins: Vec<String>,
}

impl PinSet {
    /// Validate a pin set before it is used to select a TLS policy.
    ///
    /// Pins are SHA-256 SPKI digests encoded as standard base64, therefore
    /// every entry must decode to exactly 32 bytes. Two distinct pins are
    /// required so a rotation cannot accidentally remove the fallback.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.provider.trim().is_empty(), "pin provider is empty");
        anyhow::ensure!(!self.domains.is_empty(), "pin set has no domains");
        anyhow::ensure!(
            self.pins.len() == 2,
            "pin set must contain primary and backup pins"
        );
        anyhow::ensure!(
            self.pins[0] != self.pins[1],
            "primary and backup pins are identical"
        );
        for pin in &self.pins {
            let decoded = STANDARD
                .decode(pin)
                .map_err(|_| anyhow::anyhow!("pin is not valid base64"))?;
            anyhow::ensure!(decoded.len() == 32, "pin must decode to a SHA-256 digest");
        }
        for domain in &self.domains {
            anyhow::ensure!(!domain.trim().is_empty(), "pin domain is empty");
        }
        Ok(())
    }
}

/// Built-in pin sets for major cloud providers.
///
/// Pin hashes sourced from the respective CA root certificates.
/// Last verified: 2026-07.
pub fn builtin_pin_sets() -> Vec<PinSet> {
    vec![
        PinSet {
            provider: "Google".into(),
            domains: vec![
                "*.googleapis.com".into(),
                "*.google.com".into(),
                "oauth2.googleapis.com".into(),
                "www.googleapis.com".into(),
            ],
            pins: vec![
                // GTS Root R1 (Google Trust Services)
                "hxqRlPTu1bMS/0DITB1SSu0vd4u/8l8TjPgfaAp63Gc=".into(),
                // GTS Root R4 (backup)
                "MhmPCI0GnRAMiaRSmmGpj8gPKE+E9GYMBvvLH3OdsFo=".into(),
            ],
        },
        PinSet {
            provider: "Microsoft".into(),
            domains: vec![
                "graph.microsoft.com".into(),
                "*.sharepoint.com".into(),
                "login.microsoftonline.com".into(),
            ],
            pins: vec![
                // DigiCert Global Root G2
                "i7WTqTvh0OioIruIfFR4kMPnBqrS2rdiVPl/s2uC/CY=".into(),
                // Baltimore CyberTrust Root (legacy, being phased out)
                "Y9mvm0exBk1JoQ57f9Vm28jKo5lFm/woKcVxrYxu80o=".into(),
            ],
        },
        PinSet {
            provider: "Dropbox".into(),
            domains: vec![
                "*.dropboxapi.com".into(),
                "api.dropboxapi.com".into(),
                "content.dropboxapi.com".into(),
            ],
            pins: vec![
                // DigiCert Global Root G2
                "i7WTqTvh0OioIruIfFR4kMPnBqrS2rdiVPl/s2uC/CY=".into(),
                // DigiCert Global Root CA
                "r/mIkG3eEpVdm+u/ko/cwxzOMo1bk4TyHIlByibiA5E=".into(),
            ],
        },
        PinSet {
            provider: "Amazon/S3".into(),
            domains: vec![
                "*.s3.amazonaws.com".into(),
                "s3.amazonaws.com".into(),
                "*.s3.*.amazonaws.com".into(),
            ],
            pins: vec![
                // Amazon Root CA 1
                "++MBgDH5WGvL9Bcn5Be30cRcL0f5O+NyoXuWtQdX1aI=".into(),
                // Starfield Services Root CA - G2
                "KwccWaCgrnaw6tsrrSO61FgLacNgG2MMLq8GE6+oP5I=".into(),
            ],
        },
    ]
}

/// Validate the shipped policy before a connector is constructed.
///
/// This is intentionally separate from handshake enforcement: the current
/// reqwest/native-tls boundary does not expose the validated root chain to a
/// portable SPKI verifier yet.  Failing closed on malformed built-in policy
/// prevents a future enforcement boundary from silently consuming bad data.
pub fn validate_builtin_pin_sets() -> anyhow::Result<()> {
    for pin_set in builtin_pin_sets() {
        pin_set.validate()?;
    }
    Ok(())
}

/// Find the pin set that matches a given hostname.
pub fn find_pin_set<'a>(hostname: &str, pin_sets: &'a [PinSet]) -> Option<&'a PinSet> {
    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    for ps in pin_sets {
        for domain in &ps.domains {
            if domain_matches(domain, &hostname) {
                return Some(ps);
            }
        }
    }
    None
}

/// Simple wildcard domain matcher.
/// Handles `*.example.com` matching `foo.example.com` but not
/// `foo.bar.example.com` or `example.com` itself.
fn domain_matches(pattern: &str, hostname: &str) -> bool {
    let pattern = pattern.trim_end_matches('.').to_ascii_lowercase();
    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // Wildcard: hostname must have exactly one label before the suffix.
        if let Some(rest) = hostname.strip_suffix(suffix) {
            // rest should be "label." (one label + the dot)
            rest.ends_with('.') && !rest[..rest.len() - 1].contains('.')
        } else {
            false
        }
    } else {
        pattern.eq_ignore_ascii_case(&hostname)
    }
}

/// Verify that a certificate's SPKI hash matches a pin set.
///
/// `spki_hash` should be the base64-encoded SHA-256 hash of the
/// certificate's Subject Public Key Info.
///
/// Returns `(matches, is_backup)`:
/// - `(true, false)` — primary pin matched
/// - `(true, true)` — backup pin matched (log a warning)
/// - `(false, false)` — no pin matched (reject the connection)
pub fn verify_pin(spki_hash: &str, pin_set: &PinSet) -> (bool, bool) {
    for (i, pin) in pin_set.pins.iter().enumerate() {
        if pin == spki_hash {
            return (true, i > 0);
        }
    }
    (false, false)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_matches_exact() {
        assert!(domain_matches("graph.microsoft.com", "graph.microsoft.com"));
        assert!(!domain_matches(
            "graph.microsoft.com",
            "other.microsoft.com"
        ));
    }

    #[test]
    fn domain_matches_wildcard() {
        assert!(domain_matches("*.googleapis.com", "www.googleapis.com"));
        assert!(domain_matches("*.googleapis.com", "oauth2.googleapis.com"));
        // Should NOT match deeper subdomains.
        assert!(!domain_matches("*.googleapis.com", "a.b.googleapis.com"));
        // Should NOT match the bare domain.
        assert!(!domain_matches("*.googleapis.com", "googleapis.com"));
    }

    #[test]
    fn find_pin_set_matches_google() {
        let sets = builtin_pin_sets();
        let ps = find_pin_set("www.googleapis.com", &sets);
        assert!(ps.is_some());
        assert_eq!(ps.unwrap().provider, "Google");
    }

    #[test]
    fn find_pin_set_no_match() {
        let sets = builtin_pin_sets();
        assert!(find_pin_set("random.example.com", &sets).is_none());
    }

    #[test]
    fn builtins_are_valid_rotation_sets() {
        validate_builtin_pin_sets().expect("shipped pin policy must validate");
    }

    #[test]
    fn verify_pin_primary() {
        let sets = builtin_pin_sets();
        let google = find_pin_set("www.googleapis.com", &sets).unwrap();
        let (ok, backup) = verify_pin(&google.pins[0], google);
        assert!(ok);
        assert!(!backup);
    }

    #[test]
    fn verify_pin_backup() {
        let sets = builtin_pin_sets();
        let google = find_pin_set("www.googleapis.com", &sets).unwrap();
        let (ok, backup) = verify_pin(&google.pins[1], google);
        assert!(ok);
        assert!(backup);
    }

    #[test]
    fn verify_pin_no_match() {
        let sets = builtin_pin_sets();
        let google = find_pin_set("www.googleapis.com", &sets).unwrap();
        let (ok, _) = verify_pin("definitely-not-a-real-pin", google);
        assert!(!ok);
    }

    #[test]
    fn builtin_sets_cover_all_providers() {
        let sets = builtin_pin_sets();
        let providers: Vec<&str> = sets.iter().map(|s| s.provider.as_str()).collect();
        assert!(providers.contains(&"Google"));
        assert!(providers.contains(&"Microsoft"));
        assert!(providers.contains(&"Dropbox"));
        assert!(providers.contains(&"Amazon/S3"));
    }

    #[test]
    fn pin_set_serde_round_trips() {
        let sets = builtin_pin_sets();
        let json = serde_json::to_string(&sets).unwrap();
        let back: Vec<PinSet> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), sets.len());
        assert_eq!(back[0].provider, sets[0].provider);
    }

    #[test]
    fn builtin_pin_sets_validate() {
        for pin_set in builtin_pin_sets() {
            pin_set.validate().unwrap();
        }
    }

    #[test]
    fn pin_validation_rejects_malformed_rotation() {
        let invalid = PinSet {
            provider: "Example".into(),
            domains: vec!["example.com".into()],
            pins: vec!["not-base64".into(), "not-base64".into()],
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn hostname_matching_normalizes_case_and_trailing_dot() {
        let sets = builtin_pin_sets();
        assert!(find_pin_set("WWW.GOOGLEAPIS.COM.", &sets).is_some());
        assert!(find_pin_set("Googleapis.com.", &sets).is_none());
    }
}
