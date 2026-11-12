# Vendored: crisp-internxt

This crate is a vendored copy of `crates/crisp-internxt` from
<https://github.com/CrispStrobe/crisp-cloud-rs>.

| | |
|---|---|
| Upstream branch | `feature/smb-storage-optimization-2026-11-11` |
| Upstream revision | `0f9097d2a0ca0ddd15ba56d13405d93b0be95c3a` |
| Last app-side sync | `d6e4f8c8` |
| Compared | 2026-11-12, `src/lib.rs`, `src/main.rs` (identical), `Cargo.toml` |

The same revision is pinned in `.github/workflows/provider-conformance.yml` and
in the `cloud-facade` job of `.github/workflows/ci.yml`. Advance all three
together.

## App-local divergences

These exist here and not upstream. Port them upstream (or re-apply them) when
re-syncing; do not drop them silently.

- **Proxy policy:** `pub struct HttpConfig` and
  `InternxtNativeClient::new_with_config_and_http`; the default constructors
  route through it. The login client is built with `http1_only()`. The app's
  `NativeInternxtDrive` depends on this.
- **`InternxtSession::active_token()`:** prefers `new_token` over `token`;
  `refresh_session` and the app adapter use it.
- **Login error mapping:** private `gateway_error` keeps the gateway's `code`
  and `message` (e.g. the 2FA code) in login security/access/hydration errors.
- **Progress API shape (diverges, not just additive):**
  - here: `upload_reader` returns `Result<()>`;
    `upload_reader_with_progress(.., progress: ProgressCallback)` wraps the
    reader in `ProgressReader` and returns `Result<()>`;
    `download_file_to_writer_with_progress(.., progress: ProgressCallback)`
    wraps the writer in `ProgressWriter` (one metadata call for the total).
  - upstream: `upload_reader_with_progress<F: FnMut(u64, u64)>` returns the
    plaintext SHA-256 `String`, and `download_file_to_writer_with_progress`
    takes an `FnMut(u64, u64)`.
  A re-sync must pick one shape and update callers.
- **Plaintext hash helpers:** public `sha256_hex` and `verify_sha256`.
- **Verbose diagnostics:** per-part PUT timing in the multipart retry loop when
  `set_verbose(true)`.
- **Small differences:** `is_multiple_of(16)` instead of `% 16 != 0`; one
  helper is `#[cfg(test)]` instead of `#[allow(dead_code)]`; resumable-download
  doc wording; `OpenOptions::truncate(false)` call order.
- **Tests only:** `HttpConfig`, `active_token`, hash verification, progress
  wrappers, download checkpoint round-trip, crypto cross-checked against the
  vendored `internxt-core` (dev-dependency), 2FA error mapping.
- **`Cargo.toml`:** no `rust-version = "1.85"`; dev-dependency on
  `../internxt-core`.

## Notes for the app adapter

- There is no in-memory range primitive: `download_file_to_path_ranged` only
  parallelises a full download into a path. `CloudDrive::read_range` therefore
  stays unsupported for Internxt (see the TODO in
  `src-tauri/src/drives/internxt_native_drive.rs`).
- `update_file` (upload-beside-then-swap) only takes a local path, so the app
  still overwrites from readers by trash-then-upload.

## Re-sync checklist

1. `git -C ../crisp-cloud-rs diff <old-rev> <new-rev> -- crates/crisp-internxt`
   and apply it here, keeping the divergences above.
2. Update the revision in this file, `provider-conformance.yml`, `ci.yml` and
   `docs/STORAGE_ENGINE_ARCHITECTURE.md`.
3. Run `cargo test -p crisp-internxt --lib` and the provider-conformance workflow.
