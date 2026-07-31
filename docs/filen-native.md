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
- Listings are cached per folder for 10 minutes by default and invalidated after
  mutations. `set_listing_cache_ttl` configures freshness and
  `list_folder_fresh` bypasses reuse for a single read. Path search
  supports literal components and recursive `**`, plus `*` and `?` wildcards.
  `list_folder_cached` and `list_folder_with_paths` match the native Internxt
  client naming and support bounded recursive inventories.
- Upload/download concurrency is bounded independently at chunk and file
  levels. `TransferConfig` exposes `chunk_size`, `workers`, `file_workers`,
  `retries`, and `retry_backoff_ms`.
  `upload_files_with_progress` and `download_files_with_progress` report
  serialized file-completion progress for batch consumers.
  `upload_files_resumable` adds durable per-file batch state and explicit
  `BatchConflictPolicy::{Fail, Skip, Replace}` handling.
  `download_paths_resumable` provides the symmetric local-path batch flow.
- Uploads support exact resumable state (UUID, upload key, file key, bucket,
  region, chunk size, and completed chunk indices). Downloads support true
  byte ranges, direct-to-writer streaming, and post-transfer hash
  verification. `upload_file_from_reader` avoids buffering a complete upload,
  while the `_with_progress` upload/download variants report `(completed,
  total)` bytes. `get_file` fetches and decrypts one file's metadata without a
  parent listing. `upload_path` and `download_path` recursively bridge local
  files/directories without materializing whole files. The
  `upload_path_with_timestamps` and `download_path_with_timestamps` variants
  preserve source/remote timestamps; local timestamp application is best
  effort. `resume_upload_from_reader`, `replace_file_from_reader`, and
  `replace_file_from_path` provide
  bounded replacement uploads, and `copy_file` stages on disk instead of
  materializing the complete plaintext. Resumable checkpoints
  can be persisted with `save_upload_resume_state`, restored with
  `load_upload_resume_state`, and removed with `clear_upload_resume_state`.

- Login preserves gateway error codes such as `enter_2fa` and `wrong_2fa` in
  the returned error while still sending Filen's `XXXXXX` sentinel when no
  code is supplied.

## Supported mutations

The native client supports folder creation, move/rename, file replacement,
file and folder copy, timestamp metadata updates, trash/restore, permanent
deletion, and trash listing. Gateway responses that omit a JSON `data` object
are accepted for successful empty mutations.

## Verification

Hermetic tests exercise gateway parsing, cache invalidation, retries,
concurrency ceilings, wildcard/path search, resumable gaps, reader/path
uploads, streaming/progress downloads, range downloads, 2FA error handling,
and crypto vectors. The ignored live suite uses unique folders and verifies both
directions with `../filen-python`:

```bash
export FILEN_EMAIL="$(sed -n 's/^FILEN_LOGIN=//p' /Users/christianstrobele/code/.env)"
export FILEN_PASSWORD="$(sed -n 's/^FILEN_PW=//p' /Users/christianstrobele/code/.env)"
FILEN_EMAIL="$FILEN_EMAIL" FILEN_PASSWORD="$FILEN_PASSWORD" \
  cargo test -p crisp-filen-native --test filen_live \
  -- --ignored --nocapture --test-threads=1
```

Current suite counts: Rust native unit/hermetic tests 32 passing; Python
suite 30 passing; Dart suite 244 passing with 3 expected live-suite skips when
credentials are absent. The Rust live cross-client suite has 3 authenticated
tests covering mutations and both transfer directions.

The Tauri integration is checked with:

```bash
cargo check -p crispsorter --features desktop,drive-filen-native
cargo check -p crispsorter --no-default-features \
  --features drive-filen-native --target aarch64-apple-ios
```
