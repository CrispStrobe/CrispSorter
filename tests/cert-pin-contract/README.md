# Certificate pin policy contract

This crate compiles the production pin-policy module without the application or
provider SDKs. It checks shipped rotation data, exact DNS-label policy selection,
regional S3 patterns, malformed patterns/hosts, and digest matching.

```sh
CARGO_BUILD_JOBS=1 cargo test --offline --locked --manifest-path tests/cert-pin-contract/Cargo.toml
```

These checks do not establish TLS certificate pin enforcement. Cloud clients still
use their existing TLS trust validation; no handshake verifier consumes these
pins yet. Root-chain/SPKI verification, current root-pin qualification, backup-pin
warnings, and client integration remain open work.
