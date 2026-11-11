# Experimental direct SMB source

This standalone MIT OR Apache-2.0 crate consumes the same `crisp_smb` core as
CrispCloud and CrisperPlayer, pinned to an immutable CrispCloud commit. There is
no Flutter, Dart, FRB, libsmb2 or Samba dependency in this crate.

`SmbSource` exposes asynchronous metadata, bounded streaming reader/writer
adapters, native-to-disk resume, and session cancellation. Paths are relative to
`Config.root`. The caller supplies credentials in memory and owns secure storage.
Use staging files and publish only after success. Uploads create/truncate and are
not atomic or resumable. Blocking `Read`/`Write` sinks must run on a storage worker,
not the Tauri UI executor. The native core serializes each session's operations.

The app now registers an explicit SMB drive with mounted and direct Rust paths.
`BlockingSession` owns a dedicated runtime worker for the synchronous CloudDrive
API. Both GUI and CLI save the same `Settings` through `DriveRegistry::save_smb`.
See [GUI/CLI settings and rollout](../../../docs/SMB_RUST_ROLLOUT.md) for behavior,
logs and limits. The app contract tests include production source modules and
use in-memory fixtures only for the external vault and log sink.

At the app boundary, mounted uploads use checked staged publication. Direct
Rust writes remain non-atomic and their failures disable automatic transfer and
offline replay. Both the async source and blocking app wrapper convert the core's
nullable missing-path stat response into `NotFound` rather than successful empty
metadata.

```
cargo test --manifest-path crates/crisp-smb-source/Cargo.toml --locked
python scripts/test_smb_source.py /path/to/pinned/CrispCloud
```

The second command needs Python with `impacket==0.13.0` and checks the exact
CrispCloud revision before starting its shared loopback fixture. It verifies
bounded 3 MiB streaming without a whole-file buffer, upload, metadata,
cancellation, and refusal of required encryption on an SMB2-only server.
