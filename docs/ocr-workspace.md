# OCR document workspace

The [OCR implementation roadmap](ocr-roadmap.md) scopes the remaining work,
dependencies, acceptance checks, and evidence required for release. Features
listed there as planned or awaiting verification are not completed capabilities.

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
   words in **Regions**, or open **Text editor** to revise complete sentences and
   paragraphs. Re-OCR a region or drawn area and compare the proposed text before
   applying it. Move/resize boxes, change region types, and reorder their position
   in exported text. Follow **Next issue** or press **F8** to move through the
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
- **Re-OCR region** recognizes a selected source region again; **Re-OCR area**
  lets you draw a rectangle. The area expands to cover intersecting text regions
  completely. A comparison preview lets you keep the current text or apply the
  proposed replacement; unaffected regions and their reading order are retained.
  The effective crop and its page offset are recorded. Partial OCR does not
  geometrically repair the crop, because its boxes must map back to the unchanged
  page.
- Spread splitting starts at the center and accepts an exact vertical split
  position in pixels. Large projects display the page rail in groups of 100;
  range selection and batch processing still cover the full project.
- **Text editor** shows editable passages in the sequence used by export. Move
  regions earlier/later in **Regions** to change that sequence.
- Find/replace works across included pages, handles literal Unicode text, and
  treats replacement text literally. Replace-all is undoable and invalidates
  approvals on changed pages.

### Edit complete sentences and paragraphs

Choose **Text editor** above the review list, or select a region and click **Edit
passage** to focus its passage directly. Each text area accepts whole sentences,
pasted replacements, and multiple paragraphs with line breaks. Focus a passage
to inspect its magnified source region alongside the text. **Copy page text**
copies all included passages in their export order.

Passages remain linked to their source regions. Editing their text preserves the
page image, source geometry, original OCR text, and original confidence evidence.
The corrected text is autosaved and exported with its line breaks, without
needing another OCR run. Undo/redo works through the toolbar, and edits revoke
the affected approval. Use **Regions** for confidence-colored words, geometry,
region types, or reading-order changes.

### Edit the full page as a saved draft

Choose **Edit page text** to work across the current page's passages in one text
area. Use ordinary keyboard selection, typing, cut, copy, and plain-text paste.
Ctrl/Cmd+Z undoes typing inside the draft. Tables and formulas remain visible as
protected text; pictures and excluded passages still form editing boundaries
even when their text is absent from the draft.

Draft changes save with the project, separately from the recognized page. The
dialog reports **Draft saved · not applied**, saving progress, or a save error.
**Save draft** retries or flushes its saved state. **Close and keep draft** leaves
it available through **Resume page text draft**, including after reopening the
project or navigating to another page. There is one active page draft per
project. **Discard draft** asks before removing changed draft text; it does not
apply anything to the recognized page.

Choose **Review changes** to compare the current and proposed page text. Expand
**Affected passages and source coverage** to inspect exactly which passages and
image areas the change uses. A change contained in one passage keeps that
passage's identity and original evidence. A change across adjacent text passages
combines the full affected span into **one manual paragraph**, preserves its
original source components, and resets active formatting and structural roles
as described in the preview. Review any affected caption/note links and section
placement. Check **Use combined source coverage** before applying a combined
passage. The chosen area does not create native word boxes or OCR confidence.

Several separate edits are reviewed as the span between the first and last
change, so unchanged passages between them can also be combined. Apply separate
edits if you want to keep those passage boundaries. Newlines stay inside the
resulting passage; they do not create new OCR regions. An edit that crosses a
table, formula, picture, or excluded passage is rejected. Combined coverage that
overlaps another included region is also rejected until the source areas or
scope are corrected.

**Back to editing** changes no recognized text. **Apply page changes** creates
one project history step and removes the applied draft. Project Undo restores
the previous OCR text, structure, and saved draft. Unchanged text outside the
reviewed replacement preserves its original line endings; line endings inside
the changed span use LF. A no-op draft leaves the page unchanged.

