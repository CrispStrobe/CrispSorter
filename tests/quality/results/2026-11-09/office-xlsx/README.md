# OCR-06b: literal table workbook export

These artifacts record the production XLSX writer and saved-table boundary on
9 November 2026. The producing checkout was based on `d8c218e` with the source
hashes in [receipt.json](receipt.json). This is a synthetic export qualification,
separate from recognition accuracy, an installed CrispSorter application, or
Microsoft Excel qualification.

## Executed checks

| Check | Observed result |
| --- | --- |
| Native production modules | **39 passed**: document 2, Office boundary 13, DOCX 11, XLSX 13 |
| Registry dependency comparison | **112 exact name/version/source/checksum tuples matched** the production lock |
| Actual package generation | Three DOCX and two XLSX packages; five named worksheets |
| Independent ZIP/XML inspection | Both DOCX and XLSX checks passed, including source order, literal text, relationships, merged anchors, styles, and no formula nodes |
| openpyxl 3.1.5 | **96 literal cells matched exactly**, with **7 explicit Xstring limitations**: six cell values and one sheet name |
| Frontend suite | **449 tests in 47 files passed** |
| Svelte check | **0 errors, 54 existing warnings** |
| Bundled Artifact Tool import/render | Five worksheets imported; all six rendered views inspected; known reader differences retained below |
| LibreOffice Calc | Both original XLSX packages opened; three representative PDF pages visually inspected; literal values and merged layout retained in those views |

The native harness imports the actual production modules. It does not replace
them with a second writer. Its sources, compiler versions, commands, dependency
lock, logs, fixture inputs, generated packages and independent reports are
retained here. The included DOCX fixtures are regression outputs; the separate
[OCR-06a evidence](../office-docx/README.md) records their earlier six-page visual
review. The new native workspace-adapter tests and 39-flow browser suite still
require their full application/CI execution.

## Fixture coverage

[reviewed-workbook.xlsx](reviewed-workbook.xlsx) has three explicitly ordered
worksheets: Unicode-named literal values, a table with a two-row/two-column merged
anchor, and a 64-column table. It includes leading zeros, date-looking and
scientific-looking identifiers, `=`, `+`, `-` and `@` prefixes, whitespace,
Unicode, multiline text, explicit empty strings, and header styles. The
openpyxl reader matched all **86** anchor values in this workbook.

[escaped-and-blank-workbook.xlsx](escaped-and-blank-workbook.xlsx) contains real
carriage returns, CRLF, literal and adjacent `_xHHHH_` sequences, mixed escaped
text, LF/tab-only text, an explicit empty cell, and a selected zero-cell table.
The empty table remains a named worksheet. The native and independent XML
decoders match every literal value after one standards-aware Xstring decode.
openpyxl matches **10** cells and leaves six cell strings and the escaped sheet
name encoded. These seven observations are **not** counted as exact reader
round trips. See [xlsx-openpyxl-report.json](xlsx-openpyxl-report.json).

## Independent reader and visual observations

The bundled Artifact Tool imported the original files without rewriting them.
Its value API matched **93 of 102** explicit anchors: three empty strings became
null, and the same six Xstring cells remained encoded. One sheet name also
remained encoded. Its formula matrices were empty. Its rendered views showed
additional presentation differences: `0012` became `12`, `03` became `3`,
`3.14e5` became `314000`, and an LF/tab-only cell displayed `0`. The native file
and the reader's value API retain these ordinary strings; the renderer does not
provide an exact display pass. The read-only script, raw report and all six
images are under [artifact-tool/](artifact-tool/).

To resolve that display risk, **LibreOfficeDev 26.8.0.0.alpha0**, build
`2c87e51eeaa2b413ff4ae097b2705eea1995d8e5`, opened the **original** XLSX packages
and printed derived PDFs. The first two pages of the ordinary workbook and the
escape workbook's page were visually inspected. Leading zeros,
formula-looking strings, `3.14e5`, Unicode, literal escape-shaped sequences,
merged anchors, bold headers and multiline values display correctly there,
without clipping in these fixtures. A real CR has no visible break in Calc's
PDF, while CRLF is visibly split; this observation does not establish an exact
Calc value round trip.

Default printing spreads the 64-column worksheet across 16 pages. Extracted PDF
text includes every identifier from `ID0001` through `ID0064`; those 16 pages
were not individually visually inspected. The empty worksheet is omitted by
default PDF printing and remains present in the actual workbook. Print setup is
outside this writer's current scope. The conversion command, exact reader
version, complete PDFs, extracted text and three inspected images are under
[libreoffice/](libreoffice/). The original XLSX hashes still match the native
receipt after both readers.

[reader-observations.json](reader-observations.json) records these separate
results. It supplements the immutable harness receipt, whose Office-reader gate
correctly says `not_run` at the earlier time that harness finished. No native
encoding was weakened to accommodate incomplete reader decoding.

## Limits and remaining gates

The workbook path exports only explicitly selected saved tables. It uses stable
page/object identities, literal string cells and merge rectangles; it neither
reads page pixels nor infers numbers, dates or formulas. Page approval applies
only to selected table owners. Current recognition status and unavailable
current images do not block historical saved values. Native collection checks,
size bounds, original-file protection and atomic output remain active.

The tests exercise naming, UTF-16 cell limits, line-feed limits, source scope,
invalid/missing/excluded objects, explicit blank tables, text and grid budgets,
Xstring encoding and bounded ZIP output. The fixture source and original model
sentinels cannot appear in unselected or current-value exports.

Microsoft Excel, OOXML schema validation, complete native application adapters,
the expanded browser suite, accessibility technology, arbitrary large
workbooks, document corpus accuracy and device capture remain separate gates.
No whole-application or release qualification is implied by these results.
