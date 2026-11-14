This small target compiles the actual production WebDAV PROPFIND parser and
its two existing server fixtures and production entity/truncation regressions, without
Tauri or HTTP dependencies. It uses
quick-xml 0.38.4, the app's locked version. Regressions cover XML named/numeric
references, split text, CDATA, Unicode, significant displayname whitespace,
invalid references, ignored vendor properties, rejected truncated listings,
and complete empty multistatus responses. It does not exercise HTTP
transport or compile the full application.

Run from the repository root, reusing an inactive lightweight target directory:

    CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo test --locked --manifest-path tests/webdav-propfind-contract/Cargo.toml -- --test-threads=1
