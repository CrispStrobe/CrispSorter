This small harness compiles production delta calculations and staged in-place
WebDAV uploads/downloads and delta metadata without Tauri, OCR models or
networking. The harness extracts production transfer/status/block-map methods
and metadata types, and supplies a fake transport. Run
with one Cargo job:

    CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --locked --manifest-path tests/webdav-delta-contract/Cargo.toml -- --test-threads=1

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
build.

Delta status and block-map JSON are read through an 8 KiB scratch buffer before
parsing, with decoded response budgets of 64 KiB and 32 MiB respectively. Actual
bytes read determine the limit; Content-Length never sizes an allocation. The
reader requests at most the budget plus one byte and stops as soon as it exceeds
the budget, including for chunked, close-delimited or decompressed responses.
These are input-byte limits, not total peak RAM limits: Vec capacity, parsed
strings/signatures and subsequent local/remote maps also occupy memory.
Unknown JSON fields still count toward the byte limit.

The block-map budget allows compact maps with 100,000 signatures (over 390 GiB
at the usual 4 MiB blocks), covered by a valid-input regression. File size alone
is not limited. Larger or unusually verbose maps are explicitly refused with
the decoded limit and advice to use a full transfer. This returns an error; the
caller/user must select a full transfer, rather than receiving an automatic
fallback. Normal app detection, map 404, HTTP errors, JSON errors and map-shape
checks keep their existing branches.

`metadata_contract.rs` tests the extracted production metadata methods with
real serde parsing and a stub transport. `metadata_http.rs` separately tests
the production reader against a real reqwest client and a local TCP server:
valid and oversized fixed/chunked/close-delimited bodies, inflated declared
length with truncation, parser-rejected extreme lengths, and a small gzip body
that expands beyond the decoded budget. HTTP framing defines body length;
bytes beyond an undersized Content-Length are not claimed to be detectable.
These reader checks do not compile the complete WebDAV module.

To run the HTTP checks without a new dependency build, supply an existing
compatible reqwest rlib built with `blocking` and `gzip`, and its dependency
directory (for example from the oauth-proxy-contract harness). From repo root:

    REQWEST_RLIB=/path/to/debug/deps/libreqwest-HASH.rlib
    REQWEST_DEPS=/path/to/debug/deps
    rustc --edition=2021 --test tests/webdav-delta-contract/metadata_http.rs -L "dependency=$REQWEST_DEPS" --extern "reqwest=$REQWEST_RLIB" -o /tmp/webdav-metadata-http-tests
    /tmp/webdav-metadata-http-tests --test-threads=1
    rm /tmp/webdav-metadata-http-tests

The reader's std-only tests can also run without Cargo or reqwest by compiling
a temporary wrapper that declares the production metadata file as a module:

    printf '#[path="%s/src-tauri/src/drives/webdav_delta_metadata.rs"] mod metadata;\n' "$PWD" > /tmp/webdav-metadata-std.rs
    rustc --edition=2021 --test /tmp/webdav-metadata-std.rs -o /tmp/webdav-metadata-std-tests
    /tmp/webdav-metadata-std-tests
    rm /tmp/webdav-metadata-std.rs /tmp/webdav-metadata-std-tests

The dependency-free streaming import regression can also run without Cargo or
an application build:

    python3 tests/webdav-delta-contract/test_import_scope.py -v

It compiles the exact production `read_file_to_writer` method with every actual
module-scope `std` import from `webdav.rs`. Its fake HTTP/application helpers use
fully qualified I/O traits, so they cannot supply a missing `Write` import. The
compiled fixture checks successful streaming/counts, HTTP rejection before writes,
and writer-error context. A negative control removes `Write` in temporary source
and requires rustc's E0405 missing-trait error. All generated source/binaries are
removed automatically. This checks the production method's standard-library name
resolution; it does not compile the complete module or validate real HTTP behavior.
