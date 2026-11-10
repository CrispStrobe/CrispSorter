# Reviewed OCR text export compatibility

The export preflight examines the chosen pages and included regions before a
destination picker opens. With the qualified CrispEmbed renderer
(`9c44a69c`, unchanged in the pinned v0.17.18 release), general bidirectional PDF capability
remains disabled. Selecting searchable PDF or PDF/A for affected text presents
the reason and buttons for the available text formats. Changing format keeps
the page selection, approval policy, page geometry, source provenance, and
reviewed logical text unchanged.

The application uses the same conservative script/control ranges as that native
renderer. This check detects an unavailable export path; it does not implement
Unicode layout. Ordinary Unicode text remains eligible for PDF. Supplementary
characters receive a visible advisory because a retained independent reader
diagnostic shows that copying correct Unicode does not imply that every PDF
reader can find that text with its search function. No universal PDF search or
right-to-left copy claim is made.

A selection-aware density advisory also appears when any included page reaches
50,000 Unicode scalars of reviewed text. Counting stops at that threshold and
does not allocate a character array. The retained dense-page control demonstrates
a limit in one reader for very small text advances; this is not a universal
50,000-character PDF limit. PDF and PDF/A remain available, with direct buttons
for the advertised text alternatives. Excluded regions and unselected pages do
not contribute to the advisory, and counts are per page rather than per export.

## Text alternatives

| Format | Text and line breaks | Availability |
| --- | --- | --- |
| Plain text | UTF-8 region text without trimming or normalization. Regions are separated by LF and pages by form-feed. Original CR, LF, CRLF, spaces, tabs and literal controls inside regions remain unchanged. | All workspace builds. |
| HTML | Escaped region text, automatic paragraph direction and CSS paragraph direction handling, preserved whitespace and a mobile viewport. CR is represented with a character reference so HTML source parsing does not replace it. Unsupported rich-text characters are rejected. | All workspace builds. |
| Word | Logical text in editable paragraphs, explicit first-strong paragraph direction, spaces, tabs, empty paragraphs and explicit source-page boundaries. CR, LF and CRLF each become one paragraph boundary; this is semantic line-break preservation, not byte-identical line-ending storage. | Builds with `ocr-text-export`, included by `desktop`, `ocr-workspace` and `ocr-mobile`. |
| JSON | Exact reviewed region strings and their geometry/provenance in JSON string encoding. | All workspace builds. |

The OCR-only Word writer does not use the generic desktop document exporter.
Its dependencies are the existing `docx-rs` and `unicode-bidi` packages; image
decoding is disabled for `docx-rs`, and the mobile feature does not enable
desktop integrations. Older builds that do not advertise Word never receive a
Word alternative button.

Word input is bounded to 16 MiB of reviewed UTF-8 text and 100,000 structural
items (paragraphs and tabs, with CRLF counted as one line break). The frontend
and native command check this before output begins. These input limits do not
establish a measured mobile peak-memory bound. Larger selections can use plain
text or HTML, or be exported in smaller groups.

Characters excluded by the rich document writer, including NUL, form-feed
inside a region and the XML-disallowed U+FFFE/U+FFFF values, are never silently
removed. Preflight offers plain text or JSON instead. Unpaired UTF-16 surrogates
in frontend state must be corrected before any export because they are not
valid Unicode scalar text.

## Qualification scope

`tests/ocr-text-export-contract` imports the actual application module. The
hosted workflow first checks every resolved package version and checksum
against the application lock. It exercises the module with and without Word,
builds the same portable module for Android and iOS targets, and retains the
actual TXT, HTML and DOCX files. Independent ZIP/XML, `python-docx` and Chromium
checks compare the complete logical text, whitespace, page boundaries, paragraph
direction and mobile HTML layout against explicit fixtures. A custom inverse
XML mapping is not the sole line-break oracle.

The browser workspace tests separately verify the capability reasons, disabled
save buttons, zero destination-picker calls while blocked, exact export request
text/geometry, touch-sized alternative buttons, older capability sets, and
scope changes back to an eligible Latin page.

The retained [5382 source receipt](../tests/results/ocr-studio/text-exports-5382.json)
records the completed Linux portable lane: 5 plain-export contracts and 7
Word-enabled contracts, Android target compilation, independent text/Word
readbacks, and generated HTML in Chromium at widths 390 and 1440. Its resolved
lock is now checked in unchanged; all 30 registry packages match the application
versions and checksums, and the text-only dependency graph has no image decoder
or native-library dependency. The application lock removes only the obsolete
`docx-rs` image edge. Future portable jobs use `--locked` from their first test.

The same immutable source passed 246 frontend tests and 38 browser workspace
scenarios with no browser errors; Svelte reported no errors and 54 existing
warnings. The later dense-page advisory has its own boundary/selection unit
check and extends the existing supplementary-text browser scenario. Those
successor checks are not included in the 5382 result. The iOS portable job was
queued when the initial receipt was saved and subsequently completed as
cancelled, with no execution steps reported. Its target check remains
unqualified; the receipt preserves both observations.

The portable module lane does not replace complete application builds, mobile
document-provider execution, rendering in a Word application, or physical-device
testing. General bidirectional PDF capability remains disabled irrespective of
these text-export results.
