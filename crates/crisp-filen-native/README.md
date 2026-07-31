# crisp-filen

> **Unofficial community client.** This is not an official Filen product and
> is not affiliated with or endorsed by Filen.

Native Rust client for the Filen Cloud Drive protocol.

This crate implements the protocol directly in Rust. Its cryptography and
gateway payloads are aligned with Filen's MIT-licensed Go implementation; the
Python and Dart clients are used as interoperability and behavior
cross-checks. It is designed for desktop applications, CLIs, and other Rust
consumers that need encrypted Filen transfers without spawning Python.

## Features

- Filen v1, v2, and v3 authentication and metadata crypto
- Password login with actionable `enter_2fa` and `wrong_2fa` errors
- Serializable sessions suitable for an OS keychain
- TTL-aware listing cache, explicit fresh listings, and mutation invalidation
- Recursive `*`, `?`, and `**` wildcard search
- Path resolution and bounded recursive path listings
- Folder creation, move, rename, replace, copy, trash, restore, and deletion
- Gateway-safe timestamp metadata updates
- Streaming uploads/downloads, range downloads, and hash verification
- Configurable chunk workers, file workers, chunk size, retries, and backoff
- Resumable uploads with durable checkpoints and reader-based continuation
- Resumable recursive downloads with conflict policies and byte progress
- Batch upload/download progress callbacks, including aggregate byte totals
- Standalone `crisp-filen` CLI for smoke testing and diagnostics

## Basic usage

```rust,no_run
use crisp_filen::{FilenNativeClient, FilenSession};

fn download(session: &FilenSession, uuid: &str) -> anyhow::Result<Vec<u8>> {
    let client = FilenNativeClient::from_session(session)?;
    let item = client.get_file(uuid)?;
    Ok(client.download_file(&item)?)
}
```

Transfer tuning is explicit:

```rust,no_run
use crisp_filen::{FilenNativeClient, FilenSession, TransferConfig};

fn configure(session: &FilenSession) -> anyhow::Result<FilenNativeClient> {
    FilenNativeClient::from_session_with_config(
        session,
        TransferConfig {
            chunk_size: 4 * 1024 * 1024,
            workers: 4,
            file_workers: 2,
            retries: 4,
            retry_backoff_ms: 250,
        },
    )
}
```

For large local files use `upload_file_from_reader`,
`resume_upload_from_reader`, `download_file_to_writer`, or
`download_path_with_timestamps`. The batch APIs add durable per-file state,
conflict handling, and serialized progress callbacks.

## Authentication and session storage

`FilenNativeClient::login` performs the gateway login and returns a
`FilenSession`. Store the result of `FilenSession::encode()` in a platform
keychain or other protected secret store. Do not put passwords, API keys, or
serialized sessions in source control or ordinary configuration files.

When two-factor authentication is required, pass the six-digit code as the
fourth login argument. If it is missing or incorrect, the returned error
preserves Filen's `enter_2fa` or `wrong_2fa` code.

## Testing

Fast unit and hermetic HTTP coverage requires no credentials:

```bash
cargo test -p crisp-filen --lib
cargo check -p crisp-filen --bins
```

The authenticated live suite creates unique temporary folders and removes
them during cleanup. Set credentials without sourcing an arbitrary `.env`
file:

```bash
export FILEN_EMAIL="$(sed -n 's/^FILEN_LOGIN=//p' /path/to/.env)"
export FILEN_PASSWORD="$(sed -n 's/^FILEN_PW=//p' /path/to/.env)"
cargo test --test filen_live -- --ignored --nocapture --test-threads=1
```

The live tests cover Rust/Python round trips, recursive path transfers,
timestamps, search, replacement, copy, trash/restore, range downloads, and
the mutation surface. Credentials are never read by the unit tests and are
never part of the repository.

## Compatibility and license

The authoritative protocol reference is Filen's MIT-licensed Go client. This
crate is licensed under MPL-2.0. It is an unofficial community client and is
not affiliated with or endorsed by Filen.
See the repository's issue tracker for protocol changes and compatibility
reports.
