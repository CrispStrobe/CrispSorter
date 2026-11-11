# SMB and mounted-storage review — 11 November 2026

## Current implementation

CrispSorter uses a Rust/Tauri backend and Svelte/TypeScript GUI. It has no Dart,
Flutter bridge or libsmb2 dependency for SMB. Existing Local drives can still
refer to OS-mounted SMB/NFS/SFTP paths through `LocalDrive`. Explicit SMB drives
use `SmbDrive` with `mounted` (default) or experimental direct `rust` selection.

The direct core is the same versioned `crisp_smb` source used by the Flutter
consumers, pinned at CrispCloud `13ca681f0c706cf24e65c7906f8a98e20421c590`. The
Flutter apps retain libsmb2; Sorter retains explicit OS-mount access. Errors do
not automatically switch paths. OS-mounted signing/encryption policy belongs
to the OS; direct Rust requires encryption by default. See the actual settings
and commands in [SMB_RUST_ROLLOUT.md](SMB_RUST_ROLLOUT.md).

## Implemented behavior

LocalDrive reads use bounded filesystem copying with observable destination
errors. Its legacy whole-file upload fallback is separate from the explicit SMB
adapter and must not be advertised as bounded in both directions.

The explicit SMB adapter implements bounded reads/writes, range reads, metadata,
listing, directory creation, rename and deletion. Both GUI and CLI instantiate
the same adapter. Root/path checks reject traversal, host paths and mounted
symlink escapes. Uploads create/truncate and can leave partial files on failure;
callers needing atomic publication must stage/publish explicitly.

The registry saves non-secret metadata atomically, uses a separate process lock,
merges unrelated drive changes, rejects stale edits, checks the original GUI form
snapshot and restores prior credentials when metadata publication fails. App
logs record safe settings/operation outcomes and aggregate transfer totals.

## Evidence and remaining qualification

Tests cover mounted streaming, empty/missing sources, short writes, destination
errors, authenticated Rust SMB2 operations, bounded ranges/transfers, root and
symlink checks, CLI input, GUI/CLI persistence, lock contention, stale edits,
credential rollback and legacy credential migration. The actual Svelte form/IPC
regression and application type check passed in the implementation CI run linked
from [the rollout guide](SMB_RUST_ROLLOUT.md).

These results do not establish real mounted NAS throughput, successful required
SMB3 encryption, mobile devices or final packaged release behavior. Preserve the
explicit transport/security choices while completing those gates.

## Filen/Internxt and language sharing

Sorter already uses native Rust Filen/Internxt clients when their provider
features are enabled; `desktop` includes both. Their reuse through a Dart bridge
in Cloud/Player, the fallback/compatibility rules and license differences are
recorded in [STORAGE_ENGINE_ARCHITECTURE.md](STORAGE_ENGINE_ARCHITECTURE.md).
That migration is proposed and does not add a Dart VM or provider engine selector
to Sorter.
