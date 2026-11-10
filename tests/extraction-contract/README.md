# Native extraction contracts

This standalone crate compiles the application's actual bounded container,
mail, presentation, HTML and text modules without model or UI dependencies.
Run from the repository root:

```sh
cargo test --locked --manifest-path tests/extraction-contract/Cargo.toml
cargo test --locked --manifest-path tests/sevenz-contract/Cargo.toml --lib
```

The first command also qualifies the native foreground DTO and the managed
member cache. The second compiles the exact vendored 7z reader with the same
decoder feature profile, including its upstream integrity tests and the added
allocation-limit regressions. Registry versions and checksums in both harness
lockfiles match versions present in the application's lockfile.

## Documents and member identities

The shared container dispatcher supports ZIP, TAR, GZIP, TGZ, 7z, EML and MBOX.
It returns the source SHA-256 receipt, explicit extraction coverage, the
parent's own text, an aggregate preview, and flattened member documents. The
indexing layer stores the parent and each member independently. An email's
body and headers belong to the email; attachments have separate documents.
Mailbox messages likewise retain separate identities. A nested container has
its own summary document and its descendants retain the complete entry chain.

Each entry records its format kind, validated logical name, physical position,
same-name occurrence ordinal, exact decoded SHA-256 and byte count. Stable
member IDs use the root owner, root occurrence and kind/name/occurrence chain;
reordering unrelated entries or changing a member's bytes does not change its
ID. The physical positions and receipts are used to reopen the captured
source generation. The opener snapshots and verifies the complete root, then
re-decodes and checks every selected step. A changed source or receipt fails
with a refresh instruction instead of opening a different document.

Member names are provenance, never filesystem output paths. The parsers stage
generated temporary files; the native opener publishes a verified copy under
a generated name in a bounded managed cache. The cache writer verifies bytes
again, preserves unrelated files, uses unique publication paths and removes
abandoned preparations. User-facing opening commands look up a persisted
owner/document locator before invoking these pure helpers.

## Extraction and coverage

ZIP and ZIP64 directories are checked before entry-table allocation. Ambiguous
duplicate ZIP directory records, split volumes, checksum errors and inconsistent
lengths fail. TAR GNU long-name and PAX metadata records are preflighted under
a bounded metadata size before the reader allocates them. Regular TAR entries
can share names and remain distinct through occurrence ordinals. Links and
special files are omitted visibly; sparse TAR encodings are refused explicitly.
GZIP consumes all concatenated streams and verifies their checksums. Its
combined logical payload is one entry; TGZ and `.tar.gz` then traverse a TAR
entry step.

The 7z reader supports COPY, LZMA, LZMA2 and the compiled standard filters,
including solid blocks and compressed headers. The narrow vendored patch and
its original package checksum are documented in
`../../patches/sevenz-rust2/CRISPSORTER_PATCH.md`. Default optional codecs and
password decryption are disabled. Unsupported or encrypted blocks have visible
omission coverage. A skipped entry in a decoded solid block still consumes the
actual expansion budget. Decoders are dropped before nested containers are
opened, so dictionary allocations do not accumulate along a recursive chain.

MIME decoding handles folded/encoded headers, transfer encoding, character
sets, multipart alternatives, HTML bodies, nested messages and binary
attachments. Supported binary document attachments use the ordinary native
leaf dispatcher, including its caller-selected OCR/conversion options. Text
attachments retain MIME charset-aware text while their source receipt hashes
the exact transfer-decoded bytes. Malformed transfer encoding fails; recoverable
MIME delimiter problems retain an explicit extraction-warning tag. Unsupported
binary formats have omission coverage rather than an invented text result.

Coverage records distinguish unsupported, encrypted, unsafe-name, linked and
textless entries. It includes bounded detailed omissions plus the count of
remaining details. Coverage-only parents remain visible even when no member
has searchable text. Member bodies are never substituted into parent text or
translation fields. Limits and supported-parser failures fail the entire
expansion so the indexing layer can preserve the preceding group on a failed
refresh.

## Budgets and tests

Default limits are 128 MiB archive input, 256 MiB mailbox input, 8 MiB ZIP/7z
header bytes, 32 MiB per decoded member, 128 MiB aggregate expanded bytes,
16 MiB extracted text, 2,048 total inspected entries and four entry-chain
steps. EML parsing additionally retains its 64 MiB message ceiling. Nested
archives, MIME attachment decoding, mailbox messages and ZIP-based document
packages consume the same count/expansion budget. 7z metadata counts and the
products of block/coder/name/property counts are bounded before allocations;
LZMA/LZMA2 dictionary estimates are summed under 64 MiB before constructing a
block decoder. These are component budgets, not a bound on total process RSS.

The container tests cover independent documents and exact reopening, stable
identity across TAR reorder and duplicate names, ZIP and MIME provenance,
concatenated GZIP/TAR, COPY and solid LZMA2 7z with raw and compressed headers,
base64 MIME → ZIP → actual PPTX reading order/notes/comments, nested messages,
legacy MIME character sets, independent MBOX messages, malformed transfer
encoding, CRC failures and compound depth/count/byte/text limits. Existing
ZIP preflight and parser tests remain as lower-level regressions. The cache
and DTO tests verify publication, cleanup and lossless receipt/metadata
transport.

The suite invokes the actual PPTX parser and its XML, relationship, repetition
and text-budget guards. It does not load PDF, DOCX or OCR engines or compile
the entire Tauri adapter. Application CI separately qualifies those feature
and UI boundaries. Presentation text, notes and comments are supported;
images, charts and embedded presentation media retain explicit omission tags.
Foreground cancellation disposes of a stale result and does not claim that an
already-running synchronous native parser was cancelled.
