# OCR document workspace

The OCR tab is a document review workspace built around local CrispEmbed
processing. A project combines source documents, immutable working page images,
recognition evidence, corrections, and approval state. It can be resumed without
rerunning completed recognition.

## Work through a document

1. **Add documents.** Select one or more PDFs, TIFFs, or page images. Desktop PDFs require
   the PDF rendering build feature and PDFium. Mobile PDFs use the registered
   Android or iOS document reader. JPEG orientation is baked into the
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

A recognition batch also saves its exact selected page order, working-image and
review revisions, profile, engine, language, and configuration before the first
native invocation. **Resume saved batch** continues only its unfinished entries;
other pending pages and later toolbar settings do not change that selection.
**Retry saved failures** includes failed entries when continuing the saved batch.
Completed entries are retained. A source, inclusion, or reviewed-text change blocks
continuation with an explanation, so a new explicit selection is required for
changed pages.

Page-start, result, and error checkpoints finish saving before the next native
call. If a checkpoint cannot be saved, processing stops and the current result
stays in memory. Resolve the reported problem, use **Save**, and resume the saved
batch. A successful OCR result is not converted into a recognition failure merely
because its save failed. Pause and Stop also take effect while a start checkpoint
is saving, before entering native inference. The workspace stays locked through
the final batch save; interruption recovery does not depend on the in-memory queue.

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

## Phone capture and photo preparation

On a phone, **Pages**, **Page image**, and **Text & tools** switch between the
page list, source image, and full review/settings/extraction/export controls.
The page list supports the same selection, filtering, ordering, exclusion, and
removal actions as desktop. Choosing a page opens its image; selecting a text
region or following a review issue opens the text tools. Desktop retains the
side-by-side workspace.

**Capture photo** uses the camera/photo selection supplied by the system webview.
Available choices depend on the platform. It accepts JPEG, PNG, WebP, and BMP
images up to 32 MiB. HEIC, HEIF, and AVIF are offered only when the device reports
the matching native decoder; unsupported formats require a JPEG/PNG copy. The
native import archives original bytes, applies image orientation, and creates
an immutable working page before returning. An imported photo opens **Prepare
photo** where the paired photo tools are available; recognition is a separate
explicit action.

On mobile, PDFs render one page at a time at 200 DPI, with a 16 megapixel raster
cap. Their physical page dimensions are retained when a large page must be
reduced. A document must fit the 32 MiB native input limit, 10,000 page limit,
and 512 MiB rendered-output limit; the document-provider bridge currently has
a stricter 16 MiB transfer limit. Oversized, damaged, and password-protected
PDFs produce an actionable error. No partial document is added on failure.
**Cancel import** stops after the current native step; earlier completed
documents in the selection remain saved. The original provider URI and private
source archive are stored separately, and repeated selections of the same
archived original are detected even when their provider identifiers differ.
See [mobile document import](ocr-mobile-import.md) for the native contracts,
limits, fixtures, and qualification scope.

The capture input requests `image/*` with `capture="environment"`, which is
required by the locked Android webview's delegated camera path. Android declares
the `IMAGE_CAPTURE` intent query for package visibility; no direct camera access
permission is required for that external camera intent. The iOS configuration
includes camera and photo-library purpose strings. These integration contracts
are checked separately from simulated file selection; actual device capture is
still part of release qualification below.

A useful preparation sequence is:

1. **Check photo quality.** Inspect the returned hints about resolution, lighting,
   contrast, and focus. A measured tilt populates the correction field, but
   neither image measurements nor angle estimates are calibrated probabilities
   of OCR accuracy. An absent estimate can mean an aligned page or insufficient
   evidence; inspect text baselines before deciding to rotate.
2. **Correct perspective.** Place the four handles on the paper corners, in
   clockwise order from top left. Drag with a mouse or touch; arrow keys move a
   focused handle by one image pixel and Shift moves it by ten. Exact coordinates
   are also editable. Crossed, concave, collapsed, or out-of-image selections
   cannot be applied. This is manual corner selection with a true projective
   warp, not automatic border detection.
3. **Apply tilt correction.** Review the suggested angle or enter a value between
   −20° and +20°. Positive values rotate clockwise. Native processing uses bounded
   analysis while sampling the full-resolution source for the output.
4. **Flatten shadows** when lighting remains uneven. This action uses the existing
   background-whitening primitive without additional deskew, border cropping,
   binarization, or learned restoration. Inspect the result: rotation alone can
   make shadowed text harder to recognize when new white margins meet a dark
   photographed background.
5. Compare **Original** with the working page, use **Undo** for any unwanted
   change, then recognize and review. Changed pixels invalidate old recognition
   geometry and approval. Byte-identical operations preserve reviewed text and
   approval. Original comparison hides working-image overlays and exits corner
   editing.

Photo assessment and correction keep their captured source page even if the
user navigates while a native call is running. Assessment results are discarded
when that page is no longer selected; pixel operations remain attached to their
captured page. Navigation to a different source resets corner and angle controls.

On mobile, **Add documents** uses the native document-provider picker. The
provider bridge currently limits each document to 16 MiB; larger documents must
be reduced or imported as smaller page images. Export renders into private
staging, opens the native save picker, and reports success only after the
provider copy completes. The system picker owns external destination selection
and any replacement confirmation; a user-confirmed replacement may overwrite
that selected external file. The imported original bytes and immutable workspace
images remain preserved inside the application. Cancelling the picker reports
cancellation. Temporary export staging is removed after completion, cancellation,
or failure.

