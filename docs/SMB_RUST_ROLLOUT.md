# Direct Rust SMB qualification

Keep OS-mounted SMB through `LocalDrive` as the app default. Its existing
`read_file_to_writer` now streams a bounded copy rather than allocating an entire
file. This is the immediate performance change for sorting/indexing mounted NAS
content.

`crates/crisp-smb-source` demonstrates direct native reuse of the shared Rust core
from CrispCloud `ffeae11d53054190cf6f3a6fbcec2eb44b32dfb2`. It provides bounded
reader/writer methods, metadata and native resumable downloads. It is isolated
from the main workspace so it can be checked without unrelated translation,
embedding and OCR sibling repositories. No Dart runtime or FFI bridge is used.

The core defaults to the caller's required encryption policy, requires signing,
disables DFS and mutation replay, and stores no credentials. Require encryption
for real connections. Cancellation terminates the session; a new session can
resume a staging file with a matching file-version marker. Source grants are
MIT/Apache/BSD/similar; the pinned shared bridge repository includes exact Cargo
license evidence. The consumer's Cargo.lock pins its own smaller dependency graph.

The shared fixture test exercises a 3 MiB stream with a writer that rejects chunks
larger than 256 KiB and verifies every byte, plus upload, metadata, cancellation
and fail-closed encryption refusal. It proves reuse and bounded transfer behavior;
it does not prove successful encrypted SMB3 transfers on real NAS devices.

Before exposing a direct-SMB drive in the app: connect it to `CloudDrive` and the
credential store, run blocking file sinks on a storage worker, add explicit
capabilities and cancellation ownership, qualify Windows/Samba/Synology/QNAP,
check platform sandboxes/entitlements, and complete native artifact/notices review.
A failed direct-SMB authentication must not silently try an OS mount with other
credentials. No drive registration/UI or credential migration is performed by
this prototype.

## Validation recorded for this checkpoint

The independent consumer compiled against the remote pinned core. Its live
authenticated fixture test passed, as did all four mounted-read tests. The full
Tauri app build and real SMB3/NAS tests were not run. OS mounts and current drive
registration remain unchanged.
