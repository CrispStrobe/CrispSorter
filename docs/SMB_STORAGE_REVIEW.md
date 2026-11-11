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

## Shared integration follow-up

The checkpoint above describes the incoming feature branch. The shared
integration additionally connects disk-bound GUI/CLI restore, sync pull and
index promotion to a file-target transfer queue. Each attempt writes to a
private temporary file beside its destination; byte-count validation and
cancellation checks precede publication. Failures leave existing destinations
intact and clean temporary files. Local writes now also use checked staging
instead of the legacy whole-file reader fallback. Existing capability flags
remain conservative; the standalone direct-SMB crate is still not registered.

Mounted-file FUSE reads use true seek/range access. Other FUSE providers use a
disk-backed LRU instead of cloning whole-file memory buffers. GUI byte previews
are capped at 64 MiB and provider content probes at 256 KiB; the file-target API
serves larger disk-bound operations. Cloud-backup shard import still uses its
existing byte-payload transport, and legacy provider internals can still buffer.

The source-bound storage contract and its execution/lock artifacts are wired
to Linux, macOS and Windows CI. This continuation workspace has no Rust
toolchain, so those native results must be read from the exact pushed commit's
CI run; no native success is inferred from the source review.
