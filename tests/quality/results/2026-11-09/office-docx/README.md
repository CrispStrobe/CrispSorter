# OCR-06a Word export evidence

This directory retains the actual production-generated DOCX fixtures and their
independent inspection. The producing checkout was based on `b10cf4e` with the
OCR-06a changes present. Exact source and dependency hashes are in
[receipt.json](receipt.json); no release binary or installed application was used.

## Executed checks

- **22 native tests passed:** two document-structure cases, nine extraction
  validation cases and 11 Word writer cases.
- **Dependency identity passed:** every registry package in the isolated harness
  matches the production lock by name, version, source and checksum. This is not
  a claim that the harness uses the application's complete feature graph.
- **Three DOCX files generated and independently checked:** ZIP integrity,
  package relationships, actual styles/list definitions, literal text, UTF-16
  formatting ranges, sections, merged tables, PNGs and explicit selected scope.
- **Three documents reopened; six pages inspected:** LibreOfficeDev
  `26.8.0.0.alpha0`, build `2c87e51eeaa2b413ff4ae097b2705eea1995d8e5`.
  [reader-receipt.json](reader-receipt.json) records the tool identity, page
  observations and artifact hashes. Original DOCX files were read directly;
  they were not resaved through another document writer before inspection.

| Fixture | Pages | Declared checks |
| --- | --- | --- |
| [semantic-text.docx](semantic-text.docx) | 2 | Heading, Unicode marks, list starting at 7, nested bullet, columns, note/body fallbacks, explicit break |
| [selected-table-figure-formula.docx](selected-table-figure-formula.docx) | 1 | Corrected literal table cells, two-dimensional span, actual PNG, caption, literal LaTeX, omitted unselected values |
| [whitespace-blank-pages.docx](whitespace-blank-pages.docx) | 3 | Exact passage joining, whitespace, tab, CRLF projection, retained blank middle page |

`rendered/` contains the PDFs and all page PNGs. `inputs.json` and `fixtures.json`
retain the exact synthetic inputs and expectations. The independent XML report,
raw native logs, generated harness and pruned lock are retained alongside them.

The first rendering found a real bug: the pinned Word library emitted numbering
ID 1 unconditionally, and our initial definitions reused that ID. The requested
start 7 displayed as 1 and a nested bullet displayed as a decimal item. The
writer now reserves the library's IDs. Native and independent XML checks resolve
each actual paragraph's referenced definition and reject duplicates.
`initial-numbering-failure/` preserves the observed failure separately from the
passing rerun. No exact source-hash receipt was captured for that initial attempt.

Run the production harness with an installed Rust toolchain:

```sh
python tools/quality/check_ocr_office.py --output target/ocr-office-qualification
```

Use a new or empty output directory; do not overwrite prior qualification
receipts. The retained `render_review.py` records the environment-specific
render wrapper, including an allowed temporary directory and unchanged `HOME`.
Its referenced renderer's hash is recorded in the reader receipt.

## Remaining boundaries

Complete Tauri adapter execution and the 38-flow browser suite are separate CI
gates. Microsoft Word, an independent OOXML schema validator, installed desktop
behavior, the real OCR corpus, devices, PDF/A and PDF/UA were not qualified by
this run. The development LibreOffice reader and synthetic set do not establish
general Office compatibility. Exact XML whitespace checks are separate from
visual layout and PDF text extraction.
