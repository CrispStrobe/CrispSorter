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

The build script also selects the production pair-comparison/push planner and
file-manager conflict decision, with the real conflict-policy type and mutation
unit tests. The focused regressions reject ambiguous newest-wins batches before
watermark advancement and preserve existing destinations with unknown/equal
timestamps. They do not execute Tauri commands or establish provider CAS safety.

The production sync inventory and local destination preflight are included
directly as modules. Their focused tests exercise invalid provider names,
Unicode-preserving nested inventory, platform filename checks and Unix symlink
escape rejection. Inventory regressions also cover iterative depth-first order,
entry/directory/depth budgets, provider errors, real symlink cycles and refusal
to publish incomplete rows. These preflight checks do not establish descriptor-based
protection against concurrent filesystem path swaps.

```sh
CARGO_BUILD_JOBS=1 cargo test --locked --manifest-path tests/storage-contract/Cargo.toml sync_paths_production -- --test-threads=1
CARGO_BUILD_JOBS=1 cargo test --locked --manifest-path tests/storage-contract/Cargo.toml sync_inventory_production -- --test-threads=1
```

```sh
CARGO_BUILD_JOBS=1 cargo test --locked --manifest-path tests/storage-contract/Cargo.toml file_conflict -- --test-threads=1
```

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

The held-destination pull module and production batch loop are included directly.
Tests exercise real queue waits, inventory/stream/publication directory swaps,
relocation, retries, cancellation, uncertain publication and stage ownership.
On Windows the same harness also runs the native handle backend tests; no
pathname fallback is used. Run focused controls with:

```sh
CARGO_BUILD_JOBS=1 cargo test --locked --manifest-path tests/storage-contract/Cargo.toml pull_destination -- --test-threads=1
```

The Windows workflow tries the existing loopback administrative share and logs
UNC UNRUN explicitly when it is inaccessible. UNC is also an explicitly ignored
native fixture that can be run manually. With an accessible real
UNC directory set in `CRISPSORTER_TEST_MOUNT_ROOT`, run the same command with
`-- --ignored unc_or_verbatim_root`. An unavailable share means UNC remains
unrun, not passed. Cross-compilation checks APIs only; native Windows execution
is required before merging the held-destination candidate.
