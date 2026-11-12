# Isolated rustls root-pin verifier foundation

This crate compiles the real pin-policy module and the experimental
`src-tauri/src/sync/root_pin_verifier.rs`. That verifier is deliberately absent
from `sync/mod.rs` and all application/provider client constructors. No production
dependency, TLS default, request behavior, or SDK transport is changed.
The isolated contract pins the app lock's rustls 0.23.40, WebPKI 0.103.13 and
PKI-types 1.14.1, with the explicit ring crypto provider.

The verifier restricts ordinary rustls WebPKI verification to anchors already in
the caller's trusted root store whose **complete DER SPKI** digest matches the
primary or backup pin. It never imports a server-presented certificate as a trust
anchor. Anchor subject and name constraints are retained. WebPKI performs chain,
hostname, certificate validity and signature checks; TLS1.2/1.3 handshake
signatures are delegated to the same crypto provider's standard verifier. Only
policy-matching DNS names are admitted. A backup observer receives no peer data
and signals chain acceptance, not complete session establishment.

Fixtures are fixed, committed TEST-ONLY certificate/key bytes. Tests do not
generate certificates, run OpenSSL, open sockets, or contact services. Their
manifest records SHA-256 hashes and independent OpenSSL full-SPKI exports for
RSA long-form and Ed25519 short-form DER lengths. The root-constrained allowed
and blocked chains were independently checked with OpenSSL at the fixed test
time; the blocked chain reports `permitted subtree violation`. Private leaf keys
are inert fixtures and must never be used outside these tests.

```sh
CARGO_BUILD_JOBS=1 cargo test --offline --locked --manifest-path tests/root-pin-contract/Cargo.toml -- --test-threads 1
```

Actual in-memory TLS1.2 and TLS1.3 peers exercise primary/backup success and
rejection of ordinarily trusted but unpinned roots, expired/wrong-host chains,
and corrupted handshake signatures. A fake authorization/body queued before
handshake must yield no server plaintext on rejection. Additional controls cover
malformed and wrongly signed certificates, unrelated pinned roots supplied by a
peer, root name constraints, missing trusted anchors and malformed pin policy.

Limits: caller-supplied trusted anchors are a prerequisite; this component does
not populate or qualify OS/platform roots or shipped CA pins. It uses ordinary
WebPKI defaults without configured CRLs; revocation/OCSP policy is not added.
No cross-signed-root, proxy, redirect, reqwest, SDK, platform-backend, full-app,
or live-service integration is qualified. Keep the production integration task
open until those boundaries and provider pins are verified.

All 21 focused controls passed locally with one compilation job on the pinned
TLS dependency versions. No application build or production handshake was run.
