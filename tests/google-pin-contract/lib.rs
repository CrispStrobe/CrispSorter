#[path = "../../src-tauri/src/sync/cert_pins.rs"]
pub mod cert_pins;
#[allow(dead_code)]
#[path = "../../src-tauri/src/sync/pinned_google_transport.rs"]
pub mod pinned_google_transport;
#[path = "../../src-tauri/src/sync/proxy.rs"]
pub mod proxy;
#[path = "../../src-tauri/src/sync/root_pin_verifier.rs"]
pub mod root_pin_verifier;
mod sync {
    #[cfg(feature = "experimental-google-root-pins")]
    pub use crate::pinned_google_transport;
    pub use crate::proxy;
}
include!(concat!(env!("OUT_DIR"), "/google_constructor.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
    };
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../root-pin-contract/fixtures")
                .join(name),
        )
        .unwrap()
    }
    fn roots(names: &[&str]) -> rustls::RootCertStore {
        let mut roots = rustls::RootCertStore::empty();
        for name in names {
            roots
                .add(CertificateDer::from(fixture(&format!("{name}-root.der"))))
                .unwrap();
        }
        roots
    }
    fn policy() -> cert_pins::PinSet {
        let m: serde_json::Value = serde_json::from_slice(&fixture("manifest.json")).unwrap();
        cert_pins::PinSet {
            provider: "Fixtures".into(),
            domains: vec!["svc.example.test".into()],
            pins: vec![
                m["spki_pins"]["primary"].as_str().unwrap().into(),
                m["spki_pins"]["backup"].as_str().unwrap().into(),
            ],
        }
    }
    #[derive(Debug)]
    struct CorruptSigner(Box<dyn rustls::sign::Signer>);
    impl rustls::sign::Signer for CorruptSigner {
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
    struct CorruptKey(Arc<dyn rustls::sign::SigningKey>);
    impl rustls::sign::SigningKey for CorruptKey {
        fn choose_scheme(
            &self,
            offered: &[rustls::SignatureScheme],
        ) -> Option<Box<dyn rustls::sign::Signer>> {
            self.0
                .choose_scheme(offered)
                .map(|signer| Box::new(CorruptSigner(signer)) as Box<dyn rustls::sign::Signer>)
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

    fn server(
        cert: &str,
        response: String,
    ) -> (SocketAddr, Arc<AtomicUsize>, thread::JoinHandle<Vec<u8>>) {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let corrupt = cert == "corrupt-handshake";
        let cert = if corrupt { "primary-leaf" } else { cert };
        let key = provider
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(fixture(
                &format!("{cert}-key.der"),
            ))))
            .unwrap();
        let key: Arc<dyn rustls::sign::SigningKey> = if corrupt {
            Arc::new(CorruptKey(key))
        } else {
            key
        };
        let certified = rustls::sign::CertifiedKey::new(
            vec![CertificateDer::from(fixture(&format!("{cert}.der")))],
            key,
        );
        let tls = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Resolver(Arc::new(certified))));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let count2 = count.clone();
        let handle = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(tls)).unwrap(),
                socket,
            );
            let mut seen = vec![];
            let mut buf = [0u8; 2048];
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                seen.extend_from_slice(&buf[..n]);
                if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            if !seen.is_empty() {
                count2.fetch_add(1, Ordering::SeqCst);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
            seen
        });
        (addr, count, handle)
    }
    fn builder(roots: &rustls::RootCertStore) -> reqwest::blocking::ClientBuilder {
        pinned_google_transport::pinned_builder(&proxy::ProxyConfig::default(), roots, policy())
            .unwrap()
            .no_proxy()
    }
    fn response() -> String {
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into()
    }
    #[test]
    fn reqwest_accepts_primary_and_backup_before_http() {
        for cert in ["primary-leaf", "backup-leaf"] {
            let (addr, count, thread) = server(cert, response());
            let client = builder(&roots(&["primary", "backup"]))
                .resolve("svc.example.test", addr)
                .build()
                .unwrap();
            let result = client
                .get(format!("https://svc.example.test:{}/", addr.port()))
                .bearer_auth("fixture-only-token")
                .send()
                .unwrap();
            assert_eq!(result.text().unwrap(), "ok");
            assert!(String::from_utf8(thread.join().unwrap())
                .unwrap()
                .contains("Bearer fixture-only-token"));
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }
    #[test]
    fn rejected_tls_delivers_no_http_credentials_or_body() {
        for (cert, host) in [
            ("other-leaf", "svc.example.test"),
            ("primary-expired", "svc.example.test"),
            ("primary-leaf", "wrong.example.test"),
            ("rogue-leaf", "svc.example.test"),
            ("corrupt-handshake", "svc.example.test"),
        ] {
            let (addr, count, thread) = server(cert, response());
            let client = builder(&roots(&["primary", "backup", "other"]))
                .resolve(host, addr)
                .build()
                .unwrap();
            let result = client
                .post(format!("https://{host}:{}/", addr.port()))
                .bearer_auth("fixture-only-token")
                .body("private fixture body")
                .send();
            assert!(result.is_err(), "{cert} {host}");
            assert!(thread.join().unwrap().is_empty(), "{cert} {host}");
            assert_eq!(count.load(Ordering::SeqCst), 0);
        }
    }
    #[test]
    fn redirects_are_errors_and_never_contact_target() {
        for target in [
            "http://svc.example.test",
            "https://outside.example.test",
            "https://svc.example.test",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let target_addr = listener.local_addr().unwrap();
            let location = format!("{target}:{}/", target_addr.port());
            let (addr, _, thread) = server("primary-leaf", format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"));
            let client = builder(&roots(&["primary"]))
                .resolve("svc.example.test", addr)
                .resolve("outside.example.test", target_addr)
                .build()
                .unwrap();
            let err = client
                .post(format!("https://svc.example.test:{}/", addr.port()))
                .bearer_auth("fixture-only-token")
                .body("private fixture body")
                .send()
                .unwrap_err();
            assert!(err.is_redirect());
            assert!(!thread.join().unwrap().is_empty());
            assert!(
                matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
            );
        }
    }
    #[test]
    fn no_http_downgrade_or_missing_trust_fallback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = builder(&roots(&["primary"])).build().unwrap();
        assert!(client
            .get(format!("http://{}/", listener.local_addr().unwrap()))
            .send()
            .is_err());
        assert!(listener.accept().is_err());
        assert!(pinned_google_transport::pinned_builder(
            &proxy::ProxyConfig::default(),
            &roots(&["other"]),
            policy()
        )
        .is_err());
    }
    #[test]
    fn official_google_pins_match_independently_exported_spki() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use sha2::{Digest, Sha256};
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("official-roots");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let google = cert_pins::builtin_pin_sets()
            .into_iter()
            .find(|p| p.provider == "Google")
            .unwrap();
        for (index, name) in ["gtsr1", "gtsr4"].iter().enumerate() {
            let der = std::fs::read(dir.join(format!("{name}.der"))).unwrap();
            let spki = std::fs::read(dir.join(format!("{name}-spki.der"))).unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(CertificateDer::from(der.clone())).unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(&der)),
                manifest[name]["der_sha256"]
            );
            assert_eq!(STANDARD.encode(Sha256::digest(spki)), google.pins[index]);
            assert_eq!(
                root_pin_verifier::trust_anchor_pin(&roots.roots[0]),
                google.pins[index]
            );
            assert_eq!(google.pins[index], manifest[name]["spki_sha256_base64"]);
        }
    }
    #[test]
    fn native_root_environment_child() {
        let Some(case) = std::env::var_os("CRISPSORTER_PIN_TEST_CHILD") else {
            return;
        };
        let _ = &case;
        let result = GoogleDriveDrive::new_with_proxy(
            "fixture".into(),
            "fixture-only-token".into(),
            None,
            None,
            None,
            &proxy::ProxyConfig::default(),
        );
        #[cfg(feature = "experimental-google-root-pins")]
        if case.to_string_lossy().starts_with("valid") {
            let drive = result.unwrap();
            assert_eq!(drive.api_base, "https://www.googleapis.com/drive/v3");
            assert_eq!(
                drive.upload_base,
                "https://www.googleapis.com/upload/drive/v3"
            );
        } else {
            assert!(result.is_err());
        }
        #[cfg(not(feature = "experimental-google-root-pins"))]
        {
            assert!(
                result.is_ok(),
                "default constructor must ignore experimental trust settings"
            );
        }
    }
    #[test]
    fn real_constructor_native_roots_and_default_gating() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("official-roots");
        for (case, file) in [
            ("valid", dir.join("gtsr1.pem")),
            ("valid-backup", dir.join("gtsr4.pem")),
            ("partial-error", dir.join("gtsr1.pem")),
            ("missing", dir.join("not-present.pem")),
            (
                "untrusted",
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("untrusted-root.pem"),
            ),
        ] {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::native_root_environment_child",
                    "--nocapture",
                ])
                .env("CRISPSORTER_PIN_TEST_CHILD", case)
                .env("SSL_CERT_FILE", file)
                .env(
                    "SSL_CERT_DIR",
                    if case == "partial-error" {
                        dir.join("absent-directory")
                    } else {
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("empty-trust-directory")
                    },
                )
                .env_remove("HTTP_PROXY")
                .env_remove("HTTPS_PROXY")
                .env_remove("ALL_PROXY")
                .env_remove("http_proxy")
                .env_remove("https_proxy")
                .env_remove("all_proxy")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{case}: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }

    #[test]
    fn authenticated_connect_proxy_preserves_pinning_and_no_fallback() {
        for cert in ["primary-leaf", "other-leaf"] {
            let (target, count, server) = server(cert, response());
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let proxy_addr = listener.local_addr().unwrap();
            let tunnel = thread::spawn(move || {
                let (mut incoming, _) = listener.accept().unwrap();
                incoming
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut headers = vec![];
                let mut byte = [0];
                while !headers.ends_with(b"\r\n\r\n") {
                    incoming.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                incoming
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .unwrap();
                let mut upstream = std::net::TcpStream::connect(target).unwrap();
                let mut upstream_copy = upstream.try_clone().unwrap();
                let mut incoming_copy = incoming.try_clone().unwrap();
                let forward = thread::spawn(move || {
                    let _ = std::io::copy(&mut incoming_copy, &mut upstream_copy);
                    let _ = upstream_copy.shutdown(std::net::Shutdown::Write);
                });
                let _ = std::io::copy(&mut upstream, &mut incoming);
                let _ = incoming.shutdown(std::net::Shutdown::Write);
                forward.join().unwrap();
                headers
            });
            let config = proxy::ProxyConfig {
                url: Some(format!("http://{proxy_addr}")),
                username: Some("fixture-user".into()),
                password: Some("fixture-password".into()),
            };
            let client = pinned_google_transport::pinned_builder(
                &config,
                &roots(&["primary", "other"]),
                policy(),
            )
            .unwrap()
            .build()
            .unwrap();
            let result = client
                .get("https://svc.example.test/")
                .bearer_auth("fixture-only-token")
                .send();
            if cert == "primary-leaf" {
                assert_eq!(result.unwrap().text().unwrap(), "ok");
            } else {
                assert!(result.is_err());
            }
            drop(client);
            let seen = server.join().unwrap();
            let headers = String::from_utf8(tunnel.join().unwrap())
                .unwrap()
                .to_lowercase();
            assert!(headers.starts_with("connect svc.example.test:443 "));
            assert!(headers.contains("proxy-authorization: basic "));
            assert!(!headers.contains("bearer"));
            assert_eq!(seen.is_empty(), cert != "primary-leaf");
            assert_eq!(
                count.load(Ordering::SeqCst),
                usize::from(cert == "primary-leaf")
            );
        }
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        let config = proxy::ProxyConfig {
            url: Some(format!("http://{addr}")),
            ..Default::default()
        };
        let client =
            pinned_google_transport::pinned_builder(&config, &roots(&["primary"]), policy())
                .unwrap()
                .build()
                .unwrap();
        assert!(client
            .get("https://svc.example.test/")
            .bearer_auth("fixture-only-token")
            .send()
            .is_err());
    }
}
