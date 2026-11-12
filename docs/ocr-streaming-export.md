# Streamed reviewed PDF export

This implementation is integrated into `feat/ocr-document-studio` at
`b876fb43eb19cc7de44760400e50735aa27c2c78`. It was qualified against the
pre-release CrispEmbed commit `9c44a69cad0d949e26a200e2bda1ccd58e3f9b8a`; the
native runtime and bindings now both pin the CrispEmbed v0.17.19 release
(`c364feb974b91e14cbf328c6949f3df36e89ce81`), which contains that stack. The native component writer/archive checks have
passed independently. A capability check cannot compensate for a missing native
symbol at link time, so source and bindings must move together.

## Output transaction

Reviewed PDF output is written progressively through a 64 KiB Rust buffer into
a private temporary file in the destination directory. The native renderer emits
completed PDF objects and keeps cross-reference offsets instead of constructing
a second complete PDF byte buffer. Other export formats retain their existing
byte-based path.

The application checks the native byte count against the staged file length,
flushes and synchronizes the file, and publishes it only after rendering succeeds.
A renderer error, write error, exceeded output limit, or renderer panic before
publication removes the staging file and preserves any previous output. An
unconfirmed existing destination is rejected before rendering starts. The final
no-clobber operation also protects a destination created by another writer during
rendering. After publication, directory synchronization can still report an I/O
failure; that does not mean the published file was rolled back.

Mobile PDF output is capped at 256 MiB to match the native document-provider copy
limit. Desktop PDF output is capped at 2 GiB. These are output-file limits, not
peak-memory guarantees: page decoding, compression, reviewed text, region data,
and the runtime still require memory. Native page and text limits also apply.
Physical page dimensions, corrected text, Unicode mappings, and reviewed region
geometry use the same conversion as the byte-based renderer.

## Progress and cancellation

Each export has a UUID, correlated byte-progress events, and an explicit
publication boundary. A stop received before the export's IPC request is retained
in a bounded, expiring pending set. Rendering and writing check the stop between
bounded writes, then again immediately before publication. The worker is awaited
before staging is released. A stop after publication begins is rejected, and the
completed output must still be reported as saved.

The cancellation guard wraps the buffered writer so its `Interrupted` error
reaches the native callback directly. Both the native callback and the byte-output
loop propagate that error instead of retrying it indefinitely. Nonstreaming
renderers are allowed to finish their current render before the stop is applied;
native page decoding/compression also finishes its current work before the next
writer callback. This is cooperative cancellation, not forced thread termination.
Byte progress is throttled to at most ten updates per second during writing.

On mobile the OS save picker owns the external destination and copy. Cancelling
before that step discards the private staged output. The in-application stop is
disabled during publication and the OS destination step; the picker supplies its
own cancellation before the copy. Closing the UI does not delete a file while a
native writer or provider is still using it.

## Qualification

The portable contract crate compiles the actual application storage and atomic
output modules. At source
`7688c8db61020bcc7e87c7cc7a0260dcd6f1bc37`,
[run 42618020194](https://github.com/CrispStrobe/CrispSorter/actions/runs/42618020194)
passed all 16 cases: ten storage/lease cases and six atomic-output cases. The six
output cases cover explicit replacement, partial-write failure, rejection before
rendering, a competing destination writer, publication after completion, and
cleanup after a simulated renderer panic. This result qualifies the portable
file transaction, not the new native PDF writer or mobile document picker.

The full application regression exports a corrected multilingual PDF, reopens
it with an independent parser, checks page count and text, then forces a native
output-limit failure and verifies that the earlier file remains intact. That
test requires the matching native writer and has not yet run at this checkpoint.
Native writer callback, limit, flush, and PDF conformance cases are qualified in
CrispEmbed separately. Bidirectional reader compatibility remains a separate
gate; streaming does not repair a reader's text-order behavior.

## Cancellation contract evidence

Hosted [run 42627812221](https://github.com/CrispStrobe/CrispSorter/actions/runs/42627812221)
at `8ea0a7e75c93bc285b98976590aea2f86e1b51c4` passed all 25 portable native
contracts: ten storage, six atomic-output and nine cancellation cases, with no
failures or ignored tests. The [retained receipt](../tests/results/ocr-studio/storage-cancel-8ea0/receipt.json)
records the exact source hashes, verified 8,361-byte artifact ZIP and raw log.
This exercises the production Rust modules independently of Tauri and the
native renderer. The earlier 22-case receipt remains available. Full application
and native-renderer integration remain separate qualification gates.

Completed request IDs are also retained for five minutes, capped at 64 entries.
A stop arriving while a completed worker response is still in flight returns
false; it cannot create a before-start cancellation for that same ID. Reusing a
retained completed ID is rejected. Each new export uses a fresh UUID. Three
regressions exercise completed publication, finished cancelled/failed workers,
and the bounded expiry rule; all three passed in the 25-test run above.
