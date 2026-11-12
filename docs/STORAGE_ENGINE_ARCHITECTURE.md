# Shared storage engines and CrispSorter's role

Recorded on 11 November 2026. CrispSorter's native Filen/Internxt and selectable
SMB paths are implemented. Reusing the Filen/Internxt Rust clients from Flutter
is proposed work, not an existing Dart bridge or a new Sorter engine setting.

The cross-app contract, fallback policy and migration sequence are recorded in
[CrispCloud's shared design](https://github.com/CrispStrobe/CrispCloud/blob/95f4574e58cd69881532603eb48689d43dfe8ead/docs/STORAGE_ENGINE_ARCHITECTURE.md).
This pins a documentation revision; runtime sources remain pinned separately.

## Current implementation

| Surface / provider | Current implementation |
| --- | --- |
| GUI | Svelte/TypeScript invokes the Rust/Tauri application backend |
| CLI | Rust; can use the same app data directory, registry and selected secret vault without a running GUI |
| SMB | `SmbDrive` with explicit `mounted` (default) or direct `rust` selection |
| Filen | `NativeFilenDrive` → `crisp-filen` Rust library |
| Internxt | `NativeInternxtDrive` → `crisp-internxt` Rust library |
| Provider-independent application API | Synchronous/object-safe `CloudDrive`, capabilities and shared drive construction |
| Dart / Flutter runtime | No dependency for these provider operations |

[`desktop` in Cargo.toml](../src-tauri/Cargo.toml) includes
`drive-filen-native` and `drive-internxt-native`. The release recipes enable
these native providers. A minimal build with those features absent reports the
provider unavailable; mobile builds need explicit features and target/device
qualification. The old Python subprocess drives have been removed. WebDAV can
still access a separately configured server, but is not an automatic fallback
to a local Python daemon.

Current reusable libraries and application adapters are separate:

| Library | App integration |
| --- | --- |
| [`crisp-filen`](../crates/crisp-filen-native/README.md) | [`filen_native_drive.rs`](../src-tauri/src/drives/filen_native_drive.rs) |
| [`crisp-internxt`](../crates/crisp-internxt-native/README.md) | [`internxt_native_drive.rs`](../src-tauri/src/drives/internxt_native_drive.rs) and compatibility re-exports |
| [`crisp-smb-source`](../crates/crisp-smb-source/README.md), consuming shared `crisp_smb` | [`smb.rs`](../src-tauri/src/drives/smb.rs) |

The SMB core is pinned to CrispCloud commit
`13ca681f0c706cf24e65c7906f8a98e20421c590`. The Rust Filen/Internxt clients are
currently local path dependencies; their reuse by other applications still
needs versioned source packaging and adapters. `crates/internxt-core` is an
independent MIT test reference, not the production Internxt provider.

## Target across the three apps

Keep provider/session/crypto code reusable as Rust libraries. Sorter consumes
those libraries directly; CrispCloud and CrisperPlayer can consume the same
source through Dart FFI/bridge adapters. Do not copy another protocol engine
into each application or embed a Dart VM in Sorter just to obtain fallback.

Cloud and Player currently use Dart for Filen/Internxt. Cloud's standalone CLI
has no Filen/Internxt adapter yet. The proposed migration first establishes a
common provider contract and GUI/CLI parity, then adds optional Rust-backed
Filen and Internxt adapters to the Flutter apps. Retaining their existing Dart
clients during qualification does not imply that Sorter acquires a Dart backend.

The host application retains registry/settings, secure-store integration,
persistent job scheduling, conflict/publication policy and UI/log presentation.
The provider owns its protocol, account-scoped session, file crypto, bounded
chunk operations, capability/error mapping and validated resume primitives.
Host job retries and provider chunk retries must have coordinated bounds.

The native clients use blocking HTTP. Run long transfers on storage workers,
with bounded readers/writers and cancellation; do not call blocking operations
directly from an async/UI executor. The direct SMB adapter already has a
dedicated runtime worker for the synchronous `CloudDrive` interface. Reusing
code through a bridge still requires worker-lifetime and shutdown tests in the
Flutter consumer.

## Selection, fallback and settings

Current Sorter SMB modes are `mounted` and strict `rust`. Neither automatically
retries through the other after an error. Mounted SMB security is owned by the
OS; direct Rust requires encryption by default. See
[SMB_RUST_ROLLOUT.md](SMB_RUST_ROLLOUT.md) for actual GUI fields, CLI flags,
bounded transfers, logs and remaining qualification.

Native Filen/Internxt currently have no Dart/Rust engine selector in Sorter.
Proposed `dart` / `rust` / `auto` choices apply to the future Flutter integration,
with Dart initially retained there. They are not new `crispsorter` options.

The shared policy permits Auto selection only before work/authentication starts
when the selected engine is unavailable or a known unsupported capability can
be handled by an eligible alternative under the same security policy. Login,
renewal, network, crypto/integrity or transfer errors must not trigger engine
switching. Never replay a mutation through another engine after it may have
reached the provider. Engine selection remains fixed for an active transfer.

Native sessions and resume checkpoints are provider/account/key/chunk-specific.
Version them and verify object identity/validators. An explicit engine change
must reject or safely restart incompatible state, not append bytes under another
engine's checkpoint. Dart and Rust session JSON must not be presumed compatible.

GUI/CLI settings must have the same behavior for a given registry and build.
SMB already uses a locked shared save transaction, non-secret metadata in
`drives.json`, vault rollback on write failure and original-GUI-snapshot checks.
Document equivalent concurrency, account-lifecycle and active-operation behavior
when adding shared provider configuration; do not silently change a running job.

Use the application logger for requested/effective engine, settings conflicts,
operation result/duration and aggregate transfer totals. Do not add per-chunk
noise, credentials, keys, serialized sessions, private paths or raw upstream
errors. SMB's adapter already implements this logging contract; the broader
provider bridge design is not proof of equivalent coverage for every provider.

## Browser, packaging and licenses

Tauri's webview GUI calls the native Rust process; it does not establish a
standalone browser provider backend. The clients' blocking HTTP interface is
not browser WebAssembly support. The Flutter Dart Filen entry point also imports
`dart:io`. Browser HTTP/crypto/CORS or an explicit gateway needs a separate
implementation and test target. Mobile requires native feature selection,
bundling, sandbox checks and real devices.

`crisp-filen` and `crisp-internxt` declare MPL-2.0, as do the existing Dart
Filen/Internxt clients. MPL is file-level copyleft, not an MIT/Apache/BSD-style
permissive grant; see the [Mozilla FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/).
`crisp-smb-source` and the independent shared SMB core declare MIT OR Apache-2.0;
their dependencies retain their own grants. `internxt-core` is MIT/test-only.

Changing language or adding a bridge does not relicense a component. Preserve
notices and review exact source/dependency/build/artifact evidence when sharing
the crates. Authority over original code does not change third-party grants.
The Flutter apps retain LGPL libsmb2 as their SMB default/fallback; Sorter does
not add libsmb2 or a Dart runtime through this design.

## Tests and rollout

Keep native unit/hermetic coverage and independent crypto vectors in the reusable
libraries. Run one contract against Dart and Rust adapters in the Flutter
consumers, including Dart upload → Rust download and the reverse. Cover supported
Filen metadata/crypto versions, Internxt session renewal/2FA/revocation, empty and
large transfers, range reads, cancellation, account changes, damaged data,
changed sources, disk/short-write failures and incompatible resume state.

App tests must exercise GUI/CLI persistence, capabilities, invalid selections,
native-library absence, strict/Auto rules and active-operation guards. Keep logs
and IPC output secret-free. Compare throughput, CPU and memory using identical
provider/files/security settings; existing Dart crypto can already use native
backends, so Rust is not itself a speed measurement.

Existing native and SMB entry points include:

```sh
cargo test -p crisp-filen --lib
cargo test -p crisp-internxt --lib
cargo test --manifest-path crates/crisp-smb-source/Cargo.toml --locked
```

For authenticated SMB fixtures, actual Svelte form/IPC coverage and settings
tests, follow [the SMB guide](SMB_RUST_ROLLOUT.md). For provider mutation, queue,
proxy, capability and opt-in live-test boundaries, follow
[cloud-provider-parity.md](cloud-provider-parity.md). Live accounts must be
explicitly configured; hermetic tests must not read real credentials.

Keep both existing language packages supported and tested. Flutter consumers
should offer a per-provider app default plus per-account App default/Dart/Rust/Auto
choices. Initially use Dart; a qualified default flip to Auto prefers Rust with
pre-session Dart fallback. Explicit overrides and active transfers keep their
engine. Test default changes and checkpoint compatibility through GUI and CLI.

Sorter continues to call Rust directly. A Dart choice here would need an explicit
Dart worker/runtime, with desktop packaging and platform tests; keeping the Dart
package does not automatically add that runtime. That is a separate compatibility
feature if needed. Removing either existing engine would be a separate decision,
not an assumed outcome of sharing the Rust sources.


## Executable Dart / Rust package conformance

`.github/workflows/provider-conformance.yml` runs the shared, pinned
[provider conformance harness](https://github.com/CrispStrobe/crisp-cloud-rs/tree/0aaa6666a6108f4d4d29877a34afe6ec428e0467/conformance).
It tests this repository’s actual native Filen/Internxt crate sources against
the retained Dart SDKs, without replacing the Sorter crates.

The same independent fixtures run through real package APIs and compiled Dart
CLI probes with software and native AES backends. Checks include password/key
derivation, metadata, empty and boundary-sized chunks, CTR streaming and ranges,
wrong keys, damaged data, Unicode, pagination, cache reuse, HTTP failures,
read retry and create/rename/move/trash payloads. Each engine decrypts the other
engines’ exports. Loopback mocks validate authentication and wire requests;
mutations denied with 403 must not be replayed. No live account is used.

Both GUI and CLI use these package formats. Package parity does not qualify an
app bridge, GUI settings, session migration, active-job engine selection, native
mobile packaging or every transfer/resume/cancellation path. Keep the existing
surface and provider tests, and the rollout gates above, before changing defaults.

CI publishes `report.md`, `report.json`, wire traces and resolved probe lockfiles.
Unexpected differences fail. Exact reviewed gaps remain visible and set
`promotion_ready` to false, even when both implementations share a permissive
parse bug. `--strict` fails on those gaps too. A green regression baseline is
not permission to switch defaults. Legacy text-secret encoding requires a
compatibility decision before any format change.

Reproduce from a checkout of the pinned harness, with Python 3.12+, stable Rust
and Dart 3.13.7. From this app repository’s root:

```sh
python -m pip install cryptography==50.0.0
python ../crisp-cloud-rs/conformance/run.py --pointycastle 4.0.0 --dart-pg 2.1.0 --rust-filen crates/crisp-filen-native --rust-internxt crates/crisp-internxt-native
# Add --strict for the default-promotion gate.
```

Keep this harness commit pinned and advance it deliberately with the compatibility
report. Retain both language implementations and explicit engine choices.


The first completed shared run exercised 131 cases and 363 cross-decryptions
with zero unexpected failures. Ten exact diagnostic cases remain promotion
blockers: empty CBC encryption, CBC envelope validation, two legacy secret
encoding directions, two negative CTR offsets, odd-length password salt hex,
unreadable Filen metadata, missing Internxt listing collections and listing
GET retry behavior. These are SDK compatibility findings, not live-account or
GUI qualification. Dart retries an Internxt listing 503; Rust currently returns
the error. Rejection is compared at the API boundary; exception classes/messages
are language-specific. See each CI run’s report for its actual source results.

The provider conformance workflow also runs native unit/mock tests in an isolated
workspace containing the unchanged provider crate sources and their existing
MIT reference dependency. Its resolved test lockfile is uploaded separately;
this avoids pulling unrelated desktop build dependencies into a protocol test.
