# OCR document workspace

The OCR tab is a document review workspace built around local CrispEmbed
processing. A project combines source documents, immutable working page images,
recognition evidence, corrections, and approval state. It can be resumed without
rerunning completed recognition.

## Work through a document

1. **Add documents.** Select one or more PDFs, TIFFs, or page images. PDFs require
   the PDF rendering build feature and PDFium. JPEG orientation is baked into the
   working pixels. TIFF frames are imported in order; unsupported frames stop the
   import with an error rather than silently disappearing.
2. **Arrange and prepare.** Use the page rail to select pages, filter by status,
   move pages earlier or later, exclude pages, or remove them from the project.
   Rotate, crop, or split a spread using the image toolbar. Select a recognition
   profile and apply scan preparation when the source needs repair.
3. **Recognize.** Run the current page, selected pages, pending pages, all included
   pages, or failed pages. Recognition runs sequentially to bound model memory.
   Pause and Stop take effect after the current native call; completed page
   results are retained. A failed page does not stop the remaining batch.
4. **Review.** Select a source region to see a magnified detail and its text. Edit
   text, move/resize its box, change its region type, and reorder its position in
   the exported text. Follow **Next issue** or press **F8** to move through the
   project. Approve individual regions or the complete page after inspecting it.
5. **Export.** Select a format and page scope. Export preflight accounts for every
   selected included page. Unrecognized or invalid pages block export; page
   approval is required by default and can be relaxed explicitly. Choose the
   output filename. Corrected text is used directly; export never reruns OCR.

### Projects and recovery

Projects save automatically after edits and after processing results. **Save**
flushes the outstanding save immediately; the header reports pending, saving,
saved, or failed state. A save failure remains visible and retryable.

The native workspace stores immutable, content-addressed page PNGs and versioned
JSON manifests in the application's data directory. Original files are not
modified. The workspace stays alive across tab changes, so its queue and save writer stay coordinated. Starting the application restores the latest project. **Projects** opens
another saved project; **New** starts a fresh one after flushing the current
project. Interrupted running pages return to pending when a project is restored.

Writes use a same-directory staging file, filesystem synchronization, atomic
replacement, a previous-revision recovery copy, and an expected-revision check.
A SQLite write lock serializes separate application processes. A stale writer
receives a conflict instead of overwriting newer work. Missing page assets are
reported while text corrections remain recoverable; recognition and export
require their images to be available.

Undo/redo stores bounded project snapshots, with both a count limit and an
approximately 24 MiB memory budget. Consecutive typing in one region is one undo
step. Pixel edits invalidate the old recognition geometry; Undo restores the
previous working image and result together. The original image comparison hides
working-image overlays so transformed coordinates are never drawn on the wrong
image.

### Page and region controls

- Page range selection accepts comma-separated page numbers and hyphenated
  ranges, such as `1-4, 8`.
- Excluded pages stay in the project and are omitted intentionally. They can be
  included again. A pending page is never omitted merely because it lacks OCR.
- Region geometry is expressed in pixels of the current working image. Draw a
  text, table, picture, or formula region; drag to move it; resize its corner; or
  enter exact left/top/width/height values in Region properties.
- Picture regions remain on the source image and are excluded from the text
  layer. Excluding a text region removes its text, not the underlying image.
  This control is not a redaction tool.
- Zone recognition expands a selected rectangle to cover intersecting text
  regions completely, preserves their reading-order position, and retains
  unaffected regions. The effective crop and its page offset are recorded.
  It does not geometrically repair the crop, because its boxes must map back
  to the unchanged page.
- Spread splitting starts at the center and accepts an exact vertical split
  position in pixels. Large projects display the page rail in groups of 100;
  range selection and batch processing still cover the full project.
- The Reading order view shows the text sequence used by export. Move regions
  earlier/later to change that sequence.
- Find/replace works across included pages, handles literal Unicode text, and
  treats replacement text literally. Replace-all is undoable and invalidates
  approvals on changed pages.

## Recognition and scan repair

