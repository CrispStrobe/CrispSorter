# crisp-internxt

> **Unofficial community client.** This is not an official Internxt product
> and is not affiliated with or endorsed by Internxt.

Purpose-built native Internxt Cloud Drive protocol client for CrispSorter.

The crate owns Internxt login transport, session serialization, path
resolution, folder/file operations, and file encryption. CrispSorter adds its
keychain and `CloudDrive` adapter around it. The `crisp-internxt` binary
exercises the same library without starting Tauri.

The local `internxt-core` crate is a test-only MIT reference/oracle. It is not
part of the production dependency graph; its crypto vectors catch drift while
our implementation remains free to provide path resolution, batching,
chunking, resume, and the sync adapter shape this app needs.

Build and run the offline protocol check:

```sh
cargo run -p crisp-internxt -- crypto-vector
```

For live testing, provide the password through stdin and choose an explicit
session path. Session files contain bearer credentials and the account
mnemonic; protect or remove them after testing:

```sh
printf '%s\n' "$INTERNXT_PASSWORD" \
  | cargo run -p crisp-internxt -- login user@example.com --session /tmp/internxt-session.json
cargo run -p crisp-internxt -- list /tmp/internxt-session.json .
```

## Cross-app reuse

This library is already consumed by CrispSorter's native adapter and its own
CLI. Exposing the same versioned Rust source to CrispCloud and CrisperPlayer
through Dart adapters is proposed work; no Filen/Internxt Flutter bridge is
implemented here. Keep protocol/session/crypto code in this crate and host
credentials, job scheduling and UI integration in the application.

The client uses blocking HTTP and needs a storage worker when called from
async/UI code. Cross-engine sessions/checkpoints need explicit version and
identity checks; started mutations must not be replayed through another engine.
MPL-2.0 remains the declared license. See
[the storage-engine design](../../docs/STORAGE_ENGINE_ARCHITECTURE.md) for the
Dart transition, constrained fallback, platform limits and compatibility tests.
