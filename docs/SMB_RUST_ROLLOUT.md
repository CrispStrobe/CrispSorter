# SMB paths in the GUI and CLI

CrispSorter uses Rust directly. It does not run Dart or a Flutter bridge.
Existing Local/OS-Mount drives retain their behavior. New **SMB** drives store an
explicit transport: `mounted` (default) or experimental `rust`. Both GUI and CLI
instantiate the same `SmbDrive` through `DriveRegistry::instantiate_with_proxy`.
SMB uses TCP directly and does not route through the HTTP proxy setting.

| Setting | Mounted path | Direct Rust path |
| --- | --- | --- |
| Backend | `mounted` | `rust` |
| SMB URL | Identifies the share and root | `smb://host[:port]/share/optional-root` |
| Mount folder | Required existing folder of that same share/root | Optional, retained for switching back |
| Credentials | Managed by the OS mount | Stored in the app's selected secret vault |
| Require encryption | Managed by the OS; app cannot enforce it | Required by default; explicit opt-out only |
| Fallback | Explicitly choose the other backend | Errors never retry through the mount |

## GUI

Open **Index → Drives → Add drive** (or edit an existing SMB drive), choose type
**SMB**, then choose **OS-Mount** or **Rust** in **SMB-Transport**. Enter a
credential-free SMB URL. For mounted access, choose the existing mount folder
that corresponds exactly to the URL's share/root; the app does not mount it or
verify that remote identity. For Rust, enter the SMB username (`DOMAIN\user` is
accepted) and password, and normally leave required encryption enabled.

Saving and editing use the same registry operation as the CLI. Blank credential
fields preserve stored credentials. Only non-secret settings are serialized into
`drives.json`; passwords never enter that file or app logs. Existing local drive
records are not silently converted to SMB. Changes apply to newly instantiated
operations; already running operations retain their session. Finish active
sync/indexing work before changing the source root or backend.

## CLI

The CLI works without a running GUI. Use the same app data directory and secret
vault as the GUI (the global `--data-dir` flag can select a different profile).

```sh
# Add an existing OS mount. Its root must represent this exact share/root.
crispsorter drives smb --label NAS --url smb://nas/books \
  --backend mounted --mount-path /Volumes/Books

# Inspect one SMB drive's settings; output is JSON without credentials.
crispsorter drives smb --id DRIVE_ID

# Select direct Rust and save credentials; reads one password line from stdin.
# Use an interactive stdin pipe or a secret-manager command, not a CLI password flag.
secret-manager read nas-password | crispsorter drives smb --id DRIVE_ID \
  --backend rust --username 'DOMAIN\reader' --password-stdin

# Switch back explicitly. Saved Rust credentials remain in the vault.
crispsorter drives smb --id DRIVE_ID --backend mounted
crispsorter drives list --json
crispsorter drives ls --drive DRIVE_ID / --json
crispsorter drives search --drive DRIVE_ID report --path / --json
```

To add Rust directly, combine `--label`, `--url`, `--backend rust`, `--username`
and `--password-stdin`. `--require-encryption false` is available for an explicit
legacy-server opt-out; neither surface retries without encryption automatically.
`--backend auto` and credentials in URLs are rejected. CLI input is bounded and
rejects multiple password lines. Omit `--password-stdin` to preserve an existing
password. Settings are validated before writing; a failed metadata save attempts
to restore the previous vault entry and retains the previous in-memory registry.

## Operations and logging

Both paths implement listing, metadata, bounded download/upload streams, inclusive
range reads, mkdir, rename, and deletion. `/` means the selected drive root on
both paths. Traversal, host filesystem prefixes, and mount symlink escapes are
rejected. `read_file` is capped at 64 MiB; use `read_file_to_writer` for larger
files. Streams use bounded buffers (Rust chunks at most 256 KiB). Mounted uploads
validate the declared size in a private staging file, then publish it; failure
preserves an existing destination. Direct Rust uploads create/truncate and can
leave a partial file on failure. Such failures are marked non-replayable: neither
the immediate transfer queue nor the durable offline queue automatically retries
them. If a queued mounted drive is changed to direct Rust, replay stops before
opening the provider and retains the failed record and payload for review.
Explicit manual retry remains available, subject to the current transport policy.
Direct upload atomicity is not advertised. The drive does not
advertise resumable uploads, resumable downloads, copy, versioning or trash
restoration. The lower-level core's resume API remains available for future jobs.

A dedicated runtime worker handles Rust protocol operations, allowing the sync
CloudDrive contract to be called inside Tauri's async runtime without a nested
runtime panic. File sink/source callbacks still run synchronously on the caller;
long operations should use the app's storage workers. Dropping a direct drive
cancels its session and lets the worker close its handles. Operations never replay
writes after errors. FUSE clamps its final-page requests to SMB's strict range
contract and returns an empty result at EOF, without materializing a whole file.
The shared core's nullable missing-path metadata is converted to a `NotFound`
error at both application wrappers; zero-byte files remain valid.

App logs (stderr plus GUI ring buffer/events) use the shared operation receipt
format: operation IDs, started/completed/failed phases, fixed stages/codes and
duration. They cover transport selection and security policy, settings saves and
rollback, transfers and metadata; debug stages record aggregate byte counts.
Lifecycle receipts use info and failures use error. Logs omit labels, hosts,
paths, usernames, passwords and raw upstream errors. There are no per-chunk logs.

## Tests and remaining rollout checks

The shared Rust core is pinned to CrispCloud
`4b44dc4d44ef05a18d2da2579d2e3b866b245643`; libsmb2 remains the default/fallback in
the Flutter consumers. Sorter's established fallback is the explicit OS mount.

```sh
cargo test --manifest-path crates/crisp-smb-source/Cargo.toml --locked
python scripts/test_smb_source.py /path/to/pinned/CrispCloud
npx svelte-kit sync
npm test -- src/lib/drives/smbSettings.test.ts
npm run check
npx playwright test tests/browser/smb-settings.spec.ts
```

The independent contract test compiles the **production** CloudDrive types,
SMB adapter, registry and CLI argument module. It replaces only the external
vault/log sinks with test fixtures. Coverage includes both transport contracts,
secret-free settings, invalid CLI input, GUI/CLI persistence, rollback, bounded
buffers/ranges, root handling, symlink escape, writer failure, and construction
and drop inside Tokio. Added integration regressions cover preservation of
existing mounted files after failed input, structured receipts, typed no-replay
failures, missing versus zero-byte metadata, and FUSE final-page/EOF behavior.
The live fixture requires `impacket==0.13.0` and verifies
real authenticated SMB2 transfers and refusal when SMB3 encryption is required.
The GUI form's settings mapping has Vitest coverage. Full Tauri builds, successful
encrypted SMB3/NAS transfers, platform sandboxes and final artifact license notices
remain rollout gates; these fixture tests do not establish those results.

The browser regression drives the actual Svelte add/edit form and verifies its
IPC payloads and restored selection. Its IPC backend is a fixture; the Rust
contract test covers the real persistence path. Local Chromium launch is blocked
by this environment's socket restrictions, so the browser regression runs in CI.

Local full-app `cargo check` reached the platform dependency build and stopped
because `glib-2.0` development files are absent. The workflow also includes an
app type-check job with the GTK/WebKit development packages and pinned sibling
repositories, so glue code outside the independent contract is checked in CI.
