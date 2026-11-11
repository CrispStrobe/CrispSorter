# SMB and mounted-storage review — 11 November 2026

## Current implementation
CrispSorter is Rust/Tauri, not Flutter. SMB/NFS/SFTP mounts are exposed as
`DriveType::Local` and `LocalDrive` in `src-tauri/src/drives/mod.rs`.
The reviewed Cargo manifests/lockfile and provider sources do not contain
libsmb2, dart_smb2 or a direct SMB client. Authentication, SMB dialect,
signing and encryption belong to the OS mount configuration. This says
nothing about the licenses of unrelated application dependencies.

## Priorities
1. Override the whole-file fallback for `read_file_to_writer` with bounded
   filesystem streaming, so mounted-share downloads do not allocate the entire
   file before delivering its first byte.
2. Keep errors from the destination writer observable. Test empty/large files,
   relative and absolute paths, and interrupted writers.
3. Keep mount availability, scan concurrency and metadata round trips in the
   performance qualification matrix; do not add a native SMB engine just to
   replace an engine that is not present.

The initial optimization covers reads only. The existing write-from-reader
fallback validates the declared size before modifying a destination; replacing
it safely needs staged publication/cleanup and filesystem-specific overwrite
semantics. Do not advertise full streaming capability until both directions
meet the contract.

## Direct SMB decision
Retain OS mounts for desktop use. If direct mobile SMB becomes a product
requirement, evaluate a permissively licensed pure Rust SMB2/3 implementation
behind CloudDrive, including signing/encryption, credential storage, complete
dependency-license review and target-platform tests. No new SMB dependency
or weaker mount security policy is introduced here.

## Implemented and validated on this branch
LocalDrive now overrides `read_file_to_writer` with `std::io::copy` through
the small `mounted_read` module. Four production-reader tests passed using
`bash scripts/test_mounted_read.sh`: an 8 MiB patterned source delivered in
bounded chunks, empty/missing sources, short destination writes, and immediate
propagation of a destination error. No Tauri/model downloads are needed for
this gate; a path-filtered workflow runs it on subsequent changes.

A relative/absolute LocalDrive dispatch test was also added to the application
suite, but the full Tauri suite was not run in this environment. No live mounted
NAS throughput measurement or OS security-policy qualification was performed.
Full streaming capability remains false because the upload fallback still
buffers; the read improvement does not claim both directions are bounded.

## Follow-up implementation

Explicit mounted/Rust settings are now shared by the app GUI and CLI. See
[SMB paths and tests](SMB_RUST_ROLLOUT.md) for current behavior and rollout gates.
The inventory above describes the pre-integration baseline.