The draft retains its captured source. If that page or document changes before
Apply, review is blocked and the draft remains available to copy or recover.
Start a new draft from the current page after recovering any text you need.
The full-page draft supports plain text across passages; formatting selections
remain in the per-passage Structure editor.

### Assign document structure and formatting

Open **Structure** in Review and choose **Enable document structure** for the
current page. This starts a versioned set of logical blocks linked to the
existing source regions. Text, image areas, and original OCR evidence stay with
their source regions; the structure records how the passages should be read.
Older projects can continue using Regions or Text editor without enabling it.

Choose a **Structure passage** to assign its role: paragraph, heading, list
item, caption, running header/footer, or footnote. Picture, table, and formula
regions retain their corresponding specialized roles. Headings have levels;
lists have a type, nesting level, and optional starting number. **Text alignment**
and **Break before** record alignment and explicit page-break intent.

Select text in **Structure passage text**, then use **Bold**, **Italic**,
**Underline**, or **Clear**. Ctrl/Cmd+B, I, or U applies the corresponding mark
to a selected range. The adjacent preview shows the formatting. Typing
and pasted replacement text use plain formatting; the editor retains formatting
only on unchanged ranges it can identify exactly. A replacement can remove an
intersected note reference as well as formatting, so review the notices after
editing. Ctrl/Cmd+Z and Ctrl/Cmd+Shift+Z restore text and metadata together while
the editor has focus. This editor formats one passage at a time. Use the
full-page draft for plain-text selection and paste across passages; rich-text
formatting across passages remains a separate planned increment. Newlines inside
a passage do not create new OCR regions or source-image boxes.

Use **Move block earlier/later** to change reading order. In **Sections and
columns**, choose **Start section here** to begin a section at the selected
block. Choose one or two columns, identify where the second reading column
starts, then apply the column settings. These are explicit reading-order
choices, independent of the source geometry. A heading or note that spans the
page can occupy its own one-column section. **Show structure preview** presents
the chosen roles, formatting, section order, and columns.

In **Figure and note links**, give a figure alternative text or mark it as
decorative, then apply the description. A caption's **Caption source** selects
the figure or table it describes. For a note, first give its passage the
**Footnote** role; select the reference marker in another passage, choose the
**Footnote target**, and link the selected text. Use **Follow note** in the
preview to navigate to that note. Links are explicit and restricted to valid
targets on the same page; nearby placement alone does not establish a link.

Structure is saved with the project and retained in **Structured JSON**,
including separate figure source geometry. Other output formats warn when they
flatten these semantics until their structured exporters are implemented. The
preview does not promise exact source typography, PDF tagging, or accessibility
conformance. OCR scores continue to describe the original recognition rather
than the chosen formatting or document roles.

### Split, merge, and repair passages

Place the caret inside a passage and choose **Split passage**. The preview keeps
the text on both sides exactly, including whitespace. Choose **Shared source
bounds** when both passages refer to the same image area, or **Divide source
area** to specify an explicit cut and which side contains the first passage.
The app does not infer a source boundary from a newline or invent word boxes
from your cut. The split requires nonempty text on both sides of a complete
character; a selected range is not treated as permission to discard text.

**Merge next passage** combines the immediately following compatible passage in
reading order. It cannot jump over excluded content, tables, pictures, or formulas.
Inspect **Merged passage preview** and choose **No separator**, **Space**, **Line
break**, or **Blank line**. No separator preserves the exact concatenation, which
is useful for restoring a split whose second passage begins with whitespace.
Each split or merge is one undoable operation and reopens the affected approval.

Structural edits retain **Source components** containing the original text,
recognition scores, native word evidence when present, and captured source image
coverage. Expand a component to compare its raw OCR and source crop. Repeated
split/merge operations reuse these original records rather than nesting or
duplicating their history. The new passage has manual confidence; original
scores remain attached to their source records. Text exports preserve explicit
passage separators; JSON exports also retain the source components.

