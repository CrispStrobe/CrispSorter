# Windows ONNX Runtime release compatibility

The failed Windows standard-release job built source
`b739a3c0b70f39d3ae19e1bfcb412f3b44f77794` with newly installed Rust 1.100.0.
Locked `ort-sys 2.0.0-rc.11` assumes its output directory is
`target/release/build/<package>-<hash>/out` and walks three ancestors to locate
the profile directory. Cargo 1.100 instead supplied
`target/release/build/ort-sys/<hash>/out`. Its first successful DLL staging log
therefore named `target/release/build/DirectML.dll`; the next attempt used the
missing `build/examples` parent. Symlink creation failed and the copy fallback
panicked at `dynamic_link.rs:42` with Windows error 3. This was after the ORT
download; it was not a missing download or native library link diagnostic.

The desktop release recipe now selects Windows Rust **1.99.1** and creates all
three DLL destinations (`release`, `release/examples`, `release/deps`) before
native tests. Other runners keep stable Rust. This restores DLL placement beside
the actual application/tests, rather than hiding the panic by creating parents
under the incorrect `release/build` destination. No dependency pin, features,
runtime archive, packaging gate or concurrency block changes.

The [official release announcement](https://blog.rust-lang.org/releases/latest/)
and [versioned distribution manifest](https://static.rust-lang.org/dist/channel-rust-1.99.1.toml)
identify the October 15 release and available Windows compiler/Cargo components.
The Cargo 1.99.1 [layout source](https://doc.rust-lang.org/stable/nightly-rustc/src/cargo/compiler/layout.rs.html)
documents the legacy default, with the new layout still opt-in. The offline
index audit of the existing lock covered every registry package: 872 declared
an MSRV (highest 1.98), and 464 did not declare one. This is metadata compatibility
evidence, not a successful Windows compilation. The
[retained source/layout evidence](../tests/fixtures/windows-ort-layout/evidence.json)
binds the upstream DLL-copy source to the locked crate checksum and records the
official manifest hash. Focused controls reproduce the observed incorrect path
and missing-parent copy failure without compiling or downloading ORT.

The manual `platform: windows_original_b739` selector now performs an original-
source Windows retry using corrected workflow tooling. It requires
`publish: false`, `mac_appstore: false`, and `mobile_profile: standard`. Both
preflight and build check out the exact original `b739` application commit;
`_release_tooling` separately checks out the current workflow commit and verifies
it against `GITHUB_WORKFLOW_SHA`. It dynamically requires exactly one existing
`v0.15.1` draft, an exact original source target, and matching tag resolution.
It never creates a draft or accepts a mutable branch target. No draft numeric
identifiers or execution identifiers are pinned or emitted by the retry helper.
Before either Windows checkout, the workflow disables Git's automatic CRLF
conversion so committed app, manifest and retry-pin bytes remain exact. A real
Git clone regression exercises both the conversion failure and the configured
checkout without changing source commit identities.

The committed retry lock binds original source/tree, version, Cargo/runtime
locks and manifest hashes. The guard also compares the Windows features,
assets and native sibling refs with the corrected workflow recipe. It rechecks
the existing draft before package mutation and before receipt upload. The
verified Windows receipt keeps `app_source` at the original commit, adds separate
workflow source/tree provenance, and rechecks the actual portable archive hash.
The retry skips the normal stale-installer cleanup entirely: it deletes or
retargets no draft assets and may replace only its exact versioned Windows
portable ZIP, Windows receipt and matching checksum file. The pre-upload guard
rejects other names/locations and symlinks; sealing checks the final checksum
file against the same portable bytes.
Other-platform and TestFlight receipts are neither rebuilt nor relabeled; only
the Windows job can run for this selector. Publication remains disabled and all
existing concurrency blocks remain unchanged. The selector and mismatch controls
are prepared locally; no retry has been dispatched or rebuilt.
