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

This is an experimental integration boundary, **not a registered app drive or UI**.
The app continues using `LocalDrive` for SMB mounts and its bounded mounted-file
copy path. Direct-SMB registration and credential UI remain a later rollout step
after real NAS/security tests and packaging approval.

```
cargo test --manifest-path crates/crisp-smb-source/Cargo.toml --locked
python scripts/test_smb_source.py /path/to/pinned/CrispCloud
```

The second command needs Python with `impacket==0.13.0` and checks the exact
CrispCloud revision before starting its shared loopback fixture. It verifies
bounded 3 MiB streaming without a whole-file buffer, upload, metadata,
cancellation, and refusal of required encryption on an SMB2-only server.
