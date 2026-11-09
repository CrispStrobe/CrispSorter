# Integrity of reusable OCR page images

The shared image importer names each normalized PNG snapshot by the SHA-256
of its complete encoded bytes. A filename alone is not proof that a cached
image is intact. When publishing a freshly encoded page encounters an existing
file, the importer now verifies that the file is a regular file at its canonical
private path, has the expected length, and matches the complete new snapshot.

Comparison uses two fixed 64 KiB buffers instead of loading both encoded images
into memory. It also checks for unexpected extra bytes after comparison. A
truncated, oversized, changed, or symlinked existing asset causes an explicit
import failure. Verification does not replace the damaged file or modify the
new snapshot. The asset directory cannot redirect publication through a
symbolic link. Original document files and raw-import archives are retained.

This closes a path where an existing content-addressed filename was previously
accepted without checking its bytes. It applies to the shared image path used
by ordinary camera JPEG/PNG imports as well as desktop files. The mobile native
document importer already verifies the native pages it adopts separately.

## Executed production-helper qualification

[Run 42621716073](https://github.com/CrispStrobe/CrispSorter/actions/runs/42621716073)
tested source `715cfc14aaeb88cd0774df7585d5d3ab77d864fe` on Ubuntu 24.04.5,
using Rust 1.99.1. Its focused crate includes the actual production
`ocr_asset_store.rs` and JPEG source guard by path. All nine tests passed:
the six earlier JPEG controls and three asset-integrity cases.

| Executed asset case | Result |
| --- | --- |
| Reuse of an identical 200,003-byte snapshot across multiple chunks | Accepted; both byte streams remain unchanged. |
| Truncated, oversized, or same-length corrupted existing snapshot | Rejected; neither existing nor expected bytes are replaced. |
| Existing asset symlink, asset-directory symlink, or non-file destination | Rejected; the outside target remains unchanged. |

The artifact `ocr-image-input-contracts`, ID `13678452314`, is 2,907 bytes.
It was downloaded and its SHA-256 verified as
`cc33a0585cac4321850ecbdd9e5f71a95354b624d65289e638c010b9db5989bf`.
Its recorded production-source hashes match the saved source. Registry versions
and checksums in the focused crate were checked against the repository
`Cargo.lock` before the tests ran. The
[qualification receipt](../tests/results/ocr-asset-integrity/qualification.json)
records the source, artifact, test names, and scope.

## Application and device scope

The same source also adds full application regressions that import the pinned
oriented JPEG, damage or alias its stored PNG, and repeat both file and raw-byte
imports. Those cases assert original/archive preservation, absence of extra
published assets, and successful deduplication after the valid PNG is restored.
They require the complete application test binary and are not covered by the
focused nine-test result above. Native application qualification tracks them
separately.

This is an integrity check when a snapshot is reused, not a background scan or
automatic repair service. The focused host result does not qualify a physical
camera, mobile document picker, or cloud-provider transfer.
