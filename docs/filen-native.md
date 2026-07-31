# Native Filen client

`crisp-filen-native` is the Rust Filen implementation used by Tauri when the
`drive-filen-native` feature is enabled. Its protocol and crypto behavior are
derived from Filen's MIT Go SDK, with the Python and Dart clients used for
cross-checks.

## Runtime behavior

- Credentials are exchanged for one serialized `FilenSession` and stored in
  the existing OS-keychain drive secret. `drives.json` contains only the drive
  identity and routing metadata.
- Login supports the Filen v2 PBKDF2 flow and v3 Argon2id flow. When 2FA is
  disabled, the API receives Filen's six-character `XXXXXX` sentinel; a real
  six-character code is passed through unchanged.
- Listings are cached per folder and invalidated after mutations. Path search
  supports literal components and recursive `**`, plus `*` and `?` wildcards.
- Upload/download concurrency is bounded independently at chunk and file
  levels. `TransferConfig` exposes `chunk_size`, `workers`, `file_workers`,
  `retries`, and `retry_backoff_ms`.
- Uploads support exact resumable state (UUID, upload key, file key, bucket,
  region, chunk size, and completed chunk indices). Downloads support true
  byte ranges, direct-to-writer streaming, and post-transfer hash
  verification. `upload_file_from_reader` avoids buffering a complete upload,
  while the `_with_progress` upload/download variants report `(completed,
  total)` bytes. `get_file` fetches and decrypts one file's metadata without a
  parent listing.

## Supported mutations

The native client supports folder creation, move/rename, file replacement,
file and folder copy, timestamp metadata updates, trash/restore, permanent
deletion, and trash listing. Gateway responses that omit a JSON `data` object
are accepted for successful empty mutations.

## Verification

Hermetic tests exercise gateway parsing, cache invalidation, retries,
concurrency ceilings, wildcard search, resumable gaps, reader uploads,
streaming/progress downloads, range downloads, and crypto vectors. The ignored live suite uses unique folders and verifies both
directions with `../filen-python`:

```bash
source <(sed '/^2CAP=/d' /Users/you/code/.env)
FILEN_EMAIL="$FILEN_LOGIN" FILEN_PASSWORD="$FILEN_PW" \
  cargo test -p crisp-filen-native --test filen_live \
  -- --ignored --nocapture --test-threads=1
```

The Tauri integration is checked with:

```bash
cargo check -p crispsorter --features desktop,drive-filen-native
cargo check -p crispsorter --no-default-features \
  --features drive-filen-native --target aarch64-apple-ios
```