The **Prepare photo** controls are advertised only with `ocr-photo-tools`,
included in `ocr-workspace`, and require the matching CrispEmbed source and native
library. Camera byte import and ordinary image review remain available without
that feature. The app calls explicit native photo APIs; it does not change
process-wide experimental geometry flags for unrelated OCR operations.

The paired geometry tests use generated text pages with rotation, perspective,
lighting gradients, noise, and JPEG compression, then measure text through an
independent reader. They support the declared synthetic cases and retain the
negative deskew-only shadow results. They do not qualify physical phone cameras,
all native recognition models, or every document type. Physical-device capture,
provider round trips, and representative real-camera documents remain release
qualification work. See the paired CrispEmbed
[phone-photo evidence and repeatable checks](https://github.com/CrispStrobe/CrispEmbed/blob/feat/ocr-photo-preprocessing/docs/ocr-phone-photo.md).

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
retains the source page size. Raster import reads per-frame TIFF resolution,
PNG pixel density, and JPEG EXIF/JFIF density; orientation swaps the paper axes
together with the image. Images without valid resolution metadata use 300 DPI.
Crop, rotation, and splitting update paper dimensions, while upscaling preserves
them. The PDF export panel offers an explicit scan-DPI override.

Optional PNG/JPEG metadata reads are bounded to the first 1 MiB. Density must
use physical units, be finite, fall within 10–9,600 DPI, and yield dimensions
within 0.01–14,400 points. Missing, unitless, invalid, or unusually late metadata
uses the 300 DPI fallback; an omitted TIFF resolution-unit tag uses the format's
inch default. JPEG EXIF density takes precedence over conflicting JFIF density.

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
  Cells are editable and export as CSV. Formula-like entries are prefixed as
  literal text for spreadsheet readers; ordinary numbers remain numeric and the
  original cell values remain unchanged in the project and structured JSON.
  Inspect merged cells against the source.
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
For direct filesystem exports, original source paths and workspace assets remain
protected even when replacing an existing export. Mobile external destination
replacement is controlled by the system save picker, as described above. Writes are staged atomically; a failed render does not
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

The [9 November checkpoint](ocr-studio-checkpoint-2026-11-09.md) records exact
tested revisions, retained phone-photo results, corrected CI evidence, and the
remaining native application and physical-device gates.

The main implementation is split between:

- `src/lib/ocr/model.ts`: versioned state, geometry, approvals, search, history,
  and export preflight;
- `src/lib/ocr/queue.ts`: sequential native work with cooperative pause/stop and awaited durable checkpoints;
- `src/lib/ocr/batch.ts`: persisted selection, frozen settings, source checks, and interruption recovery;
- `src/lib/ocr/photo.ts`: capture validation and four-corner geometry;
- `src/lib/ocr/autosave.ts`: serialized, revision-aware save coordinator;
- `src/lib/ocr/client.ts`: typed Tauri transport and test adapter;
- `src/lib/components/OcrWorkbench.svelte` and `ocr/PageCanvas.svelte`: workspace
  and source-image editing;
- `src/lib/components/ocr/PhotoPreparation.svelte`: capture guidance, measured/manual
  tilt, perspective selection, and background whitening;
- `src-tauri/src/ocr_workspace.rs`: durable assets/projects, import, transforms,
  processing, extraction, and atomic export;
- `src-tauri/src/ocr_photo.rs`: bounded capture measurements and paired native photo geometry;
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

## Mobile OCR model installation

The paired `ocr-mobile` Cargo profile enables image and TIFF OCR, photo correction,
review diagnostics, and searchable PDF export. It deliberately omits `pdf-render`:
PDF import needs a separately packaged mobile rasterizer, and this profile reports
PDF import as unavailable. Release defaults and release runtime pins are unchanged.

The mobile profile provisions models in the application's private `ocr-models`
store. Downloads are explicit user actions; recognition only loads an installed,
verified model or a user-supplied local GGUF path. The workbench's basic installer
uses `dbnet-det` and `tesseract-<language>` (English by default). Other registered
engines use the existing Settings model browser and the same installer.

The installer reads exact-name download metadata from the paired CrispEmbed
registry. Every primary and declared companion file needs a real SHA-256 pin;
unpinned entries fail with an actionable error. Rust streams HTTPS responses with
a 64 KiB buffer, a 2 GiB limit per file, bounded redirects, and a 30-minute request
timeout. It checks length, digest, and the primary GGUF header before publication.
It respects the existing offline flags and model licence-consent checks. It never
uses a shell downloader or changes the process environment on mobile.

Android model downloads use the pinned public TLS root bundle and normal Rustls
certificate/hostname verification; this client does not consult user/private
Android trust anchors. The independent artifact SHA-256 check is still required
before a model becomes ready.

Each model has its own directory, so companion filenames cannot collide across
models. A complete verified version is published through an atomic manifest;
interrupted downloads and failed companion downloads leave the previous version
intact. Readiness validates every declared artifact, with verification cached only
while the immutable file's size and modification time match. Explicit local GGUF
paths retain the existing behavior and are outside the managed download limit.

These provisioning tests use tiny synthetic streams, including corruption,
interruption, size/length failures, incomplete companion sets, and symlink escapes.
They do not download production weights. A source/link check alone does not prove
mobile inference, and simulator inference does not qualify physical camera capture,
memory pressure, thermal behavior, or recognition accuracy on real phone photos.
