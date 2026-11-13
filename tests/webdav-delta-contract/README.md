This small harness compiles production delta calculations and staged in-place
WebDAV uploads/downloads without Tauri, OCR models or networking. The harness
extracts both production transfer methods and supplies a fake transport. Run
with one Cargo job:

    CARGO_BUILD_JOBS=1 cargo test --locked --manifest-path tests/webdav-delta-contract/Cargo.toml

All changed ranges are fetched, size checked and hash verified in a private,
automatically cleaned temporary file before any destination write. Only changed
blocks are staged, so local temporary disk use equals downloaded changed bytes,
rather than the whole original file. Stage creation/write failures return before
any destination mutation. Memory uses fixed 64 KiB copy/hash buffers plus range
results capped at 4 MiB, independent
of advertised block sizes; block-map/index metadata remains proportional to the
number of blocks.

Commit writes only changed blocks and the final length through the same opened
read+write destination handle used to inspect original bytes. This retains its
inode, ownership, ACLs, custom attributes and hardlink relationships, subject to
normal filesystem write behavior (for example, clearing privileged file bits).
It never publishes a replacement inode or copies security metadata.

This is **not atomic**. Local commit I/O errors, process interruption and
concurrent in-place writes can leave partial changes. Observed edits before
commit are rejected, but the hash recheck is not a lock. Initial pathname
inspection/open follows existing symlinks and resolves ancestors, retaining the
previous behavior. It is not an atomic no-follow open.
Once opened, later path replacement cannot redirect commit: a moved original
file remains the selected object. No pathname-race fix is claimed.

Upload maps are checked for ordered, contiguous offsets, exact block lengths
and complete file coverage before opening or hashing the local source. Diffing
also validates both layouts, and downloads use the same validator. Regression
tests exercise matching hashes with malformed offsets, sizes, coverage and extra
blocks; the extracted upload method sends neither blocks nor finalize on invalid
input. A valid changed-upload control verifies the fake transport records both.

Upload hashing uses a fixed 64 KiB scratch buffer even for u32::MAX advertised
blocks. Each changed block is copied to a private temporary file, checked against
its local block signature, then sent using an owned body. Local EOF/read errors,
staging failures and detected changes fail before that block POST. Temporary
disk space can reach one changed block; cleanup occurs on all return paths.
Bodies through 4 MiB stay replayable; larger bodies stream with an exact length.
Unhandled redirects fail before finalization. Earlier successful POSTs remain
possible on later failures; skipped source blocks are not rechecked, so no atomic
source snapshot or remote transaction is claimed.

The extracted-method tests use a **stub transport**, not a real HTTP client or
server. They read the body through bounded buffers, check exact bytes/length,
and inject 307/308 responses to check failure before finalization. They do not
verify reqwest redirect replay or real HTTP headers. Real-client mockito tests
in `src-tauri/src/drives/webdav.rs` cover body/content-length/If-Match and actual
small-body replay versus large-body refusal; those require the application test
build. Block-map JSON parsing and metadata storage remain unbounded.
