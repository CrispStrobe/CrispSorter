# ZIP, mail, presentation, HTML and text extraction contracts

This package compiles the production ZIP, email, PPTX, HTML and text extractor modules directly.
The document envelope has inert placeholders for unrelated audio/image fields;
the parsers, file reads, text decoding, limits, and test cases are the actual
application implementations. It needs Rust 1.98 or newer and no desktop shell,
GPU, model downloads, or external extraction programs.

Run from the repository root:

```sh
cargo test --locked --manifest-path tests/extraction-contract/Cargo.toml
```

Its resolved registry dependency versions and checksums match the application's
lockfile. The tests cover encoded and folded headers, transfer encodings,
character sets, multipart boundaries, alternative selection, forwarded mail,
text attachments, explicit binary-attachment coverage, malformed encoding,
depth limits, mbox message boundaries and failure attribution, HTML entities,
hidden content, inline word boundaries, saved-page provenance, and bounded reads.

ZIP contracts invoke the real text/HTML/mail parsers through the production
archive dispatcher callback. They cover nested member provenance, generated
temporary filenames and cleanup, ZIP64 directories, compressed input and
central-directory allocation guards, shared nesting/member/expanded-byte/text
limits, document-package expansion preflight, checksums, duplicate names,
encrypted members, unsafe paths, symbolic links, and failed native parsers.
The suite includes the actual presentation reader and its reading-order,
speaker-note, comment, and text-budget tests. It does not load PDF, DOCX, or OCR
engines; full application CI qualifies their dispatcher and option wiring.

ZIP sources remain one indexed document with a labeled section per readable
member. Unsupported, encrypted, linked, unsafe, and textless members have
visible omission notes and extraction coverage tags. An archive with no
searchable member text fails. A limit, checksum, malformed archive, or supported
member parser failure fails the whole refresh, preserving the existing index
through the normal failed-refresh contract. Standard ZIP and bounded ZIP64 are
supported; split/multi-volume archives and other archive formats are not.

Default archive limits are 128 MiB compressed source, 8 MiB central directory,
32 MiB per expanded member, 128 MiB total expanded bytes, 16 MiB extracted text,
2,048 total entries, and four ZIP levels including the source. Nested archives
and ZIP-based document packages consume the same entry/expansion budgets.
Member names never become filesystem extraction paths. Source bytes are copied
to a private bounded snapshot before directory parsing, and temporary files are
removed after their native parser returns. Cancellation of a foreground wait
does not claim cancellation of an already running native parser.

Extraction limits are explicit errors, so a partial result cannot replace a
complete indexed document as a successful extraction. An incomplete MIME
delimiter with recoverable text carries an extraction warning tag. Binary
attachments contribute their filenames and a name-only coverage tag; their
contents are not claimed as extracted. Mbox remains a single indexed source
with labeled message sections. HTML extraction does not execute scripts, fetch
resources, or evaluate external CSS; it respects markup-level hidden attributes
and supported inline visibility declarations.

Direct foreground EML, MBOX, and PPTX ingestion uses a private, generated source
copy before invoking these same readers. Input ceilings are 64 MiB for EML,
256 MiB for MBOX, and 128 MiB for PPTX; extracted text is limited to 16 MiB.
PPTX also passes the ZIP directory/member expansion checks before parsing, and
its reader bounds repeated relationship reads, slide references, XML nesting,
XML event counts, and assembled text. Malformed XML and document type declarations
fail instead of publishing a partial parse. Slide text, speaker notes, and
comments are supported; images, charts, and embedded media are explicitly omitted.
Image-only decks fail clearly rather than indexing generated slide headings as
source text. The bridge does not invoke OCR or expand binary mail attachments.
Frontend tests qualify host-path-only dispatch, cancellation result disposal,
and extraction coverage tags. Index ingestion retains its source fingerprint
checks before extraction, after extraction, and at the native write boundary.