**Shared source area** means the text passages have unresolved overlapping image
coverage. Text and JSON remain available, while spatial exports require distinct
coverage or merging the shared passages back together. A manually divided area
describes your chosen coverage, not newly measured recognition geometry.

**Repair passage text** proposes one operation at a time: normalize line endings,
trim trailing spaces, collapse inline spaces, join lines, or remove eligible
end-of-line hyphens. Inspect **Current passage** and **Proposed passage**, then
select the exact changes to apply. Spacing changes, line joins, and dehyphenation
start unchecked because alignment and intentional hyphens may carry meaning.
The conservative proposals retain tabs, indentation, paragraph breaks, lists,
numbers, and identifiers; they do not provide semantic paragraph reconstruction.
Cancel preserves the text, and one Undo restores an accepted repair. Original
recognition evidence remains available after a repair.

### Recognize only the text that needs another pass

Select **Re-OCR region** on a region or passage, or use **Re-OCR area** in the
source-image toolbar and drag over the relevant text. Drawing across part of an
existing passage includes the whole passage so recognition cannot silently erase
its remaining words. Press **Escape** to cancel an unfinished drawing.

After recognition, **Review new OCR text** displays the current text and the
proposed OCR text side by side. **Keep current text** dismisses the result without
changing the project. **Use new OCR text** replaces only the covered source
regions, preserves other corrections, and requires renewed review. Undo restores
the previous text and evidence. Recognition never overwrites the current text
before this comparison.

When the covered passages already have source components, an accepted new OCR
result retains those originals and adds the new recognition snapshot. Current
word scores describe the new result; earlier evidence remains in its own source
record.

Empty recognition, failed inference, invalid crop coordinates, or a result for a
different page image leave the existing text intact. If you navigate while OCR
is running, the result remains attached to the source page captured when the
operation started; applying it returns to that page. A stale result cannot
overwrite intervening changes to its source page. Navigating or leaving drawing
mode cancels an unfinished source-image gesture.

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

### Inspect and correct individual words

Select a region in the page image or its numbered review card to open **Word
confidence** beneath the text editor. Words are colored in flowing text that
preserves the source punctuation, whitespace, and line breaks. Turn on **Show
scores** to display percentages beside the words, or **Unscored** when the
recognizer did not supply evidence that can be aligned to a word. The separate
**Region** badge describes the whole original OCR region; it is never copied onto
individual words.

Click a word to select its exact text in the region editor, then type the
correction. **Next word to check** cycles through words with missing scores or
scores below the review threshold within the selected region. Use **Next
issue** or **F8** to continue through regions and pages. The source magnifier shows
the selected word when the recognizer supplies its genuine source box; otherwise
it shows the selected region. Word boxes are not inferred from a line's dimensions.

**System Tesseract · word evidence** uses an available local Tesseract installation
and its language data to obtain word scores and source boxes. Its original word
score controls the word's color directly. The tooltip identifies it as an
**Original word recognition score**; it is not a character average or minimum.
Native token boundaries, including punctuation within a token, are retained.
When a native word has no score, it stays **Unscored** while retaining its box;
other confidence values are not substituted for the missing score.

When verified Unicode-codepoint recognition scores are available, the displayed
word score is their **mean**, and the tooltip also reports the **lowest character
score**. The word's color follows its weakest character, so a high mean cannot
hide an uncertain glyph:

| Color | Meaning |
| --- | --- |
| Green · High | The native word score, or every aligned character score, meets the project's review threshold, 85% by default. |
| Amber · Medium | The native word score, or weakest aligned character, is below the review threshold but at least 60%. |
| Red · Low | The native word score, or weakest aligned character, is below both 60% and the review threshold. |
| Gray · Unscored | Verified word confidence is unavailable. |
| Blue · Edited | The text was manually changed; original scores do not measure the correction. |

A word with an 87% mean and one 20% character is therefore red, even at the
default 85% review threshold. Changing the threshold in **Recognize → Review
threshold** updates the colors and review flags together. The legend, optional
score labels, and tooltips explain the colors. These are review signals, not
calibrated word accuracy probabilities. Generic model-token likelihoods, detector
scores, invalid arrays, and contradictory alignment metadata cannot produce word
scores. Valid native word evidence takes precedence over character-derived scores.

