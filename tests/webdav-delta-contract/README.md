This small harness compiles production delta calculations and staged in-place
WebDAV downloads without Tauri, OCR models or networking. Run with one Cargo job:

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
