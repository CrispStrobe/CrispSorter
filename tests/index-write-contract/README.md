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
acknowledgement in this package. The production search engine also compiles here.
Its dense retrieval contracts use an explicit fixed query vector to test joins
and candidate behavior deterministically. Other model adapters fail if called;
this package does not run model inference or measure relevance quality.

The suite checks:

- Atomic complete-document replacement, legacy duplicate cleanup, and removal
  of obsolete tail chunks without changing unrelated sources.
- Independent occurrence identities for identical bytes at different paths or
  owners; stable identities and reviewed fields across file moves; exact input
  ordering of committed IDs after repeated-source batch coalescing.
- Bounded repair of shared legacy IDs, retaining other sources' text, vectors,
  and metadata without cross-owner text leakage or cross-source proximity hits.
- Scoped deletion and selected-source promotion receipts, including a new
  conflicting identity appearing while a prepared write waits for its lease.
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
- Background source recovery, preserved OCR options, one live scheduler per
  data directory, coalesced follow-up requests, dropped worker recovery, and
  durable acknowledgements before progress advances.
- File-byte receipts checked under the index writer lease after extraction and
  embedding, including changes while a prepared document waits to commit.
- Document diversity across long sources, complete late-chunk hydration, owner
  isolation in lexical and dense retrieval, and literal structured filters.
- Actual stored-passage navigation and provenance, including sparse chunks and
  owner constraints, rather than a reconstruction from returned snippets.
- Complete lexical matching totals, disk-backed identity deduplication, matching
  facets, index-generation checks, and cooperative cancellation.
- Streamed complete reports and atomic returned-window reports, source and
  storage protection, stable metadata ordering, zero-result receipts, and
  cancellation before publication.
- Federated rank fusion using the production wire type and merge function,
  repeated hits within a source, finite representative evidence, deterministic
  ties, and owner-aware CLI result identities.
- Legacy schema rebuilds from complete owner-scoped stored text, interrupted
  staging and directory publication, rollback preservation, and missing or
  empty full-text storage with existing document rows.

This suite does not qualify model inference, platform extractors, GPU backends,
or desktop worker adapters and command registration. Those remain the
responsibility of the application and platform integration suites. The separate
search contract package qualifies query parsing and lexical execution; the
server and extraction packages exercise their production modules independently.
