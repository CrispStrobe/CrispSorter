# Isolated Windows pull-handle prototype

This crate compiles the Windows backend directly without compiling the app. The integrated candidate imports these same backend tests into the existing
`tests/storage-contract` harness, which is the native CI suite; this standalone
crate remains a small optional API check and is not an additional required suite.
Native execution is still required before merging the candidate.

The backend retains directory objects and uses one-component `NtCreateFile`
operations, native `FileRenameInformation` with the destination root handle,
and handle-based `FileDispositionInformation` cleanup. Only explicit root capture
uses an ambient path. A selected root alias is allowed; descendant symlinks,
reparse points, streams and traversal are rejected. The caller still applies the
app's Windows reserved-name policy before calling this low-level backend.
Ordinary directory handles request traversal/attributes/synchronization, not
DELETE or broad write rights. Only exclusive stage files request DELETE, which
is needed for atomic publication and owned cleanup. No temporary subdirectory or
new add-subdirectory ACL requirement is introduced for staging.

Stage ownership begins only after exclusive create succeeds. Pre-publication
cancellation deletes that owned file object, even if its name moved. Cleanup is
disarmed **before** rename: an error might be a lost acknowledgement after the
server published the file, so deleting the original handle could erase published
bytes. A failed/uncertain rename may leave a stage orphan. The caller must retain
its existing non-replayable-write behavior and must not unlink a pathname on Drop.
No leaf inode-CAS or protection against arbitrary same-user content edits is
claimed.

Cross-check (one job, no linker or app/native-library compilation):

```
cargo check --manifest-path tests/windows-pull-handles/Cargo.toml --tests --target x86_64-pc-windows-gnu
```

Required next step on native Windows NTFS, including normal writable ACLs:

```
cargo test --manifest-path tests/windows-pull-handles/Cargo.toml
```

Native tests cover short leaf replacement, collision ownership, cancellation,
replacement of the old staging name, post-rename lost acknowledgement, directory
and root relocation, root/descendant junction conversion after capture, no-follow
symlink/junction handling, and readonly/invalid destinations. Flag compatibility
for exclusive file/directory creation is exercised by the real syscalls. These
native tests have **not run** on this Linux host; a cross-check is not evidence
that Windows/SMB accepts the flags or supplies the required semantics.

UNC coverage is explicitly ignored by default. To execute it, configure
`CRISPSORTER_TEST_MOUNT_ROOT` to an accessible real UNC test directory (a loopback
administrative share is suitable if the runner permits it) and run:

```
cargo test --manifest-path tests/windows-pull-handles/Cargo.toml -- --ignored unc_or_verbatim_root
```

If that share is unavailable, report the UNC test as **unrun**; do not interpret
ordinary tests or cross-compilation as UNC coverage. No credentials, mount or
share configuration is created by this prototype.
