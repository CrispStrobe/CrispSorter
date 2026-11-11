# Storage source contract

This small native gate compiles the real transfer queue, staged file publisher,
mounted-file reader, disk cache and operation logger. The build script selects
the complete production CloudDrive trait, LocalDrive implementation and their
shared types from the app's Rust AST. Provider SDKs, OCR models, Tauri and sibling
repositories are not needed; no storage implementation is substituted.

It exercises streaming dispatch without the whole-file API, private temporary
files, verified byte counts, failure/retry cleanup, queued and active
cancellation, late-cancellation receipt ordering, backpressure, disk-cache
eviction and active-reader ownership, bounded content/preview reads, local
upload validation, and structured log redaction.

```sh
cargo test --manifest-path tests/storage-contract/Cargo.toml -- --test-threads=2
```

The creation workspace has no Cargo or rustc, so it cannot truthfully claim a
local native run or manufacture a tested dependency lock. The mounted-storage
workflow has an explicit first-generation step: when Cargo.lock is absent it
generates that snapshot, runs tests with `--locked`, and uploads both the exact
lock and execution log on Linux, macOS and Windows. Once the generated lock is
committed, the workflow uses it directly. Full app compilation, live mounted
NAS throughput, FUSE mounting and the separate direct-SMB fixture remain
additional gates; this contract does not claim to execute those surfaces.
