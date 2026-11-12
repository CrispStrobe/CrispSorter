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

The receipt journal and Internxt receipt capability fail closed on non-Unix
platforms because directory-entry persistence is not implemented there. The
Unix mutation fixture suite retains its existing behavior. On non-Unix targets,
a production-journal test verifies refusal before journal creation, and the
source-selected native adapter capability test checks that it exposes neither
receipt capability nor an adapter. The lightweight receipt workflow stages both
Linux and Windows controls with source/tree/file hashes and retained logs.
Mobile Unix targets execute the fsync protocol but still require
their own crash-durability and application qualification.

```sh
CARGO_BUILD_JOBS=1 cargo test --offline --locked --manifest-path tests/trash-receipt-contract/Cargo.toml
```
