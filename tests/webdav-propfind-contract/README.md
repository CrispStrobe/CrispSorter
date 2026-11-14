This small target compiles the actual production WebDAV PROPFIND parser and
its existing server fixtures and production parser regressions, without
Tauri or HTTP dependencies. It uses
quick-xml 0.38.4, the app's locked version. Regressions cover XML named/numeric
references, split text, CDATA, Unicode, significant displayname whitespace,
invalid references, ignored vendor properties, rejected truncated listings,
complete empty multistatus responses, and rejection of missing, unrelated,
nested or multiple document roots. Propstat regressions cover status before/after
properties, mixed success/failure blocks in either order, rejected malformed or
duplicate status, ignored nested vendor propstats/status/types, failed resource types,
and conflicting available type declarations across separate blocks.

Properties are buffered until propstat closes: only 2xx or legacy statusless
blocks contribute values. A failed explicit resourcetype (including an empty
one) requires an available type from another block, or the listing fails rather
than classifying an unknown directory as a file. Conflicting available types
across separate blocks also fail clearly. Optional failed metadata stays absent. Missing status, omitted resourcetype, and direct properties without
propstat (both response/prop/resourcetype and response/resourcetype) retain
legacy behavior; a statusless block cannot be distinguished from
a successful legacy server reply. Namespace URIs and response-level status are
not validated. This target does not exercise HTTP transport or compile the full
application.

Run from the repository root, reusing an inactive lightweight target directory:

    CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --locked --manifest-path tests/webdav-propfind-contract/Cargo.toml -- --test-threads=1
