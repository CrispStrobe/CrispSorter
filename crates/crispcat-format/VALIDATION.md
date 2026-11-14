# Standalone codec extraction acceptance

The CAF and index implementation files were moved byte-for-byte from the
reviewed MIT `crispcat` crate at `c1881142`. Public `crispcat::caf` and
`crispcat::index` names are re-exports of the new library, preserving type
identity. No format semantics or encoding guesses were changed.

Observed on macOS arm64:

- Independent manifest: 23 unit tests and one compiled documentation example passed.
- Existing consumers: 20 `crispcat` unit tests, two catalogue integration tests, one cross-crate API compatibility test and 11 CLI tests passed.
- An external Cargo project compiled using both crates and passed the new
  `FileIndex` directly to the existing API without conversion.
- `cargo package --offline --locked --allow-dirty` built and verified a
  10-file package, including this validation record.
- The current consumer retry needed local sibling-package metadata refreshed in the application workspace lock. The original lock was restored afterward; every registry version and checksum was unchanged.
- All registry versions/checksums were preserved. Unrelated changes caused by
  newer local ML sibling path-package versions were discarded; the application
  workspace lock adds only the codec package and consumer dependency.

These are codec/consumer checks, not a new Tauri/ML application release.
GitHub-hosted cross-platform checks and registry publication remain separate.
The existing MIT license and notices are retained.
