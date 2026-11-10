# Durable index contract tests

This standalone Rust package qualifies the application's storage and recovery
logic with its actual LanceDB, Arrow, Tantivy, and SQLite implementations. It
runs without the desktop shell, a GPU, downloaded models, or private model
dependencies.

Run from the repository root:

```sh
cargo test --manifest-path tests/index-write-contract/Cargo.toml --locked
```

Lance's build requires a `protoc` executable. Set `PROTOC` to its absolute path
when it is not on `PATH`. The vendored protobuf build dependency supplies
platform binaries in environments that do not have a system package.

`build.rs` selects production items from their Rust syntax trees. The index
transaction implementation and its tests are compiled from the application
source, including the durable write journal and the metadata transaction.
There is no second implementation of replacement, deletion, recovery, or queue
acknowledgement in this package. Model adapters are deliberately unavailable;
calling one fails the test instead of producing a fabricated embedding.

The suite checks:

- Atomic complete-document replacement, legacy duplicate cleanup, and removal
  of obsolete tail chunks without changing unrelated sources.
- Preservation of the last searchable document after extraction, embedding,
  model output validation, or preparation fails.
- Recovery before readiness after the vector transaction commits but the full
  text writer cannot commit; uncommitted and partially committed receipts fail
  safely without applying a different document generation.
- Serialized writes from two independently opened pipelines and a shared read
  gate covering recovery and both stores.
- Durable paired imports, explicit conflict replacement, preservation of both
  conflicting documents when requested, and deletion without resurrection.
- Metadata updates that keep chunk-specific evidence and vectors while
  refreshing lexical title and language fields.
- Metadata-only re-scans, explicit failure and skip state, source hash changes,
  and mounted-source predicates applied before a result limit.
- Queue recovery after reopening, error retry, idempotent and job-scoped
  acknowledgements, and transaction rollback on an injected SQLite failure.

This suite does not qualify model inference, platform extractors, GPU backends,
the desktop command registration, or the full search engine. Those remain the
responsibility of the application and platform integration suites. The separate
search contract package qualifies query parsing and lexical execution.
