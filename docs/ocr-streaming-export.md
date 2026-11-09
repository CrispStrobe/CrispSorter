# Streamed reviewed PDF export

This implementation is saved on `feat/ocr-streaming-export`. It requires the
matching CrispEmbed writer API and remains separate from the primary OCR feature
branch until the exact paired native build passes. The runtime lock must move
together with the native source; a capability check cannot compensate for a
missing native symbol at link time.

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
