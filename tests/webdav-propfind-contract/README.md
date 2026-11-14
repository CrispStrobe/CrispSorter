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
and conflicting available type declarations across separate blocks. Response-level
regressions cover failed resources in either listing order, 2xx status-only
responses, malformed/empty/duplicate statuses, ignored vendor subtrees (including
nested responses), and independent response/propstat outcomes.

Properties are buffered until propstat closes: only 2xx or legacy statusless
blocks contribute values. A failed explicit resourcetype (including an empty
one) requires an available type from another block, or the listing fails rather
than classifying an unknown directory as a file. Conflicting available types
within a successful or legacy statusless propstat, as well as across separate
blocks, also fail clearly. Each declaration is classified independently;
consistent duplicates remain accepted, while conflicts inside failed propstats
are ignored if an available type exists. The focused production parser tests
cover both declaration orders, empty/paired tags, split prop elements, status
position, foreign subtrees, and state reset between blocks/responses. Optional failed metadata stays absent. Missing status, omitted resourcetype, and direct properties without
propstat (both response/prop/resourcetype and response/resourcetype) retain
legacy behavior; a statusless block cannot be distinguished from
a successful legacy server reply. Direct response statuses are read only at
multistatus/response/status and must be valid 2xx status lines. A failed resource
fails the whole listing rather than silently returning incomplete results. For
compatibility, a 2xx direct status may coexist with propstats; it does not override
a failed property status or unavailable resource type. Namespace resolution accepts the DAV: URI with any prefix or default namespace,
including nested rebinding and restoration. Explicit foreign URI elements and unknown
prefixes are opaque; fields must also occur at recognized DAV property paths, so
DAV-looking children inside vendor subtrees cannot overwrite metadata. Unprefixed,
no-namespace elements remain accepted per element for legacy and mixed server
replies, including namespace resets. This compatibility tradeoff filters foreign
URIs rather than enforcing strict namespaces; it is not full XML/schema validation.
This target does not exercise HTTP transport or compile the full application.

Run from the repository root, reusing an inactive lightweight target directory:

    CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --offline --locked --manifest-path tests/webdav-propfind-contract/Cargo.toml -- --test-threads=1
