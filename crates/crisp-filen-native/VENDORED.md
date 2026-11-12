# Vendored: crisp-filen

This crate is a vendored copy of `crates/crisp-filen` from
<https://github.com/CrispStrobe/crisp-cloud-rs>.

| | |
|---|---|
| Upstream branch | `feature/smb-storage-optimization-2026-11-11` |
| Upstream revision | `0f9097d2a0ca0ddd15ba56d13405d93b0be95c3a` ("style: format listing URL and clarify provider conformance scope") |
| Last app-side sync | `d6e4f8c8` (same change, applied here) |
| Compared | 2026-11-12, `src/lib.rs`, `src/main.rs`, `Cargo.toml` |

The vendored baseline above differs from the upstream validation source. Both
`.github/workflows/provider-conformance.yml` and the `cloud-facade` job pin
`33bdc33f64d0a2f1144dceec8ed372d18c12b551`, which includes the Windows root
fix and the Rust clippy correction. This is the merge of upstream PR #3; its tree is identical to the previously
verified PR head `121bfa6682ab710cc19fbd7c7897e4aea0729920`.

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
2. Record the new vendored baseline here; assess and record the separately
   pinned upstream validation revision in the workflows and architecture guide.
3. Run `cargo test -p crisp-filen --lib` and the provider-conformance workflow.

- **Windows root paths** (2026-11-12): `resolve_path` walks only `Component::Normal` segments. Upstream skips only `/`, so on Windows the root component `\` was looked up as a folder name and every absolute path failed. Upstream fix: CrispStrobe/crisp-cloud-rs#1.
