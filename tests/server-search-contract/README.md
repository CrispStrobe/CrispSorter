# Server storage and HTTP contracts

Run from the repository root with current stable Rust, a C/C++ build toolchain,
and `protoc` available on `PATH` (or `PROTOC` pointing to it):

```sh
cargo test --locked --manifest-path tests/server-search-contract/Cargo.toml
```

This is an isolated Cargo workspace. Its registry versions come from the main
workspace lock, while its path dependencies and `#[path]` modules point directly
to production sources. No copied parser, storage implementation, query scorer,
HTTP client, queue, route, or protocol implementation is substituted.

The tests use real temporary LanceDB, Tantivy, and SQLite stores and loopback
HTTP. They cover migration publication and failure, all-chunk reconstruction,
known versus unknown positional boundaries, duplicate canonicalization,
owner-scoped mutation and hydration, strict hybrid constraints, generation
recovery, monotonic delta timestamps, cursor pagination, durable queue packing,
and versioned client/server negotiation. The sync tests also use the production
client transport and durable checkpoint implementation.

`desktop.rs` supplies minimal application transport types. `adapters.rs` supplies
an ordinary HTTP client builder instead of the application's platform proxy
configuration. The model adapter rejects attempted model calls, allowing tests
to verify query validation happens before embedding without downloading models.
These tests do not establish model quality, hardware inference behavior, proxy
compatibility, or full Tauri application packaging.

The build script's bundled `protoc` path applies only to this harness process;
Lance's dependency build scripts still need `protoc` or `PROTOC` available before
Cargo starts. Keep the main workspace and this lock's registry records aligned
when adding dependencies.
