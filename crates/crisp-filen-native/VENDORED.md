# Vendored: crisp-filen

This crate is a vendored copy of `crates/crisp-filen` from
<https://github.com/CrispStrobe/crisp-cloud-rs>.

| | |
|---|---|
| Upstream branch | `feature/smb-storage-optimization-2026-11-11` |
| Upstream revision | `0f9097d2a0ca0ddd15ba56d13405d93b0be95c3a` ("style: format listing URL and clarify provider conformance scope") |
| Last app-side sync | `d6e4f8c8` (same change, applied here) |
| Compared | 2026-11-12, `src/lib.rs`, `src/main.rs`, `Cargo.toml` |

The table records the app-side sync baseline. The provider conformance and
cloud-facade CI jobs instead pin upstream `98583771f13919d5e4efa95133843c1eabc94e92`,
a one-line Clippy-only fix cherry-picked onto `0aaa6666a6108f4d4d29877a34afe6ec428e0467`.
Keep those two CI pins aligned; this does not relabel the app-local sync baseline.

## App-local divergences

These exist here and not upstream. Port them upstream (or re-apply them) when
re-syncing; do not drop them silently.

- **Proxy policy:** `pub struct HttpConfig` (proxy URL + optional basic-auth
  credentials, rejected without a URL) and
  `FilenNativeClient::from_session_with_http_config`. Upstream builds a plain
  `reqwest` client. The app's `NativeFilenDrive` depends on this.
- **Trash filtering:** `list_trash_filtered(kind, limit)` and the private
  `filter_trash_items`, plus `empty_trash()` (sequential permanent delete).
  Upstream's CLI filters trash inline. `src/main.rs` differs accordingly:
  `list-trash` calls `list_trash_filtered`, and there is an extra
  `empty-trash --force` command.
- **Cancellation clone placement:** the batch upload/download workers clone
  `options.cancellation` once outside the spawn loop (behaviourally equivalent).
- **Docs/tests only:** extra doc comments on resumable download state and
  `download_file_to_path_resumable`; tests for `HttpConfig`, a login capture
  server checking 2FA wire values, trash filtering and hermetic `empty_trash`.
  Upstream has one comment (UTF-8 byte offsets in a test) not present here.
- **`Cargo.toml`:** `description` and `repository`
  (`https://github.com/CrispStrobe/crisp-filen`) differ.

## Notes for the app adapter

- `TransferConfig::chunk_size` is the Filen wire chunk size (1 MiB);
  `download_file_range` maps byte offsets with it. Never tune it per drive.
- `resume_upload_from_reader` uploads sequentially; `TransferConfig::workers`
  only affects in-memory uploads and chunk downloads.

## Re-sync checklist

1. `git -C ../crisp-cloud-rs diff <old-rev> <new-rev> -- crates/crisp-filen`
   and apply it here, keeping the divergences above.
2. Update the revision in this file, `provider-conformance.yml`, `ci.yml` and
   `docs/STORAGE_ENGINE_ARCHITECTURE.md`.
3. Run `cargo test -p crisp-filen --lib` and the provider-conformance workflow.

- **Windows root paths** (2026-11-12): `resolve_path` walks only `Component::Normal` segments. Upstream skips only `/`, so on Windows the root component `\` was looked up as a folder name and every absolute path failed. Upstream fix: CrispStrobe/crisp-cloud-rs#1.
