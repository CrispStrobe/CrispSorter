# Durable duplicate trash contract

This narrow gate compiles the real private receipt journal and the production
Internxt receipt adapter methods, selected by the build script. The SDK transport
is an in-memory fixture; no credentials, keychain, HTTP, Tauri, models or remote
mutations are used. The app pins merged SDK `9b57beee7ac2e6ae9aaa15a1332a92bcfb5e1460`
and enables only Internxt's receipt-specific adapter, which calls the SDK's
`trash_once` and `restore_from_trash_once` APIs. This fixture models transport;
it cannot establish the SDK's no-replay policy. The pinned SDK's 11 real HTTP
controls and four configuration regressions qualify that separate boundary. Tests also reproduce
incomplete provider filename metadata to verify rejection; the production
capability and adapter exposure are checked separately from disabled legacy
path-only restore. It covers durable reopen, exact UUID/type/parent binding,
account/endpoint/root changes, duplicate names, replacement objects, replay,
concurrent claims, in-flight deletion/reconciliation races, idempotent success
confirmation, oversized-receipt rejection, private journal files, and read-only
recovery after uncertain delete/restore responses. The SDK wire protocol remains covered separately in
the native provider crate; this gate does not claim full application compilation
or live service qualification.

```sh
CARGO_BUILD_JOBS=1 cargo test --offline --locked --manifest-path tests/trash-receipt-contract/Cargo.toml
```