Built-in profiles cover balanced documents, precision review, degraded scans,
books with columns, and screenshots. Settings are saved in the project and
snapshotted at the start of a batch, so mid-run settings changes cannot alter a
partially processed batch.

The native integration exposes the supported engines reported by the build,
including DBNet + TrOCR, PARSeq, Tesseract LSTM, and the supported vision-language
OCR engines. Explicit engine selection builds a concrete pipeline stage.
Tesseract language selection resolves the corresponding language model; other
engines use the languages supported by their configured models. Missing, incomplete, or
unresolvable model files produce actionable errors.

The preparation controls expose classical deskew/cleanup, NAFNet denoise,
straightening curved pages, Restormer/SCUNet/InstructIR restoration, and
PAN/Real-ESRGAN/SAFMN super-resolution. Layout analysis supports semantic document
regions or model-free connected components, with optional header/footer removal.
A conservative left-to-right block/column heuristic preserves line order within
blocks and handles spanning headings; ambiguous geometry keeps the native order.
Inspect the resulting sequence, especially for complex or right-to-left layouts.
Advanced settings accept registry names or installed model paths and the existing
per-stage pipeline configuration.

Prepared images are materialized before recognition. The recognizer's internal
geometric cleanup is disabled for review runs; returned boxes must match the
prepared image used for the preview and export. Pipeline instances are keyed by
configuration, so changing a profile does not keep an earlier engine cached.
Preparation notes are retained with the page and shown for review. If a repair
returns the same image pixels, corrected text and approval remain intact.

## Confidence and review evidence

Recognition scores are review signals, not calibrated probabilities of correct
text. Detection confidence says that an area looks like text; it does not measure
whether its transcription is correct. The model distinguishes recognition,
detection, unknown, and manual evidence.

Unknown or detector-only confidence remains in the review queue. Low recognition
scores and unapproved corrections also remain in the queue. A page can be
explicitly approved after visual inspection even if its engine cannot provide
scores. Approving a blank page is an explicit acknowledgement that no text is
expected. Subsequent text, geometry, type, exclusion, or reading-order edits
invalidate the corresponding approval.

Per-character highlighting requires explicit Unicode-codepoint alignment and an
exact codepoint count. Some native engines expose token likelihoods in the same
legacy sequence buffer; those are never treated as character offsets just
because the array length happens to match the text.

Physical page sizes are kept separately from raster dimensions. PDF import
retains the source page size; images without resolution metadata use 300 DPI.
Crop, rotation, and splitting update paper dimensions, while upscaling preserves
them. The PDF export panel offers an explicit scan-DPI override.

The optional paired native diagnostics include source and working dimensions,
coordinate validity, engine stages, automatic acceptance decisions, detected
language, reading order, score provenance, and warnings. Automatic pipeline
acceptance is distinct from human page approval.

## Structured extraction

The **Extract** panel works on the current page or selected region:

- **Fields:** GLiNER operates on the reviewed text, so corrections affect the
  resulting values. The layout option runs LiLT against page pixels. Field names,
  minimum extraction score, and values are editable. Field JSON retains the
  original value and flags manual corrections; its score always describes the
  original model extraction. Export the result as JSON.
- **Tables:** native structure recognition returns a table. Only inert cell text
  is placed in the editor; returned HTML is not inserted into the live DOM.
  Cells are editable and export as CSV. Inspect merged cells against the source.
- **Formulas:** native math recognition returns LaTeX. Edit and copy it, or save a
  `.tex` file.

Extracted results are retained with their source page, selected region, and
source evidence. Navigation during extraction cannot attach a result to another
page. Text-based fields become stale when their input text changes; pixel-based
results become stale when the source image or crop geometry changes. Undo/redo
restores the matching editor values. Original scores and text remain available
in structured JSON even after corrections. **Update search index** uses the
approved corrected text for the current source document, feeding CrispSorter's
existing chunking and semantic/full-text indexing pipeline.

## Output formats

