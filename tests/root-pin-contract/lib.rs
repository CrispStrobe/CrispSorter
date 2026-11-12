#[path = "../../src-tauri/src/sync/cert_pins.rs"]
pub mod cert_pins;
#[path = "../../src-tauri/src/sync/root_pin_verifier.rs"]
pub mod root_pin_verifier;

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use rustls::client::danger::ServerCertVerifier;
    use rustls::client::WebPkiServerVerifier;
    use rustls::crypto::CryptoProvider;
    use rustls::sign::{Signer, SigningKey};
    use rustls::pki_types::{
        CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime,
    };
    use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};
    use sha2::{Digest, Sha256};
    use std::{
        io::{Cursor, Read, Write},
        path::Path,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join(name),
        )
        .unwrap()
    }
    fn certificate(name: &str) -> CertificateDer<'static> {
        fixture(&format!("{name}.der")).into()
    }
    fn roots(names: &[&str]) -> RootCertStore {
        let mut store = RootCertStore::empty();
        for name in names {
            store.add(certificate(&format!("{name}-root"))).unwrap();
        }
        store
    }
    fn provider() -> Arc<CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }
    fn policy() -> cert_pins::PinSet {
        let manifest: serde_json::Value =
            serde_json::from_slice(&fixture("manifest.json")).unwrap();
        cert_pins::PinSet {
            provider: "Test fixtures".into(),
            domains: vec![
                "*.example.test".into(),
                "*.allowed.test".into(),
                "blocked.test".into(),
            ],
            pins: vec![
                manifest["spki_pins"]["primary"].as_str().unwrap().into(),
                manifest["spki_pins"]["backup"].as_str().unwrap().into(),
            ],
        }
    }
    fn verifier(
        store: &RootCertStore,
        counter: Arc<AtomicUsize>,
    ) -> root_pin_verifier::RootPinVerifier {
        root_pin_verifier::RootPinVerifier::new(
            store,
            policy(),
            provider(),
            Some(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })),
        )
        .unwrap()
    }
    fn now() -> UnixTime {
        UnixTime::since_unix_epoch(Duration::from_secs(1_800_000_000))
    }
    fn verify(v: &dyn ServerCertVerifier, leaf: &str, chain: &[&str], host: &str) -> bool {
        let chain: Vec<_> = chain.iter().map(|name| certificate(name)).collect();
        v.verify_server_cert(
            &certificate(leaf),
            &chain,
            &ServerName::try_from(host.to_owned()).unwrap(),
            &[],
            now(),
        )
        .is_ok()
    }

    #[test]
    fn fixture_hashes_and_complete_spki_match_independent_openssl_exports() {
        let manifest: serde_json::Value =
            serde_json::from_slice(&fixture("manifest.json")).unwrap();
        for (name, hash) in manifest["files"].as_object().unwrap() {
            assert_eq!(
                format!("{:x}", Sha256::digest(fixture(name))),
                hash.as_str().unwrap(),
                "{name}"
            );
        }
        for name in ["primary", "backup", "other", "rogue", "constrained"] {
            let store = roots(&[name]);
            let exported = fixture(&format!("{name}-spki.der"));
            let independent_pin = STANDARD.encode(Sha256::digest(&exported));
            assert_eq!(independent_pin, manifest["spki_pins"][name]);
            assert_eq!(
                root_pin_verifier::trust_anchor_pin(&store.roots[0]),
                independent_pin
            );
            // Both DER short and long length forms are independently covered.
            assert_eq!(
                exported[1] >= 128,
                name == "primary" || name == "constrained"
            );
            assert_ne!(
                STANDARD.encode(Sha256::digest(
                    store.roots[0].subject_public_key_info.as_ref()
                )),
                independent_pin
            );
        }
    }

    #[test]
    fn only_matching_already_trusted_anchors_can_authorize_a_chain() {
        let store = roots(&["primary", "backup", "other"]);
        let counter = Arc::new(AtomicUsize::new(0));
        let pinned = verifier(&store, counter.clone());
        assert!(verify(&pinned, "primary-leaf", &[], "svc.example.test"));
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        assert!(verify(&pinned, "backup-leaf", &[], "svc.example.test"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        let ordinary = WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider())
            .build()
            .unwrap();
        assert!(verify(
            ordinary.as_ref(),
            "other-leaf",
            &[],
            "svc.example.test"
        ));
        assert!(!verify(
            &pinned,
            "other-leaf",
            &["primary-root"],
            "svc.example.test"
        ));
        assert!(!verify(
            &pinned,
            "rogue-leaf",
            &["rogue-root", "primary-root"],
            "svc.example.test"
        ));
        assert!(!verify(&pinned, "primary-leaf", &[], "other.example.test"));
        assert!(!verify(
            &pinned,
            "primary-leaf",
            &[],
            "svc.unconfigured.test"
        ));
        assert!(!verify(&pinned, "primary-expired", &[], "svc.example.test"));
        let mut broken = fixture("primary-leaf.der");
        *broken.last_mut().unwrap() ^= 1;
        assert!(pinned
            .verify_server_cert(
                &broken.into(),
                &[],
                &ServerName::try_from("svc.example.test").unwrap(),
                &[],
                now()
            )
            .is_err());
        assert!(pinned
            .verify_server_cert(
                &vec![1, 2, 3].into(),
                &[],
                &ServerName::try_from("svc.example.test").unwrap(),
                &[],
                now()
            )
            .is_err());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn trust_anchor_name_constraints_are_preserved_when_filtering_roots() {
        let store = roots(&["constrained", "backup"]);
        assert!(store.roots[0].name_constraints.is_some());
        let pinned = verifier(&store, Arc::new(AtomicUsize::new(0)));
        assert!(verify(
            &pinned,
            "constrained-allowed",
            &[],
            "svc.allowed.test"
        ));
        assert!(!verify(&pinned, "constrained-blocked", &[], "blocked.test"));
    }

    #[test]
    fn absent_primary_can_use_trusted_backup_but_absent_pins_fail_construction() {
        let counter = Arc::new(AtomicUsize::new(0));
        let pinned = verifier(&roots(&["backup"]), counter.clone());
        assert!(verify(&pinned, "backup-leaf", &[], "svc.example.test"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        assert!(root_pin_verifier::RootPinVerifier::new(
            &roots(&["other"]),
            policy(),
            provider(),
            None
        )
        .is_err());
        let mut invalid = policy();
        invalid.pins[1] = invalid.pins[0].clone();
        assert!(root_pin_verifier::RootPinVerifier::new(
            &roots(&["primary"]),
            invalid,
            provider(),
            None
        )
        .is_err());
    }

    #[derive(Debug)]
    struct FixedTime;
    impl rustls::time_provider::TimeProvider for FixedTime {
        fn current_time(&self) -> Option<UnixTime> {
            Some(now())
        }
    }
    #[derive(Debug)]
    struct CorruptSigner(Box<dyn Signer>);
    impl Signer for CorruptSigner {
        fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
            let mut signature = self.0.sign(message)?;
            signature[0] ^= 1;
            Ok(signature)
        }
        fn scheme(&self) -> rustls::SignatureScheme {
            self.0.scheme()
        }
    }
    #[derive(Debug)]
    struct CorruptKey(Arc<dyn SigningKey>);
    impl SigningKey for CorruptKey {
        fn choose_scheme(&self, offered: &[rustls::SignatureScheme]) -> Option<Box<dyn Signer>> {
            self.0
                .choose_scheme(offered)
                .map(|signer| Box::new(CorruptSigner(signer)) as Box<dyn Signer>)
        }
        fn algorithm(&self) -> rustls::SignatureAlgorithm {
            self.0.algorithm()
        }
    }
    #[derive(Debug)]
    struct Resolver(Arc<rustls::sign::CertifiedKey>);
    impl rustls::server::ResolvesServerCert for Resolver {
        fn resolve(
            &self,
            _: rustls::server::ClientHello<'_>,
        ) -> Option<Arc<rustls::sign::CertifiedKey>> {
            Some(self.0.clone())
        }
    }

    /// Actual rustls TLS state machines, no socket/server/model/HTTP mock.
    /// Queue a request before handshake; rejection must release no plaintext.
    fn handshake(
        leaf: &str,
        host: &str,
        version: &'static rustls::SupportedProtocolVersion,
        corrupt_signature: bool,
    ) -> (bool, Vec<u8>) {
        let pinned = verifier(
            &roots(&["primary", "backup", "other"]),
            Arc::new(AtomicUsize::new(0)),
        );
        let client = ClientConfig::builder_with_details(provider(), Arc::new(FixedTime))
            .with_protocol_versions(&[version])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(pinned))
            .with_no_client_auth();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fixture(&format!(
            "{leaf}-key.der"
        ))));
        let signing = provider().key_provider.load_private_key(key).unwrap();
        let signing: Arc<dyn SigningKey> = if corrupt_signature {
            Arc::new(CorruptKey(signing))
        } else {
            signing
        };
        let certified = rustls::sign::CertifiedKey::new(vec![certificate(leaf)], signing);
        let server = ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[version])
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Resolver(Arc::new(certified))));
        let mut client = ClientConnection::new(
            Arc::new(client),
            ServerName::try_from(host.to_owned()).unwrap(),
        )
        .unwrap();
        let mut server = ServerConnection::new(Arc::new(server)).unwrap();
        client
            .writer()
            .write_all(b"Authorization: Bearer fixture-only\r\n\r\nrequest-body")
            .unwrap();
        let mut plaintext = Vec::new();
        for _ in 0..32 {
            if client.wants_write() {
                let mut wire = Vec::new();
                client.write_tls(&mut wire).unwrap();
                server.read_tls(&mut Cursor::new(wire)).unwrap();
                if server.process_new_packets().is_err() {
                    return (false, plaintext);
                }
            }
            loop {
                let mut bytes = [0u8; 1024];
                match server.reader().read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => plaintext.extend_from_slice(&bytes[..n]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("TLS plaintext read: {error}"),
                }
            }
            if server.wants_write() {
                let mut wire = Vec::new();
                server.write_tls(&mut wire).unwrap();
                client.read_tls(&mut Cursor::new(wire)).unwrap();
                if client.process_new_packets().is_err() {
                    return (false, plaintext);
                }
            }
            if !client.is_handshaking()
                && !server.is_handshaking()
                && !client.wants_write()
                && !server.wants_write()
            {
                return (true, plaintext);
            }
        }
        panic!("TLS fixture did not finish within its bounded handshake budget");
    }

    #[test]
    fn actual_tls12_and_tls13_handshakes_preserve_signature_checks_and_withhold_requests() {
        for version in [&rustls::version::TLS12, &rustls::version::TLS13] {
            for leaf in ["primary-leaf", "backup-leaf"] {
                let (success, plaintext) = handshake(leaf, "svc.example.test", version, false);
                assert!(success, "{leaf} {version:?}");
                assert!(plaintext.starts_with(b"Authorization: Bearer fixture-only"));
            }
            for (leaf, host, corrupt) in [
                ("other-leaf", "svc.example.test", false),
                ("primary-expired", "svc.example.test", false),
                ("primary-leaf", "wrong.example.test", false),
                ("primary-leaf", "svc.example.test", true),
            ] {
                let (success, plaintext) = handshake(leaf, host, version, corrupt);
                assert!(!success, "{leaf} {host} corrupt={corrupt} {version:?}");
                assert!(
                    plaintext.is_empty(),
                    "a rejected handshake released request bytes"
                );
            }
        }
    }
}
