# Storage source contract

This small native gate compiles the real transfer queue, staged file publisher,
mounted-file reader, disk cache and operation logger. The build script selects
the complete production CloudDrive trait, LocalDrive implementation and their
shared types from `drives/filesystem.rs`. Provider SDKs, OCR models, Tauri and sibling
repositories are not needed; no storage implementation is substituted.

It exercises streaming dispatch without the whole-file API, private temporary
files, verified byte counts, failure/retry cleanup, queued and active
cancellation, late-cancellation receipt ordering, backpressure, disk-cache
eviction and active-reader ownership, bounded content/preview reads, local
upload validation, refusal to replay a possibly published write, and structured
log redaction.

```sh
cargo test --locked --manifest-path tests/storage-contract/Cargo.toml -- --test-threads=2
```

The committed dependency lock was retrieved from the successful macOS native
run at `fe274b06` (53 tests). The creation workspace has no Cargo or rustc;
the newer merged source requires its own hosted result. The mounted-storage
workflow enforces `--locked` and retains both the exact lock and execution log
on Linux, macOS and Windows. Full app compilation, live mounted
NAS throughput, FUSE mounting and the separate direct-SMB fixture remain
additional gates; this contract does not claim to execute those surfaces.