| Format | Contents | Availability |
| --- | --- | --- |
| Plain text | Corrected text, with form-feed page boundaries | All builds |
| Markdown | Corrected text in page order | All builds |
| HTML | Escaped corrected text grouped by page | All builds |
| JSON | Structured page/region data and review evidence | All builds |
| DOCX | Editable flowing text in page order | Desktop builds |
| hOCR | Text and pixel geometry | CrispEmbed |
| ALTO | Structured XML text and pixel geometry | CrispEmbed |
| Searchable PDF | Working page image plus invisible corrected text | Paired updated CrispEmbed build |

Searchable PDF export requires the matching updated native renderer and the
`ocr-reviewed-pdf` feature (included in `ocr-workspace`). Older native builds retain the other export
formats. Archival PDF/A export is not advertised: it requires embedded resources
and independent conformance validation. DOCX export produces flowing text, not
an exact reconstruction of arbitrary page typography. Exports preserve empty recognized
pages as image pages when the output supports images.

The reviewed PDF renderer rejects right-to-left text and directional controls
with an actionable error. Its current text mapping cannot preserve their logical
order consistently across PDF readers. Such documents can still be reviewed and
exported as text, JSON, hOCR, or ALTO without publishing a reversed searchable
layer. Full bidirectional PDF support needs a separately validated renderer.

All exports validate dimensions, finite coordinates, nonempty page selection,
and asset availability. Existing output files require explicit replacement.
Original source files and workspace assets remain protected even when replacing
an existing export. Writes are staged atomically; a failed render does not
truncate an existing file.

## Keyboard shortcuts

| Action | Shortcut |
| --- | --- |
| Save immediately | Ctrl/Command + S |
| Find and replace | Ctrl/Command + F |
| Undo workspace change | Ctrl/Command + Z |
| Redo workspace change | Ctrl/Command + Shift + Z |
| Next review issue | F8 |
| Previous/next page | Alt + Left/Right |
| Exit drawing mode / close dialog | Escape |

Native editing shortcuts continue to work inside text inputs. Toolbar Undo is
available for project-level changes while a text field has focus.

## Development and verification

The main implementation is split between:

- `src/lib/ocr/model.ts`: versioned state, geometry, approvals, search, history,
  and export preflight;
- `src/lib/ocr/queue.ts`: sequential native work with cooperative pause/stop;
- `src/lib/ocr/autosave.ts`: serialized, revision-aware save coordinator;
- `src/lib/ocr/client.ts`: typed Tauri transport and test adapter;
- `src/lib/components/OcrWorkbench.svelte` and `ocr/PageCanvas.svelte`: workspace
  and source-image editing;
- `src-tauri/src/ocr_workspace.rs`: durable assets/projects, import, transforms,
  processing, extraction, and atomic export;
- `src-tauri/src/extractors/ocr_crispembed.rs`: configuration-keyed native
  pipelines and review provenance.

Run `npm run check`, `npm test`, and `node tests/ocr-workspace/smoke.mjs`. The
browser harness exercises the real component with a deterministic adapter and
synthetic document fixture; it does not claim model accuracy. See
`tests/ocr-workspace/README.md` for installation and screenshot output.

The standard CI/release recipes keep their existing CrispEmbed pin. To enable
new native review diagnostics, use the matching CrispEmbed feature-branch source
and native library together, then build CrispSorter with
`--features ocr-workspace` (plus `desktop` for the
desktop-only features). The paired CI workflow pins an immutable native source
revision, builds its library, and checks the Rust consumer against that exact
revision. Never mix a new Rust binding with an older prebuilt native library.

### Release qualification

Automated model-free tests cover recovery/revision conflicts, page completeness,
geometry validation, confidence semantics, review invalidation, literal
find/replace, undo bounds, queue serialization, save retries, and browser review
flows. Native tests cover strict TIFF handling, source protection, atomic export,
manifest recovery, and native diagnostic serialization.

Before treating a release as production-qualified, run representative scanned
PDF/TIFF/image corpora through the actual deployed model and GPU combinations;
measure character/word error rates; validate multilingual PDF search/copy,
reading order, tables, and formulas; independently validate archival output; and
exercise large documents, low disk space, process interruption, and recovery on
each supported desktop platform. Scanner drivers, exact typography reconstruction,
and automatic layout equivalence are outside the implemented workspace contract.
