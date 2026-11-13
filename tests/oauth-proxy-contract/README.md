# OAuth shared proxy contract

Compiles the complete production OAuth, shared proxy and pin-policy modules,
plus selected production Tauri command bodies and the credentials type. Keychain,
application state, command registration and log sinks are fixtures. Token exchange,
refresh and revoke use real HTTP against loopback fixtures, including an explicit
authenticated proxy; no provider account or credential discovery occurs.

```sh
CARGO_BUILD_JOBS=1 cargo test --offline --locked --manifest-path tests/oauth-proxy-contract/Cargo.toml
```

This checks proxy routing, credential preservation after failure, early invalid
configuration rejection and asynchronous command wiring. It does not qualify
native keychain integration, full Tauri execution, a real proxy/provider, or TLS
certificate pin enforcement. Shared pin-set validation remains policy validation;
no handshake root/SPKI verifier has been installed.

The 41 focused controls passed locally with one compilation job. Operation-log
sinks and OS-keychain storage are fixtures; tokens and proxy credentials in the
HTTP fixtures are inert test values, and no live provider is contacted.

The harness also compiles the production WebDAV constructor, authenticated
request and range-read methods, plus its response validator. With reqwest gzip
enabled, it checks that ordinary reads still decompress while range reads request
identity and reject encoded responses. Loopback regressions cover missing or
conflicting Content-Range, wrong offsets, EOF clipping, overflow, truncated and
oversized bodies, and the existing 416 error behavior. A server withholding the
rest of an oversized response verifies rejection after the declared span plus
one byte. Run `cargo test --locked --manifest-path tests/oauth-proxy-contract/Cargo.toml webdav_range -- --test-threads=1`.

This validates each returned range, not atomicity of the WebDAV delta-download
operation: that separate path currently applies blocks in place and can retain
earlier changes if a later request fails.