The currently pinned native CrispEmbed integration does not supply verified word
offsets or Unicode character alignment, so its words normally appear
**Unscored**. Its region scores remain useful separately. The separate System
Tesseract provider supplies direct word evidence; adapters with verified character
alignment can also supply derived word scores. The two browser fixtures exercise
these contracts with synthetic inputs. Adding verified alignment to the other
native engines requires matching bindings and a library release that exports it.

After any text correction, the current region's words turn blue for **Edited**.
**Original OCR** retains the original text and its available scores for
comparison; its words are read-only because their offsets may no longer
match the corrected text. Use **Tab** to focus an original word and **Enter** to
inspect its available scores without selecting or changing the corrected text.
When an original word has a source box, inspection also highlights and magnifies
that saved box. Current words remain **Edited** after any change in the region;
the app does not guess how original word spans map onto a rewritten passage.
**Current text** returns to the editable word view.
Corrections use the existing autosave, undo/redo, approval, and export workflow.
Editing an approved region revokes approval, and source-image warnings follow the
same review rules as the issue queue, including low native word or character scores and
unapproved corrections.

### Vocabulary and correction rules

Open **Vocabulary and rules** to maintain terms for **This project** or **All
projects**. Project vocabulary travels with the OCR project. User-wide vocabulary
is saved separately and is unaffected by undoing a document edit. Give terms and
rules the intended language; a term in one language is not silently used as a
dictionary for another.

Select a current word to add it to vocabulary, **Protect identifier**, or **Ignore
suggestion** for that exact occurrence. These actions do not correct the text,
approve it, or raise its OCR confidence. Protected terms can represent product
codes, names, and identifiers that must remain unchanged by suggestions and
literal correction rules. An ignored occurrence remains tied to its page, source
image, and exact text version.

Vocabulary suggestions are review aids, not retraining of the OCR model. **Use
suggestion** opens **Review proposed corrections**; inspect the replacement before
applying it. Cancel leaves the text unchanged. An accepted correction uses the
same original-evidence preservation, autosave, approval invalidation, and Undo
behavior as a manual text edit.

Saved rules replace literal text. Configure language, case matching, and whole
word matching, then choose the correction scope and preview the rule. The preview
lists eligible occurrences and reports protected or approved text that was
skipped. Select only the occurrences you intend to change. Applying the preview
is one undoable operation; changing a target passage or the vocabulary policy
makes an old preview invalid rather than applying stale text offsets.

If user-wide vocabulary cannot be saved, the draft stays visible for **Retry
saving vocabulary**. If its saved data cannot be loaded, user-wide edits remain
disabled; **Retry loading vocabulary** reads the existing data again before
allowing changes. A load error is never treated as permission to replace the
existing vocabulary with an empty list.

Vocabulary JSON import merges entries and rules in the selected scope; export
retains their languages, case settings, and protection choices. Exact occurrence
ignores belong to their source project and are not included in portable vocabulary
exports. **Clear ignored occurrences** makes those occurrences eligible for
suggestions again without approving or changing their OCR text.

The active proofreading language is separate from recognition language. `und`
means unspecified; language-neutral protected entries apply across languages.
Case-insensitive matching uses Unicode case folding, and canonically equivalent
Unicode spellings retain exact source UTF-16 ranges. An accepted vocabulary word
does not receive a nearby vocabulary suggestion merely because another word is
similar. Actual engine alternatives and explicit saved rules remain separate.

Rule previews work on included text passages; tables, formulas, pictures, and
excluded passages are reported as skipped. Use their dedicated editors or manual
text editing for those objects. Previews are bounded to 5,000 correction matches
and one million source characters; large projects can be processed with selected
pages or passages. Protection matching has its own work limit.

