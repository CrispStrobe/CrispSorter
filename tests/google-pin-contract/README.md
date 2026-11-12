# Opt-in Google Drive pinned reqwest contract

This isolated crate compiles the actual shared proxy policy, root-pin verifier,
and pinned reqwest builder. Its build script extracts the actual Google Drive
struct/constructors (without changing function bodies) to check feature gating
and Linux native-root loading without pulling in Tauri/native inference builds.
It does not compile the whole provider module, app command registry or packaging.
Both reqwest native-tls and rustls are enabled; the transport must explicitly use
the preconfigured rustls verifier rather than native-tls's default verifier.
The explicit ring provider avoids building the unused AWS-LC C implementation in
this lightweight harness. TLS dependency versions match the app lock.

The actual app feature `experimental-google-root-pins` is off by default. On
Linux it affects Google Drive API/upload requests from `new_with_proxy`, not
Google OAuth or any other provider. Non-Linux construction rejects the feature.
The normal constructor, origins, and shared proxy behavior remain unchanged when
the feature is absent. The new transport is HTTPS-only, restricts its verifier
to www.googleapis.com, rejects all redirects, disables automatic retries, and
uses a 60-second request timeout. Native-root load errors or absent trusted pins
fail closed. No bundled public roots are trusted by this transport.

Tests open only deterministic loopback sockets. Existing fixed test-only TLS
fixtures are reused from `../root-pin-contract/fixtures`; their private keys are
inert test data. HTTP fake tokens and bodies are never real credentials.
Official public Google roots/SPKI exports and acquisition hashes are retained in
`official-roots`; OpenSSL independently exported complete DER SPKI from Google's
published PEMs on 2026-11-12. No test fetches certificates or contacts providers.
The subprocess root-loading tests control SSL_CERT_FILE and SSL_CERT_DIR
(an empty fixture directory, or an intentionally missing directory to test partial
load failure) without racing process-wide environment changes in parallel tests.

Run serial Cargo jobs, subject to the repository resource gate:

```sh
CARGO_BUILD_JOBS=1 cargo test --locked --offline --manifest-path tests/google-pin-contract/Cargo.toml
CARGO_BUILD_JOBS=1 cargo test --locked --offline --manifest-path tests/google-pin-contract/Cargo.toml --features experimental-google-root-pins
```

Limits: no live Google API/upload request, cross-signed-root chain, CRL/OCSP,
full-app build, macOS/Windows trust-settings validation, SOCKS or HTTPS-proxy
qualification. Explicit/environment HTTP proxy CONNECT behavior is retained;
HTTPS-proxy TLS also uses the restricted verifier and may fail closed. Rejected
TLS delivers no HTTP token/body to the origin, but configured proxy credentials
necessarily authenticate the CONNECT proxy before origin verification. The root
pin checks CA identity; root rotation availability remains an operational risk.

All 34 focused controls passed locally with and without the feature; Cargo used
one job, and no full-app build or live provider request was run.
