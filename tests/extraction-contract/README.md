# Email and HTML extraction contracts

This package compiles the production email and HTML extractor modules directly.
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

Extraction limits are explicit errors, so a partial result cannot replace a
complete indexed document as a successful extraction. An incomplete MIME
delimiter with recoverable text carries an extraction warning tag. Binary
attachments contribute their filenames and a name-only coverage tag; their
contents are not claimed as extracted. Mbox remains a single indexed source
with labeled message sections. HTML extraction does not execute scripts, fetch
resources, or evaluate external CSS; it respects markup-level hidden attributes
and supported inline visibility declarations.