See [Proofreading, recognition hints, and model training](ocr-proofreading-engine-support.md)
for the inspected engine interfaces and the outstanding requirements for training.

### Physical page sizes

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

The **Extract** panel works on the current page or selected region. Select a
source region in **Review**, choose **Fields**, **Table**, or **Formula**, and run
the corresponding extraction. Each result is saved as a separate object:
extracting a second table or formula keeps the earlier one available. Use
**Saved extraction** to choose an object of the current kind, give it an
**Object name**, or mark it **Exclude object**.

Every object keeps its captured source page, document, image dimensions, selected
region and bounds, extraction method, and the reviewed text used for text-based
fields. **Original model output** exposes the exact raw response and the original
structured data separately from your current edits. Original scores continue to
describe that original response; changing a field or cell does not create a new
recognition score.

- **Fields:** GLiNER reads the reviewed text; the layout option runs LiLT against
  page pixels. Choose the field labels and minimum extraction score, then correct
  the returned values. **Save fields as JSON** writes the complete selected
  object, including original and current values, source captures, review and
  exclusion state, and the original model response.
- **Tables:** native recognition returns table HTML and dimensions. The editor
  displays inert, escaped cell text and retains validated row/column spans;
  returned HTML is never inserted into the live DOM. Cells accept multiline text.
  A source box is retained only when it is supplied as genuine cell evidence.
  **Save table as CSV** writes the selected table's current cell values, placing
  an empty value at positions covered by a merged cell. Formula-like entries are
  prefixed as literal text for spreadsheet readers, while the stored cell strings
  and structured JSON remain unchanged. Inspect merged cells against the captured
  source; CSV cannot carry their span metadata.
- **Formulas:** native math recognition returns LaTeX. Correct it in **LaTeX**,
  inspect the rendered formula preview, then copy it or use **Save formula as
  LaTeX** for a `.tex` file. The original model LaTeX remains available alongside
  the current version.

Edits belong to the selected object and participate in project history and
autosave. Ctrl/Command+Z and Ctrl/Command+Shift+Z inside the extraction editors
restore the project data together with the displayed value. Saved objects and
their original evidence survive closing and reopening the project. The document's
**Structured JSON** export retains the versioned collection at
`pages[].provenance.extractionObjects`, including original outputs, structured
baselines, current edits, and source captures.

A result that finishes after page navigation remains attached to its captured
source page. **Current source** means its inputs still match that page.
**Historical source** means the image, selected geometry, or reviewed input text
has since changed. Historical objects remain selectable with their original
source image and edits available for inspection; a changed page does not silently
replace or discard them. Review their provenance before using them with a newer
page image.

**Update search index** uses the approved corrected text for the current source
document, feeding CrispSorter's existing chunking and semantic/full-text indexing
pipeline.

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

Native editing shortcuts continue to work inside text inputs. When a page has
document structure, Ctrl/Cmd+Z and Ctrl/Cmd+Shift+Z in its passage editors use
project history so text, formatting, and note references restore together.
The unapplied full-page draft keeps native textarea Undo until Apply; afterward,
project Undo restores the applied change. Toolbar Undo remains available outside
the draft dialog while a text field has focus.

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

Desktop release recipes enable the reviewed PDF renderer and keep its source
and native library pinned together. Runtime capability checks still determine
whether a particular installed build can export reviewed PDF. The paired CI
workflow pins an immutable native source revision, builds its library, and
checks the Rust consumer against that exact revision. For development, use
`--features ocr-workspace` (plus `desktop` for desktop-only features) with the
matching native library. Never mix a new Rust binding with an older prebuilt
native library. Renderer availability does not imply PDF/A or PDF/UA conformance.

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

The [OCR workspace qualification guide](../tests/quality/ocr-workspace-qualification.md)
and its receipt schema keep synthetic UI, native parser, native provider, real
corpus, device, and standards results separate. A passing browser fixture does
not qualify live recognition or an installed application. Each receipt records
the actual source version, runtime and model identities, effective settings,
input/output checksums, timings, and any blocked or unrun checks.
