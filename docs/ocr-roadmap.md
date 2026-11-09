# OCR roadmap

This is the scoped implementation plan for closing the gaps between CrispSorter's
document review workspace and established desktop OCR workflows. It covers the
whole application, including existing PDF, DOCX, indexing, template, and watched
folder features. It is not a claim of equivalent recognition accuracy or format
fidelity.

The starting point is commit `3c11da6` on `feat/ocr-word-review`, audited on
**2026-11-09**, with the runtime pinned by
[`crispembed-runtime-lock.json`](../scripts/crispembed-runtime-lock.json) to
CrispEmbed **0.17.16**. The interactive baseline is described in
[OCR document workspace](ocr-workspace.md); pipeline and CLI behavior are in
[OCR workflow](ocr-workflow.md).

## Status and delivery rules

The initial scope checkpoint was `12caadb`. The status table and delivery log
below record subsequent increments. Existing features are recorded separately as the baseline. A package
is complete only when its stated acceptance boundary has evidence; a prototype,
an accepted engine ID, or a synthetic fixture does not close its qualification
work.

Track these states separately when implementation starts:

| State | Required evidence |
| --- | --- |
| Planned | Scope, dependencies, and acceptance criteria below |
| Implemented | Reviewed code, migration behavior where applicable, relevant automated tests, and a pushed feature-branch commit |
| Native verified | The pinned application, binding, native library, and model combination passes real integration cases |
| Qualified | The declared platforms, document corpus, hardware, languages, or output standard pass the package's additional checks |

Use small commits for the lettered increments. Push each coherent, verified
increment to the feature branch and record its commit and test evidence in this
document before starting the next. Keep documentation accurate when only part of
a work package is implemented. A native library change, model evaluation,
physical scanner test, or independent standards audit is a separate deliverable
from its frontend controls.

Preserve these invariants in every increment:

- Source assets and original OCR evidence remain recoverable. Proposed
  replacements identify their source revision and cannot overwrite a changed
  project silently.
- Corrections, structural edits, and changed recognition invalidate the affected
  approval. Undo, autosave, reopening, and exports use the same current model.
- Missing confidence stays missing. Detection confidence, recognition confidence,
  character averages, and calibrated correctness are different quantities.
- Output capabilities describe what the installed build can actually produce.
  Unsupported content must produce an actionable limitation, not a success label
  on incomplete output.
- Every new automated operation has a visible result or failure. Source deletion,
  upload destinations, and unattended acceptance of uncertain text require an
  explicit user configuration; enabling a watcher does not imply them.

## Existing baseline to retain

These are implemented paths, with their present limits. Their presence does not
imply that every engine, document, platform, or output format is qualified.

| Area | Existing implementation | Boundary relevant to this plan |
| --- | --- | --- |
| Project and recovery | Multi-document projects, atomic revision-aware autosave, immutable source assets, reopen/recovery, and bounded undo/redo; [`model.ts`](../src/lib/ocr/model.ts), [`autosave.ts`](../src/lib/ocr/autosave.ts), [`ocr_workspace.rs`](../src-tauri/src/ocr_workspace.rs) | Undo snapshots are session history; there is no durable, attributed correction ledger |
| Inspection and correction | Region overlays, source magnifier, region and multiline passage editing, word review, review queue, approvals, and literal find/replace; [`OcrWorkbench.svelte`](../src/lib/components/OcrWorkbench.svelte), [`PageTextEditor.svelte`](../src/lib/components/ocr/PageTextEditor.svelte), [`WordReview.svelte`](../src/lib/components/ocr/WordReview.svelte) | Passage editing retains region boundaries; numeric word scores require verified aligned recognition evidence |
| Partial OCR | Selected-region and drawn-area recognition with a before/after preview, cancellation, stale-result checks, and unaffected-region preservation; [`zoneReview.ts`](../src/lib/ocr/zoneReview.ts) | Replaces covered regions after acceptance; it is not a persistent history of all past recognition runs |
| Pages and preparation | PDF/TIFF/image import, arrange/crop/rotate/split, classical cleanup, dewarp, restoration, and super-resolution; [`page_source.rs`](../src-tauri/src/extractors/page_source.rs), [`ocr_workspace.rs`](../src-tauri/src/ocr_workspace.rs) | File import is not scanner acquisition; each preparation recipe still needs document and model qualification |
| Recognition and extraction | Engine/configuration selection, sequential batch pause/resume/stop/retry, fields, LiLT, table and formula extraction; [`client.ts`](../src/lib/ocr/client.ts), [`extraction.ts`](../src/lib/ocr/extraction.ts), [`ocr_crispembed.rs`](../src-tauri/src/extractors/ocr_crispembed.rs) | Extraction currently stores one result per kind on a page; tables are a flat cell matrix |
| Reviewed exports | Text, JSON, Markdown, HTML, structured DOCX, and build-dependent hOCR/ALTO/searchable PDF; `prepareExport` in [`model.ts`](../src/lib/ocr/model.ts), `export_bytes` in [`ocr_workspace.rs`](../src-tauri/src/ocr_workspace.rs), [`ocr_render.rs`](../src-tauri/src/extractors/ocr_render.rs) | DOCX supports the declared reflowable structures with explicit fallback warnings; XLSX remains next; OCR PDF/A is rejected; reviewed PDF has script and renderer limits |
| Surrounding document tools | PDF page operations, annotations, AcroForms, text insertion/substitution, compression, and DOCX transformations; [`PdfWorkspace.svelte`](../src/lib/components/PdfWorkspace.svelte), [`PdfEditor.svelte`](../src/lib/components/PdfEditor.svelte), [`PdfTools.svelte`](../src/lib/components/PdfTools.svelte), [`DocxTools.svelte`](../src/lib/components/DocxTools.svelte) | These tools are not a structured OCR editor. Existing PDF paragraph reflow and complete PDF archival/accessibility conformance are not established |
| Search and repeat processing | Full-text/semantic library search, cloud-source integrations, approved-text reindexing, watched-folder ingestion, template/zone backend, and text-comparison backend | These foundations need the guided workflows below; rule action emission alone is not a durable automation executor |

Existing tests and evaluations are assets to extend, not replace:
[`src/lib/ocr/`](../src/lib/ocr/),
[`tests/ocr-workspace/`](../tests/ocr-workspace/),
[`tests/quality/`](../tests/quality/), and
[library model evaluation](library-model-evaluation.md).

## Priorities and dependencies

**P0** makes review evidence trustworthy and reduces correction work. **P1**
finishes common document workflows. **P2** covers wider acquisition, archival,
and specialized output. Priority describes impact; lettered increments are the
units to implement and push one at a time.

| ID | Priority | Work package | Principal dependency | Status |
| --- | --- | --- | --- | --- |
| OCR-01 | P0 | Native word evidence and honest capabilities | A verified engine result/offset contract | 01a–c implemented for System Tesseract; focused native, paired application, and browser CI passed |
| OCR-02 | P0 | Dictionaries, suggestions, and reusable corrections | Stable text ranges; OCR-01 for engine alternatives | 02a–c implemented; 02d feasibility documented; training blocked |
| OCR-03 | P0 | Passage split/merge and text repair | Existing project/history model | 03a–c implemented; 30 browser interactions and focused/paired native export checks passed |
| OCR-04 | P1 | Document structure and formatting editor | OCR-03 semantics and versioned migration | 04a–c bounded workflows implemented; all 34 browser interactions and 343 unit tests passed for the page-draft checkpoint |
| OCR-05 | P1 | Multiple tables/formulas and structural repair | Stable region IDs; OCR-04 shared block links | 05a–c implemented; 421 unit tests and typecheck pass; expanded browser qualification pending |
| OCR-06 | P1 | Structured DOCX/XLSX and export profiles | OCR-04 and OCR-05 | 06a implemented; 22 native module tests, 433 frontend tests and three independently rendered DOCX fixtures passed; app/browser CI pending; 06b–c planned |
| OCR-07 | P1 | Reusable form and zone templates | Existing templates backend; OCR-03 geometry rules | Planned |
| OCR-08 | P1 | Full comparison and review navigation | Existing diff/search; revision identity | Planned |
| OCR-09 | P1 | Persistent OCR automation executor | Existing watcher/queue; export capability contract | Planned |
| OCR-10 | P2 | Scanner and camera acquisition | Import pipeline; platform/device adapters | Planned |
| OCR-11 | P1 | Preparation previews and quality guidance | Existing cleanup/restoration pipeline | Planned |
| OCR-12 | P1 | Multilingual and RTL output | Paired renderer/shaping support; OCR-01 offsets | Planned |
| OCR-13 | P2 | Validated PDF/A output | Content-complete renderer and independent validator | Planned |
| OCR-14 | P2 | Tagged PDF and PDF/UA | OCR-04 structure, OCR-12 text mapping | Planned |
| OCR-15 | P2 | OCR-specific PDF compression | Existing compressor; output validation fixtures | Planned |
| OCR-16 | P1 | Integration and durable correction provenance | Stable source/revision identifiers | Planned |
| OCR-17 | P1 | Accessible review interactions | Baseline and each new UI increment | Planned |
| OCR-18 | P0 | Reproducible quality and release qualification | Starts now; accompanies all other packages | 18a implemented; other increments planned |

Default sequence: commit this scope first; establish the OCR-18a evidence manifest;
then implement OCR-01a, OCR-02a/b, and OCR-03a/b as separate reviewable increments.
Continue through structure, tables, exports, and workflow integration before
specialized standards or hardware claims. Start an upstream or device dependency
early, but do not mark it complete while implementing an independent software
slice. OCR-17 checks accompany each UI change, rather than being deferred until
the end.

## Delivery log

| Checkpoint | Delivered | Evidence and remaining boundary |
| --- | --- | --- |
| `12caadb` | Scoped OCR-01 through OCR-18; corrected capability and archival-output claims | Documentation audit; no new product capability claimed |
| `fae6cc7` — OCR-18a | Versioned qualification receipt schema, recording guide, and a dated same-run Tesseract TSV/TXT capture | JSON Schema validation and artifact hashes checked. External Tesseract 5.5.0 produced five aligned words. Production Rust adapter, full app, real-scan corpus, devices, and output standards remain separate gates |
| `349725a` — OCR-01a–c | Strict UTF-16 word evidence, explicit installed System Tesseract provider, direct score colors, exact source-word highlight/magnifier, immutable original evidence in projects and JSON | 235 unit tests passed; Svelte check had 0 errors and 54 existing warnings. [Focused Linux/Windows CI](https://github.com/CrispStrobe/CrispSorter/actions/runs/42682663208) and [paired application CI](https://github.com/CrispStrobe/CrispSorter/actions/runs/42682663303) passed. The Linux run exercised the actual Rust adapter against installed Tesseract; Windows exercised parser/feature gates. Corpus accuracy and device qualification remain separate |
| `6330227` — OCR-02a–c and 02d feasibility | Project and personal vocabulary, protected terms, occurrence ignores, labeled suggestions, literal rules, selective stale-safe correction previews, import/export, and serialized retryable preference saves | 253 unit tests passed; Svelte check had 0 errors and the same 54 unrelated warnings. [Browser CI](https://github.com/CrispStrobe/CrispSorter/actions/runs/42685445730) passed the 28-interaction suite, including native-evidence UI and proofreading. No recognizer training or automatic native lexicon change is claimed |
| `f3984fd` — OCR-03a–c | Grapheme-safe passage split, explicit source coverage, adjacent merge with exact separators, selectable repair proposals, inspectable immutable source components, and preservation through later re-OCR | 280 full unit tests passed; Svelte check had 0 errors and 54 existing warnings. [Browser CI](https://github.com/CrispStrobe/CrispSorter/actions/runs/42688791807) passed all 30 interactions and 175 OCR unit tests. Focused Linux/Windows native checks passed, including Linux live recognition. [Paired native export checks](https://github.com/CrispStrobe/CrispSorter/actions/runs/42688791842) passed, including all 29 workspace cases |
| `46a3e04` — OCR-04 structure | Versioned blocks and contiguous sections, explicit two-column order, per-passage roles/emphasis/alignment/breaks, figure descriptions, caption and footnote links, source-linked JSON, and bounded structure history | 306 full unit tests passed locally; Svelte check had 0 errors and 54 existing warnings. [Browser CI](https://github.com/CrispStrobe/CrispSorter/actions/runs/42692620197) passed all 32 interactions and 201 OCR unit tests. [Focused Linux/Windows checks](https://github.com/CrispStrobe/CrispSorter/actions/runs/42692619973) passed, including the production semantic validator on both platforms. [Paired application checks](https://github.com/CrispStrobe/CrispSorter/actions/runs/42692619960) passed, including 30 workspace cases, both structure cases, the other declared native suites, and the desktop compile gate. Structured file writers remain OCR-06 |
| `ed0480a` — OCR-04 page-text draft | Whole-page plain-text selection/editing, durable unapplied drafts, exact affected-passage previews, explicit combined source coverage, stale/tampered preview rejection, and draft-aware project history | All 343 unit tests in 38 files passed locally and in CI, including native JSON key sorting and failed/pending-page repair; Svelte check had 0 errors and 54 existing warnings. [Browser CI](https://github.com/CrispStrobe/CrispSorter/actions/runs/42696088961) passed all 34 interactions, including draft recovery and cross-passage coverage. [Report and screenshots](https://github.com/CrispStrobe/CrispSorter/actions/runs/42696088961/artifacts/13711038096) retain the execution evidence |
| `4904354` — OCR-05a independent extraction objects | Multiple editable tables, formulas and field sets; immutable original output/data; actual captured image/text; historical retention; table spans/headers; safe literal CSV; bounded formula preview; per-object export and project review/undo | All 379 unit tests in 41 files passed locally and in CI; Svelte check has 0 errors and 54 existing warnings. Its [browser run](https://github.com/CrispStrobe/CrispSorter/actions/runs/42699692705) passed 33 interactions and failed two: a fixture geometry assertion and CSV using the JSON serializer. Both were already corrected in `5ecd539`; that later revision still needs its own browser evidence. Native structured DOCX/XLSX consumption remains OCR-06 |
| `5ecd539` — OCR-05a export-format correction | Table copy/save emits literal CSV, formula copy/save emits exact LaTeX, and field copy/save retains complete source-linked object JSON; format and extension share one decision | The isolated correction passed all 382 unit tests in 42 files, including three exported-content regressions. Svelte check has 0 errors and the same 54 existing warnings; browser harness syntax and whitespace checks passed. Two browser assertions now use the actual flat region geometry. Browser execution remains pending CI |
| `691be05` — OCR-05b table structure repair | Previewed insert/delete row/column, explicit rectangular merge separators, span splitting, header changes, captured-source inspection, exact removed-value review, stale/tampered preview checks, and one-step Undo | All 405 unit tests in 43 files passed locally and in CI; Svelte check has 0 errors and 54 existing warnings. Its [browser run](https://github.com/CrispStrobe/CrispSorter/actions/runs/42701880471) passed 34 interactions and failed two: approval navigation still on Extract, and a compact Save button without an accessible name. The original data and unrelated objects remain unchanged; the follow-up correction is recorded below |
| `e112040` — OCR-05c continuation joining | Explicit ordered joins, complete column mapping, repeated-header review, separate editable results, flat source snapshots and receipts, inspectable manual baselines, stale application checks, and later-source-change detection | All 421 unit tests in 45 files passed, including 12 core and four project-lifecycle cases. Svelte check has 0 errors and the same 54 existing warnings. [Paired native CI](https://github.com/CrispStrobe/CrispSorter/actions/runs/42705067117) passed all 30 workspace cases and the other declared suites, including the desktop compile gate. [Focused Linux/Windows CI](https://github.com/CrispStrobe/CrispSorter/actions/runs/42705066693) passed. Browser execution is still pending. Source tables and their inclusion settings remain unchanged. No cell typing, formula evaluation, or native recognition-accuracy claim is introduced |
| `b10cf4e` — OCR-05 accessible Save and browser navigation | Stable Save button accessible name at compact widths, actual autosave-state observation, and explicit Review navigation before page approval | Typecheck has 0 errors and the same 54 existing warnings; final harness syntax and whitespace checks pass. The 05b failure screenshots were inspected. [Browser execution against this correction](https://github.com/CrispStrobe/CrispSorter/actions/runs/42707630172) remains pending; the earlier 34-pass/2-failure run is not a pass for this change |
| OCR-06a structured Word export (this checkpoint) | Reviewed headings, lists, styled ranges, reading columns, breaks, current-image figures, explicit editable table/formula selection and placement, native provenance validation, and disclosed fallback notes | All 433 frontend tests in 46 files passed; Svelte check has 0 errors and 54 existing warnings. The production-module harness passed 22 native tests, matched every registry dependency to the production lock, generated three DOCX files, and passed independent ZIP/XML checks. LibreOfficeDev 26.8.0.0.alpha0 reopened all three; all six rendered pages were inspected. [Exact fixtures, logs, locks, receipts and images](../tests/quality/results/2026-11-09/office-docx/README.md) retain the result and the initial numbering collision that was corrected. The 38-flow browser suite and complete app adapter execution await CI; Microsoft Word and schema validation remain separate gates |

## OCR-01: Native word evidence and honest capabilities

**Status: 01a–c implemented for System Tesseract; focused Linux/Windows, paired
application, and browser CI passed. Broader corpus qualification remains open.**
The explicit provider, validator, source-word inspection, and original
evidence persistence are implemented in
[`ocr_word_evidence.rs`](../src-tauri/src/ocr_word_evidence.rs) and the
[word evidence contract](ocr-word-evidence.md). The UI also shows word scores from explicitly aligned
character recognition scores. The pinned embedded path commonly lacks verified
word offsets or Unicode character alignment, so real results may correctly show
**Unscored**. See `hasAlignedCharacterConfidence` in
[`confidence.ts`](../src/lib/ocr/confidence.ts), `getWordEvidence` in
[`words.ts`](../src/lib/ocr/words.ts), and the adapter in
[`ocr_crispembed.rs`](../src-tauri/src/extractors/ocr_crispembed.rs).

**Increments**

- **01a — capability and provenance contract:** report the actual source of
  confidence, available text-range/geometry evidence, native/model versions,
  and why a result is unscored. Preserve unknown evidence across save/export.
- **01b — native integration:** add a versioned result contract for word
  spans or explicitly aligned character spans, recognition scores, optional
  genuine word boxes, and optional native recognition alternatives. First
  implement an explicit `system_tesseract` adapter using installed Tesseract's
  TSV and text outputs, distinct from the CrispEmbed `tesseract` engine. Report
  installation/language availability and qualify its real word ranges and boxes.
  For later CrispEmbed engines, update the binding and native library together
  whenever their existing ABI cannot provide this evidence.
- **01c — precise inspection:** when genuine word geometry exists, focus the
  source magnifier on that word. Retain the region crop when it does not. Add
  further engines only with their own evidence tests.

**Acceptance and validation:** test emoji, combining marks, ligatures, whitespace,
punctuation, CJK, multiline text, invalid lengths, token scores, detector scores,
and out-of-range values. Specify native offset units and convert to UTF-16 editor
ranges explicitly. Edited text must not inherit old word scores; the original
view retains the original evidence. Threshold behavior must agree among the
word UI, overlay, queue, and export metadata. Record real native fixtures and
engine/model versions. Character averages are described as averages, not as the
probability that a word is correct. Calibration is a separate measured result
under OCR-18; no word box may be synthesized from text length and presented as
native evidence.

## OCR-02: Dictionaries, suggestions, and reusable corrections

**Status: 02a–c implemented; browser CI passed. 02d feasibility decision
documented; training remains blocked on its data/toolchain/evaluation gates.**
The [workspace guide](ocr-workspace.md#vocabulary-and-correction-rules) describes
the shipped controls. Versioned project/personal preferences and captured
literal previews live in [`proofreading.ts`](../src/lib/ocr/proofreading.ts),
[`proofreadingData.ts`](../src/lib/ocr/proofreadingData.ts), and
[`proofreadingStore.ts`](../src/lib/ocr/proofreadingStore.ts).
The [engine support decision](ocr-proofreading-engine-support.md) separates app
vocabulary, currently unwired native recognition hints, and model training.
The reference workflow is [ABBYY text verification][abbyy-proofing].

**Increments**

- **02a — vocabulary:** persist user and project dictionaries with explicit
  language, add/remove/import/export, and an ignore decision distinct from
  approving the OCR. Support protected identifiers and specialized vocabulary.
- **02b — suggestions:** offer conservative spelling/confusion suggestions and
  applicable native alternatives. Show original text and affected ranges; accept
  one occurrence or preview selected occurrences. Never apply a suggestion just
  because its string is in a dictionary.
- **02c — reusable correction rules:** store user-created literal or bounded
  pattern rules with scope, language, examples, and preview. Treat field regex
  validation as validation, not as recognition certainty.
- **02d — engine training feasibility and adapter:** document which installed
  engines can consume lexicons or trainable patterns. Add training only for a
  supported engine with versioned training data, opt-in collection, reset/export,
  and a held-out evaluation. A correction memory alone is not model training.

**Acceptance and validation:** dictionaries survive reopening, are scoped and
removable, and handle Unicode/case policy without corrupting source spelling.
Suggestions leave identifiers, dates, quantities, and ambiguous words untouched
until accepted. Batch replacements show counts and support one-step undo; stale
ranges are rejected. Test false suggestions and multilingual vocabulary, not
only successful replacements. Use a licensed, packaged dictionary source or
clearly identified user data. Report correction-time benefit and introduced
errors on held-out documents before claiming recognition improvement. Engine
training restrictions differ; [ABBYY's training guidance][abbyy-training] is a
workflow reference, not evidence that CrispEmbed exposes the same facility.

## OCR-03: Passage split/merge and text repair

**Status: 03a–c implemented; 30 browser interactions and focused/paired native
export checks passed.** The [passage guide](ocr-workspace.md#split-merge-and-repair-passages)
describes the controls. [`passages.ts`](../src/lib/ocr/passages.ts) captures and
revalidates split/merge/repair previews; [`passageData.ts`](../src/lib/ocr/passageData.ts)
retains flat source records with their own image, original text, and evidence.
New structural passages carry manual confidence. Explicit separators preserve
the exact text of a split through copying, search projections, and export.
Shared coverage blocks spatial export while exported siblings overlap; a manual
source division or complete merge resolves it. Existing source records also
survive accepted partial/full re-OCR and image changes. Automatic repairs are
restricted to included text-like passages and reviewable literal ranges; they
do not infer semantic paragraph structure or change table/formula content.

**Increments**

- **03a — explicit passage split:** choose a text boundary and specify how source
  geometry is divided or shared. Preserve a link to the original recognition;
  ambiguous geometry requires a visible choice.
- **03b — explicit passage merge:** select adjacent passages in reading order,
  preview separators and source coverage, and merge atomically. Preserve the
  component source links and expose the changed reading order.
- **03c — repair proposals:** preview line-break normalization, end-of-line
  dehyphenation, whitespace cleanup, and paragraph joins. Preserve intentional
  hyphens, lists, identifiers, and page boundaries unless the user selects them.

**Acceptance and validation:** no edit infers region boundaries merely by
splitting on newline. Split/merge preserves all text and source references,
invalidates affected review/extraction evidence, and survives undo, save, reopen,
partial OCR, search, and export. Empty selections and incompatible cross-page
merges are explained. Test paragraphs containing multiple line breaks,
multi-column order, hyphenated names, negative numbers, and formula/table regions.
Semantic paragraph repair and page-image spread splitting remain separate tools.

## OCR-04: Document structure and formatting editor

**Status: 04a–c bounded workflows implemented. The structure checkpoint passed
32 browser interactions and focused Linux/Windows native validation. The
page-text draft checkpoint passed 343 unit tests, the typecheck, and all 34 browser
interactions. The paired application checks for the structure checkpoint are
still in progress.**
[`structureData.ts`](../src/lib/ocr/structureData.ts) adds independently versioned
logical blocks, sections, reading columns, styled ranges, and explicit links.
[`structure.ts`](../src/lib/ocr/structure.ts) supplies semantic editing operations;
the Structure view provides correction controls and a preview. Text and reading
order remain authoritative in source regions. Enabling default structure on an
older project preserves its approvals and evidence; subsequent semantic edits
reopen the affected review.

This checkpoint supports paragraph/heading/list/header/footer roles, selected
bold/italic/underline ranges, alignment, page breaks, contiguous sections with
one or two reading columns, figure descriptions/decorative flags, anchored
captions, and same-page footnote links. Columns describe logical reading order,
not newly detected source geometry. Pasted/replaced text is plain; unchanged
prefix/suffix formatting survives, while affected note markers require renewed
linking. The UI reports resets and project Undo includes formatting.

Replacing source regions or images archives up to eight prior structure snapshots
within 16 MB, including their exact text and captured source image. The history
is inspectable and included in project/JSON data; it is distinct from a full
attributed audit ledger. Current JSON also retains included picture source
regions so figure links resolve. OCR-06a now consumes the supported structure in
DOCX. Other file formats explicitly warn that they use the plain-text/source-image
projection; unsupported Word semantics receive their own specific warnings.

The separate **Edit page text** dialog adds a whole-page plain-text draft with
native selection, typing, paste, and local Undo/Redo. Review shows the exact
changed envelope and affected passages. A one-passage edit keeps its ID and
original OCR evidence. An edit spanning adjacent textual passages requires
explicit acceptance of their combined source coverage; it creates one manual
passage with immutable original source components. Newlines stay inside that
passage. Pictures, excluded regions, tables, and formulas remain protected
boundaries, and overlap with another included source region blocks combining.
Formatting or links that would reset are disclosed before Apply. This is a
bounded plain-text editing surface alongside the semantic controls; continuous
rich WYSIWYG editing and arbitrary page-layout reconstruction are not claimed.

One unapplied draft per project is saved with its captured source and remains
recoverable after closing, reloading, or removal/change of the current source.
Apply requires that source to match, using JSON key-order-independent comparison
because native saves can reorder object properties. Drafts are bounded to one
million UTF-16 units and 16 MiB of captured data, with at most 128 affected
passages. Invalid oversized input remains in the open dialog for copying or
reduction and is not reported as saved. Project Undo restores an applied draft;
ordinary Undo/Redo cannot replace a newer pending draft. Apply is one project
history operation and clears only the exact accepted draft. Explicit manual
acceptance also clears a retained failed/pending status while reopening review.
Semantic-only edits now enter the page review queue until page approval.

**Increments**

- **04a — versioned structure:** add stable paragraph/block IDs, typed hierarchy,
  reading order, source-region links, and migration from existing projects.
  Separate logical structure from source-page geometry.
- **04b — editable semantics:** support headings, paragraphs, lists, emphasis,
  alignment, and explicit page/section breaks. Keep plain-text passage editing
  available and define how pasted text changes formatting.
- **04c — complex layout:** add columns, picture/caption anchors, running
  headers/footers, and linked footnotes incrementally, each with a supported
  fixture and a correction UI. Recognized structure is a proposal users can fix.

**Acceptance and validation:** older projects migrate without lost text,
geometry, approvals, or original evidence; newer unsupported versions fail
safely. Rich edits have deterministic plain-text projections for search and
review. Keyboard selection/paste/undo works across supported block boundaries.
Define a bounded supported set first: single-column reports, a two-column page,
headings/lists, one anchored figure, and notes. More complex magazines and exact
font reconstruction require additional fixtures. Do not promise universal exact
typography. [ABBYY's output modes][abbyy-docx] demonstrate the explicit tradeoff
between editing flexibility and source layout that our export UI must explain.

## OCR-05: Multiple tables/formulas and structural repair

**Status: 05a–c implemented; the expanded browser suite still needs execution
against this checkpoint.** The versioned
[extraction object model](../src/lib/ocr/extractionObjects.ts)
retains multiple tables, formulas, and field sets independently. Each accepted
result stores its exact original model output, original structured data, current
corrections, stable identity, and captured source image/text. Actual region crops
keep their source rectangle; no cell geometry is inferred from HTML or text.

The extraction panel selects and edits each saved object, shows its captured
source and original output, and exports selected CSV, LaTeX, or complete field
object JSON. Image changes, passage edits, page splitting, and re-OCR keep earlier
objects with an explicit historical-source status. Legacy records are retained
unchanged; only records with recoverable source measurements are migrated, with
diagnostics for the rest. Object changes reopen page approval and use project
Undo/Redo. Distinct page-level structure/extraction issues can be navigated.

[Table data](../src/lib/ocr/tableData.ts) preserves cell IDs, headers, row/column
spans, and exact string values. The [inert HTML parser](../src/lib/ocr/tableHtml.ts)
uses parse5, omits active subtrees/attributes, retains plain cell/link text, and
never inserts source markup into the document. It rejects overlapping, incomplete,
nested/multiple, or ambiguous grids, nonempty unmodeled captions, and excessive
HTML depth/size. These errors preserve existing results. Raw output is retained
for accepted objects. CSV places a merged cell's text at its anchor and leaves
covered positions empty; formula-like strings remain literal.

Formulas have an editable LaTeX source and a bounded, untrusted KaTeX preview.
The rendered preview is not a mathematical-equivalence check. Corrected fields
retain original model scores separately and do not acquire new confidence.
The collection is bounded to 128 objects/16 MiB per page, with at most 128 source
captures per object, 10,000 table-grid positions, and one million text units per
data object. Current document JSON retains the collection. OCR-06a consumes only
explicitly selected tables/formulas in DOCX; other document formats still use
OCR text/source-image projections until their structured writers are implemented.

[Table repair operations](../src/lib/ocr/tableOperations.ts) insert/delete rows or
columns, merge a complete rectangular selection with an explicit separator,
split spanned cells, and change header roles. The dialog shows the captured
source and exact current/proposed grids; deletion lists the nonempty values to
remove and the grid also identifies empty cells. Merging preserves every input
string in row order, including empty values. Splitting retains the complete
string in the top-left cell and creates empty cells elsewhere. Deleting part of
a span retains all its text; inserting through a span extends it. Changed coverage
drops current pixel boxes, while unchanged/shifted cells retain their measured
source associations. Original data is unchanged.

Each accepted operation reopens page review and creates one Undo step. Preview
application compares the selected object and every captured source state, so
changed source text, geometry, image, or object content requires a new review.
Historical results remain editable with their source status visible. Failed
application retains the operation choices; Cancel changes nothing. Empty tables
start at one cell when inserting and return to exactly 0 × 0 when the final row
or column is deleted. No numeric or formula interpretation is introduced.

[Continuation joining](../src/lib/ocr/continuationJoin.ts) creates a new table
from 2–16 explicitly ordered nonempty inputs across pages. Differing column
counts require a complete, distinct destination mapping for every source column;
source columns cannot be dropped. Spanned cells must map to contiguous columns.
Unmapped destination positions become explicit empty cells without inferred
geometry or header roles. Header removal is an explicit top-row count for each
continuation, never an automatic text match. It cannot cut through a span; the
preview lists exact removed values and warns about differing, nonheader, or
entirely removed continuation rows. The first input retains every row.

The joined object is stored on the first input's page and has a manual baseline,
not a newly recognized model result. Flat versioned snapshots retain each input's
original output/data and its corrected table at joining. Joining a joined table
retains its ancestors through receipt references instead of nesting histories.
The reader checks all origins, mappings, references, cycles and capture conflicts
within the existing limits, plus a maximum of 128 flat source snapshots. Original
source objects and their inclusion flags stay unchanged. Later contributing-table
edits or removals mark the joined result historical without changing its values.
Snapshots and manual baselines remain inspectable and survive Undo/Redo,
serialization, page transforms, splitting, and selected-object/document JSON.
Actual source boxes remain associated with their original capture; logical row
and column placement does not move those pixels.

**Increments**

- **05a — multiple extraction objects:** key each table, formula, or field set by
  stable ID and source region/revision. Preserve multiple results per page,
  independent corrections, and original model output.
- **05b — table structure and repair:** represent cell IDs, row/column spans,
  optional genuine geometry, headers, and text. Add insert/delete row/column,
  merge/split, and keyboard cell navigation with a source preview.
- **05c — continuity and formulas:** let users join selected continuation tables
  with repeated-header handling, and retain multiple editable formula objects
  with source images and rendered previews. Keep inferred numeric types opt-in.

**Acceptance and validation:** two tables and two formulas on one page survive
re-extraction, undo, save, and export independently. Test merged cells, empty
cells, multiline content, irregular model HTML, active HTML removal, and stale
sources. Reject overlapping/invalid spans; do not invent cell boxes. Joining
tables requires explicit mapping when columns differ. Preserve leading zeros,
currency symbols, signs, and exact string values by default. Formula preview is
not a proof of mathematical equivalence. Structural table operations follow the
workflow described in [ABBYY's table editing documentation][abbyy-tables], with
our own model and validation contract.

## OCR-06: Structured DOCX/XLSX and export profiles

**Status: 06a implemented; 22 production-module tests, three generated packages,
independent ZIP/XML checks, and all six LibreOffice-rendered pages passed.
All 433 frontend tests passed. Browser execution and complete application
integration await CI; 06b–c remain planned.** The previous
DOCX branch flattened page text through `export_to_docx`. Its replacement uses
the reviewed document structure and explicit extraction-object selections in a
pure [Word writer](../src-tauri/src/ocr_docx.rs), behind the existing native
[workspace export route](../src-tauri/src/ocr_workspace.rs). OCR XLSX output is
not yet implemented.

**06a supported boundary:** paragraphs retain literal corrected text, whitespace,
tabs, and empty lines; headings, numbered/bulleted lists, emphasis, alignment,
reading columns, and explicit breaks use Word structures. Compatible passages
with an explicit empty/space separator remain in one paragraph. Conflicting
style, column, list, or table boundaries fail with a correction instruction
instead of inserting a different separator. CRLF/lone CR become single visible
Word line breaks, with a warning; original byte sequences remain in project JSON.

Pictures use only the current managed page image and explicit figure bounds.
Selected table objects become editable cell grids with horizontal/vertical spans
and exact string values. No object is selected automatically. Each selection
explicitly appends to its page or replaces one included source region of the same
kind. Duplicate replacements, missing/excluded objects, and selections outside
the chosen page scope fail preflight. A joined table's other source passages can
remain in the document; the UI must disclose those duplicates instead of removing
them silently. Legacy extraction migration is projected into the export payload
without changing the project.

The [native Office boundary](../src-tauri/src/ocr_office.rs) independently checks
the versioned collection, grid coverage, unique IDs, real capture bounds, original
field scores, flat continuation ancestry, and exact joined baselines before
consuming selected values. It never opens provenance image paths. Office options
are versioned and capped at 512 selected objects. XML-incompatible characters
cause an actionable error; they are never stripped.

The locked docx-rs 0.4.20 writer keeps header/footer and note text in body order,
exports formulas as literal LaTeX, and styles table header cells without automatic
repeated rows. Figure alternative/decorative metadata and caption cross-reference
links remain in the project/JSON. These limits produce explicit warnings; no
native Word-equation, footnote, or accessibility conformance claim is made.
DOCX additionally rejects pages above [Word's 22-inch limit](https://learn.microsoft.com/en-us/openspecs/office_standards/ms-oi29500/e7017520-06b4-438f-97d2-3e49f247ca9f)
and tables above a conservative 63 logical columns, consistent with
[Word's row-cell limit](https://learn.microsoft.com/en-us/openspecs/office_standards/ms-oi29500/cc8ad504-f84b-4d8b-ac1d-808e6f7b2a31).
The wider source table model is retained for CSV and the subsequent XLSX writer.

The `ocr-office` feature is included in desktop builds and independently enables
the actual native writer/adapter in the paired test build. `structured_docx`
reports that capability separately from recognition-engine availability. The
standalone production-module fixture harness generated real package bytes and
retained exact source hashes and dependency versions/checksums. Its 22 passing
tests comprise two semantic-validator, nine extraction-boundary, and 11 writer
cases. The independent package checker resolves actual paragraph numbering
references and rejects duplicate definitions; it caught the same failure class
as the initial LibreOffice render. The corrected rerun preserves the requested
start value and bullet format. The native adapter also rejects positional
legacy IDs for explicit object selection or passage replacement.

The [dated evidence directory](../tests/quality/results/2026-11-09/office-docx/README.md)
contains original generated packages, dependency/source receipts, raw logs,
independent reader output, every rendered page, and the initial failure.
LibreOfficeDev 26.8.0.0.alpha0 build
`2c87e51eeaa2b413ff4ae097b2705eea1995d8e5` was the reader. These synthetic cases
establish the recorded structures, literal values, images and breaks; they do
not establish Microsoft Word compatibility, installed-app behavior, arbitrary
document fidelity, or OOXML schema conformance.

**Increments**

- **06a — editable document export:** emit actual paragraphs, headings, lists,
  styled runs, pictures, tables, and explicit breaks from OCR-04/05, with warnings
  for unsupported structures. Preserve corrected text and source order.
- **06b — table workbook export:** emit one named sheet per selected table or
  explicit joined table, preserve cell spans where supported, and keep strings
  literal unless users request numeric/formula interpretation.
- **06c — output profiles:** distinguish plain/reflowable text, editable document,
  source-oriented layout, and searchable page-image PDF. Add picture quality,
  page range, inclusion, and header/footer options; persist the profile used.

**Acceptance and validation:** inspect generated package XML for semantic
structures and reopen artifacts in independent readers. Check text extraction,
table cells/spans, image count, section breaks, notes, and selected page scope.
Render a declared document set for visual review; publish known differences.
Source-oriented layout preserves supported positions but does not guarantee
identical fonts or editable reflow for arbitrary pages. Spreadsheet strings must
remain safe and lossless, including values beginning with `=`, `+`, `-`, or `@`.
Never label a flat paragraph DOCX as layout-preserving. Output choices are
informed by [ABBYY's documented export distinctions][abbyy-docx].

**Implementation boundary:** Office writers must run behind the existing native
managed-source validation, output-path protection, and atomic-write path. The
versioned extraction objects inside page provenance need independent native
validation before their cells or source captures are consumed. Select table and
formula objects by stable IDs and make their document placement explicit when
multiple results share one source region. Preserved continuation inputs and a
joined result must not all be included merely because they exist in the project.
No writer may open arbitrary image paths from an unvalidated provenance record.

The current desktop DOCX dependency is `docx-rs` 0.4.20. Its new structured path
must be exercised by a production-module harness that generates actual output,
since the existing paired workflow's desktop compile check does not execute
that writer. The bounded XLSX writer can use the existing ZIP/XML dependencies
for literal strings and merged rectangles. Validation must check relationships,
defined styles and numbering, text ranges, source pictures, literal spreadsheet
values, and exact selected scope. Independent reader and rendered-output results
must name their producer commit and tool versions; neither a ZIP/XML smoke nor
a compile check establishes general Office compatibility.

## OCR-07: Reusable form and zone templates

**Status: Planned.** Named normalized zones and persistence already exist in
[`templates.rs`](../src-tauri/src/index/templates.rs), with OCR/checkbox
extraction in [`zone_ocr.rs`](../src-tauri/src/index/zone_ocr.rs) and CLI/Tauri
commands. The gap is a guided authoring, application, and review workflow.

**Increments**

- **07a — visual template authoring:** draw/name/type zones, select page geometry,
  save versions, duplicate, import/export, and preview coverage on a sample.
- **07b — application and validation:** map template pages to input pages,
  detect incompatible dimensions/orientation, review extracted values with source
  crops, and apply required/type/regex/checkbox validation rules.
- **07c — repeated-form processing:** use explicit alignment anchors and
  tolerances where supported, report mismatches for human review, and provide a
  template step to OCR-09 without silently accepting failed fields.

**Acceptance and validation:** a template applies reproducibly to declared
compatible layouts; wrong-page, shifted, missing-field, and unchecked-checkbox
cases are visible. Unknown zone confidence is not converted to a passing score.
Template edits do not alter prior reviewed results. Test multiple pages and
several instances of one form plus deliberately incompatible forms. Automatic
alignment requires its own corpus and error thresholds. Existing AcroForm
filling remains a separate PDF capability; this package is document extraction,
not a promise of arbitrary form designer or handwritten-form accuracy.

## OCR-08: Full comparison and review navigation

**Status: Planned.** [`comparison.rs`](../src-tauri/src/index/comparison.rs)
provides `compare_texts`; related commands can compare indexed documents.
`toggleCompare` in [`IndexSearch.svelte`](../src/lib/components/IndexSearch.svelte)
currently presents result metadata/snippets. The OCR project already supports
literal find/replace through `findMatches` and `replaceMatches` in
[`model.ts`](../src/lib/ocr/model.ts).

**Increments**

- **08a — comparison view:** select two full document/project revisions, show
  additions/deletions with context, navigate changes, and synchronize known source
  regions. Clearly mark unmatched pages or missing geometry.
- **08b — proofing search:** add a persistent result list, current-page/project
  scope, issue/language filters, whole-word and case choices, and a preview for
  bulk replacement. Preserve exact source ranges after edits.
- **08c — report and visual comparison:** export a text-difference report with
  source/version identifiers. Add an optional aligned page-image comparison as a
  separate mode; require explicit alignment and tolerance for meaningful images.

**Acceptance and validation:** comparisons use complete text and captured
revisions, not truncated search snippets. Test moved passages, inserted pages,
repeated words, Unicode, and multiple changes in one region. Find navigation
must not leave a stale replacement target after switching editors or pages.
Text differences must not be described as detecting all layout/image changes.
Visual differences do not identify semantic corrections automatically. No
comparison result may overwrite either source document.

## OCR-09: Persistent OCR automation executor

**Status: Planned.** Folder ingestion and the interactive OCR queue work.
[`rules.rs`](../src-tauri/src/watcher/rules.rs) defines actions, but
`emit_automation_actions` in [`watcher/mod.rs`](../src-tauri/src/watcher/mod.rs)
emits evaluated actions without a complete durable side-effect executor.
[`AutomationPanel.svelte`](../src/lib/components/AutomationPanel.svelte) edits
and tests rule JSON. This is partial automation infrastructure.

**Increments**

- **09a — persistent jobs:** capture input identity, rule/profile versions,
  planned steps, output destinations, state, attempts, and errors. Add a dry-run
  view, collision rules, resumable execution, and idempotency keys.
- **09b — OCR workflow execution:** connect recognize, review-exception routing,
  approved export, and reindex steps. Execute supported actions through one
  accountable worker; unsupported actions remain explicit failures.
- **09c — unattended operation:** add opt-in hotfolder/scheduled jobs, settled-file
  detection, resource limits, backoff, retry/cancel controls, and a human exception
  queue. Layer templates and output profiles in as their packages complete.

**Acceptance and validation:** duplicated watcher events and process restarts
must not duplicate exports or apply the same move/upload twice. Test partially
written files, unavailable models, disk exhaustion, conflicting destinations,
network loss, failed classification, cancellation, and restart between each
step. Preserve sources by default and keep unreviewed output distinct from
approved output. Classification-dependent actions wait for actual classification
results. Run a real unattended batch separately from fake-worker tests. The
target workflow is comparable in shape to [OmniPage watched-folder jobs][omni-watch],
not a claim that emitted action events already implement it.

## OCR-10: Scanner and camera acquisition

**Status: Planned.** The existing file importer accepts PDF/TIFF/images. No
direct TWAIN/WIA/SANE acquisition adapter is established in the audited app.
Scanner drivers are explicitly outside the current workspace contract.

**Increments**

- **10a — acquisition interface:** define device capabilities, scan settings,
  page-arrival events, batch identity, cancellation, and error reporting. Provide
  a fake adapter and an external scan-folder import path.
- **10b — first platform adapter:** implement one supported platform's device
  interface, then qualify named devices. Add ADF/duplex where reported by the
  device, keeping page order and source metadata.
- **10c — additional acquisition:** add other operating-system adapters and an
  optional permission-aware camera capture flow, with crop/orientation preview.

**Acceptance and validation:** fake-device tests cover empty feeder, jam,
disconnect, cancellation, duplicate pages, and partial batches. Physical-device
qualification separately records device/driver/OS, flatbed/ADF, duplex order,
resolution, color mode, and recovery after failure. Unsupported settings remain
unavailable rather than silently ignored. Hardware support is listed only after
these runs; a compiled adapter is not broad scanner compatibility. Use the
[FineReader specification][abbyy-spec] and [OmniPage product workflow][omni-product]
as acquisition references, not as our supported-device list.

## OCR-11: Preparation previews and quality guidance

**Status: Planned.** Classical cleanup, dewarp, restoration, and upscaling
already exist in `cleanup_image`/preparation commands in
[`ocr_workspace.rs`](../src-tauri/src/ocr_workspace.rs) and
[`ocr_crispembed.rs`](../src-tauri/src/extractors/ocr_crispembed.rs).

**Increments**

- **11a — reversible preparation preview:** compare original/current/proposed
  imagery at readable zoom, show dimensions and recipe, and apply to an explicit
  page scope. Keep recognition and geometry invalidation clear.
- **11b — diagnosis and selective retry:** surface measurable blur, skew,
  resolution, contrast, or clipping indicators with their limits; propose a
  recipe and compare before/after OCR without discarding the accepted result.
- **11c — additional cleanup:** implement missing image operations such as
  perspective correction, blank-page detection, or background removal only with
  a declared use case and test set; reuse existing native operations first.

**Acceptance and validation:** clean and degraded versions of the same pages
retain small punctuation, pale text, lines, figures, and page dimensions.
Geometry transforms must map source-linked results correctly or invalidate them
explicitly. Quality warnings are hints, not proof of unreadability. Model-based
restoration can alter glyph appearance, so preserve the source and quantify OCR
benefit/harm on the corpus before recommending a default. Do not treat a more
visually attractive output as necessarily better recognized text.

## OCR-12: Multilingual and RTL output

**Status: Planned.** Unicode text exports exist, while the checked PDF path in
[`ocr_render.rs`](../src-tauri/src/extractors/ocr_render.rs) rejects currently
unsupported RTL/directional content. A listed recognition language does not
guarantee end-to-end PDF shaping or text extraction.

**Increments**

- **12a — language/script contract:** distinguish recognition language, spellcheck
  language, logical text direction, and output support. Show the actual renderer
  limitation before export; preserve Unicode without lossy transliteration.
- **12b — native shaping and mapping:** integrate font fallback/embedding,
  bidirectional ordering, shaping, and Unicode text mapping for a declared script
  set. Keep logical reading order separate from glyph placement.
- **12c — multilingual UI and export qualification:** validate mixed-direction
  passages, punctuation, numbers, combining marks, vertical text if claimed, and
  text selection/search in supported output viewers.

**Acceptance and validation:** independent text extraction and copy/search agree
with corrected logical text; rendered glyphs are present and correctly ordered.
Fixtures include Arabic/Hebrew with Latin IDs, Indic combining/shaping examples,
CJK, and ordinary Latin text. Qualification is per script/output/viewer, with
explicit exclusions. Do not remove the current protective rejection until the
paired native renderer passes. OCR-01 range evidence and OCR-04 semantics must
survive direction changes without invalid text selections.

## OCR-13: Validated PDF/A output

**Status: Planned.** OCR export explicitly rejects PDF/A because conformance has
not been validated. Separately, `convert_to_pdfa` in
[`pdf_ops.rs`](../src-tauri/src/pdf_ops.rs) writes identification metadata and an
OutputIntent, while its own comment says it does not remediate fonts/images.
Metadata alone is not an archival conversion result.

**Increments**

- **13a — truthful operation and preflight:** distinguish metadata preparation
  from validated conversion in PDF Tools/CLI and report unsupported content.
  Select a bounded initial target, such as PDF/A-2b, and a pinned independent
  validator before implementing the converter.
- **13b — conformant construction/remediation:** handle required embedded fonts,
  color profiles, metadata, prohibited features, and content dependencies for
  that target. Fail without replacing the source when remediation is unavailable.
- **13c — validation receipt:** validate each produced file, preserve the report
  with output/profile/tool versions, and expose success or actionable failure.
  Add other PDF/A levels only through their own requirements and tests.

**Acceptance and validation:** a corpus of valid and deliberately invalid PDFs
must produce correct validator outcomes, with readable pages and unchanged
corrected text. Include nonembedded fonts, transparency, encryption, attachments,
damaged metadata, and multilingual content according to the chosen target.
Check output using an independently implemented validator, not only the writer.
The OCR `--pdfa` path stays unavailable until its own artifacts pass. Existing
PDFs that cannot be remediated must not acquire a misleading “archival” success
status. Standard-specific requirements and validator versions are recorded in
the implementation receipt; vendor claims in the [FineReader specification][abbyy-spec]
are a product comparison reference, not conformance evidence for our files.

## OCR-14: Tagged PDF and PDF/UA

**Status: Planned.** Searchable text and a reading-order list do not by themselves
create an accessible tagged PDF. A complete tagged/PDF/UA authoring and validation
path is not established in the audited OCR export.

**Increments**

- **14a — semantic export preflight:** require document language/title, reviewed
  reading order, real heading/list/table roles, image alternatives or decorative
  designation, and warnings for missing semantics.
- **14b — tagged PDF writer:** generate a valid structure tree and text/object
  associations, table relationships, and artifact classification from OCR-04/05.
- **14c — PDF/UA qualification:** select the target version, run independent
  structural validation, and test representative documents with assistive
  technology. Preserve diagnostics rather than setting a conformance flag alone.

**Acceptance and validation:** keyboard/screen-reader reading follows logical
order on columns, tables, notes, and mixed-language text. Meaningful figures have
reviewed alternatives; OCR cannot infer their accessibility meaning reliably by
itself. Automated checks and manual assistive-technology review are both required
for a qualified output profile. Revalidate after compression or PDF Tools edits.
Do not describe ordinary searchable PDF as PDF/UA. [ABBYY's PDF settings][abbyy-pdf]
and [specification][abbyy-spec] provide feature references; the chosen standard's
requirements govern acceptance.

## OCR-15: OCR-specific PDF compression

**Status: Planned.** General PDF stream/object compression is implemented in
[`pdf_compress.rs`](../src-tauri/src/pdf_compress.rs). It deliberately does not
silently downsample/re-encode page images. OCR-specific mixed-raster-content
(MRC) processing and qualified image-quality profiles are separate work.

**Increments**

- **15a — explicit quality profiles:** show original/proposed size, resolution,
  and image quality choices for searchable OCR PDFs, with lossless as a supported
  option and a preview for lossy changes.
- **15b — image/MRC implementation:** add deliberate downsampling and, if useful
  on the corpus, foreground/background segmentation with supported codecs.
- **15c — combined export qualification:** retain corrected text, geometry, page
  size, tags, and archival requirements for each advertised profile.

**Acceptance and validation:** compare bytes, peak memory, rendering time, and
small-text/line/image readability on monochrome, color, photographed, and faint
documents. Reject changes that corrupt text extraction or page count. A smaller
file alone is not a passing result; report cases where compression does not help.
Re-run OCR-13/14 validators when claiming those profiles after compression.
MRC is motivated by [established PDF output settings][abbyy-pdf], not assumed to
improve every document.

## OCR-16: Integration and durable correction provenance

**Status: Planned.** The application already has indexing, cloud sources, PDF
and DOCX tool workspaces, and approved-text reindexing. The missing work is a
coherent handoff and durable history, not another independent set of tools.

**Increments**

- **16a — source-aware handoffs:** carry document/page/revision identity from
  library search to OCR review, comparison, export, and existing PDF/DOCX tools.
  Show output destinations and the status of ongoing jobs in one place.
- **16b — durable review ledger:** record correction/recognition/approval events,
  previous evidence references, operation type, timestamps, and a user-supplied
  reviewer identity where applicable. Reopen and export the audit record, with
  retention and size limits. Keep it distinct from bounded interactive undo.
- **16c — reviewed output integration:** explicitly choose whether to update
  indexed text or create a derived file, preserve the original, and make changed
  downstream files detectable. Add connectors through existing permission and
  credential paths, without automatically uploading source documents.
- **16d — adjacent PDF tool boundaries:** document and expose limitations of
  existing text substitution, raster/Form-XObject redaction, and signing status.
  Scope any expansion separately with adversarial content tests; do not imply
  that OCR review implements arbitrary PDF paragraph reflow, complete redaction,
  cryptographic signature validation, or PAdES/LTV.

**Acceptance and validation:** open a library result, review it, export it, and
reindex the chosen corrected text without losing source identity. Reopening a
project can explain which revision produced an export even after undo history is
gone. Partial OCR retains a durable reference to the superseded result when the
ledger is enabled. Test renamed/moved sources, stale external edits, failed
handoffs, storage limits, and audit export. Concurrent team assignment, shared
editing, authentication, and conflict resolution are a separate later system;
a local reviewer field is not collaborative access control.

Code anchors: [`IndexSearch.svelte`](../src/lib/components/IndexSearch.svelte),
[`PdfWorkspace.svelte`](../src/lib/components/PdfWorkspace.svelte), `activate` in
[`OcrWorkbench.svelte`](../src/lib/components/OcrWorkbench.svelte),
`createHistory`/`serializeProject` in [`model.ts`](../src/lib/ocr/model.ts),
[`pdf_text_edit.rs`](../src-tauri/src/pdf_text_edit.rs),
[`pdf_redact.rs`](../src-tauri/src/pdf_redact.rs), and
[`pdf_ops.rs`](../src-tauri/src/pdf_ops.rs).

## OCR-17: Accessible review interactions

**Status: Planned.** The workspace already has labels, keyboard shortcuts, and
numeric geometry controls. Those controls and synthetic browser tests do not
establish full accessibility conformance.

**Increments**

- **17a — baseline audit and fixes:** verify complete keyboard paths for import,
  passage/word review, geometry edits, partial OCR preview, extraction, and export.
  Make focus, current selection, error, busy state, and save status perceivable.
- **17b — scalable presentation:** test high contrast, color independence,
  text zoom/reflow, compact layouts, large documents, and reduced motion.
- **17c — assistive-technology qualification:** use named screen-reader/browser/OS
  combinations, fix reading order and announcements, and publish the tested scope.

**Acceptance and validation:** no operation requires color recognition or precise
dragging as its only input. Modal focus is contained and restored; cancellable
operations have usable controls; word/paragraph selection is announced without
flooding live regions. Keep automated checks in the browser harness and attach
manual keyboard/screen-reader results to each affected workflow. Rich tables and
editors added by OCR-04/05 must pass their own navigation cases. Output-document
accessibility remains OCR-14, separate from application accessibility.

## OCR-18: Reproducible quality and release qualification

**Status: 18a implemented; 18b–d planned.** The
[receipt schema](../tests/quality/ocr-workspace-receipt-schema.json) and
[recording guide](../tests/quality/ocr-workspace-qualification.md) retain the
existing frozen real-scan panel and require boundary-specific results. The first
[external Tesseract capture](../tests/quality/results/2026-11-09/system-tesseract-word-smoke/receipt.json)
is explicitly a synthetic-input executable smoke, not corpus or application
qualification. Existing unit/browser checks and model evaluations already
provide useful evidence. Extend
[`tests/quality/`](../tests/quality/) and
[library model evaluation](library-model-evaluation.md) rather than treating the
product as unevaluated.

**Increments**

- **18a — corpus and receipt contract:** select licensed/public or user-approved
  documents, version ground truth, list supported languages/layouts/degradation,
  and record model/native/app versions, settings, checksums, device, and timing.
  Define pass/fail criteria before comparing candidates.
- **18b — recognition and review measurements:** record character/word error,
  missed text, word-evidence coverage, low-confidence recall, false flags, table
  structure/cell accuracy, field/formula correctness, and human correction time.
  Distinguish raw recognition from post-processing and manual corrections.
- **18c — runtime and recovery:** measure cold/warm latency, peak memory, CPU/GPU
  paths, cancellation, long batches, large projects, interrupted saves, low disk,
  and installer/runtime pairing on declared platforms.
- **18d — output and comparative qualification:** verify independent text
  extraction, rendered layout, PDF standard reports, and device results. Compare
  other products only using the same corpus/settings, licensed access, and
  disclosed methodology; do not infer an accuracy ranking from feature lists.

**Acceptance and validation:** publish machine-readable results and representative
artifacts, including failures and excluded cases. Separate synthetic UI receipts,
real native/model receipts, device receipts, and standards receipts. No overall
“passed” label may hide an unrun category. Set performance budgets from an actual
baseline and target hardware, not invented numbers. A regression threshold must
protect both average quality and critical cases such as amounts and identifiers.

The existing software entry points are:

```sh
npm run check
npx vitest run src/lib/ocr
node tests/ocr-workspace/smoke.mjs
```

Use the browser harness instructions in
[`tests/ocr-workspace/README.md`](../tests/ocr-workspace/README.md) and record setup
failures as setup failures. Native tests must use the paired features/dependencies
from [`Cargo.toml`](../src-tauri/Cargo.toml); record the exact commands/build in
each receipt. The commands above do not download/qualify OCR models, prove a
scanner works, or validate PDF/A/PDF/UA. Those are additional package-specific
runs, not implied by frontend test success.

## External workflow references

The following official documents were consulted on **2026-11-09** to scope user
workflows. They describe the named version/edition, not universal guarantees,
and do not substitute for testing CrispSorter. ABBYY FineReader PDF and Tungsten
OmniPage are distinct products. OmniOCR is another product and should not be used
as an alternate name for OmniPage.

- [ABBYY FineReader 16: checking recognized text][abbyy-proofing] — word review,
  suggestions, source inspection, and user dictionaries.
- [ABBYY FineReader 16: DOC(X)/RTF/ODT settings][abbyy-docx] — editable versus
  source-oriented output and document-structure choices.
- [ABBYY FineReader 16: editing tables][abbyy-tables] — cell and row repair.
- [ABBYY FineReader 16: non-standard fonts and training][abbyy-training] — a
  bounded, engine-specific training workflow.
- [ABBYY FineReader 16: PDF settings][abbyy-pdf] — image compression, output,
  and tagging choices.
- [ABBYY FineReader PDF 16 specification][abbyy-spec] — declared scanner,
  language, and document-format capabilities.
- [Tungsten OmniPage Ultimate][omni-product] and
  [OmniPage 19.2 watched folders][omni-watch] — acquisition, output, and
  unattended conversion workflows. Watched folders are edition-specific.

[abbyy-proofing]: https://help.abbyy.com/en-us/finereader/16/user_guide/checkingtext/
[abbyy-docx]: https://help.abbyy.com/en-us/finereader/16/user_guide/docx_settings/
[abbyy-tables]: https://help.abbyy.com/en-us/finereader/16/user_guide/edittable/
[abbyy-training]: https://help.abbyy.com/en-us/finereader/16/user_guide/ornatetype/
[abbyy-pdf]: https://help.abbyy.com/en-us/finereader/16/user_guide/pdf_settings/
[abbyy-spec]: https://support.abbyy.com/hc/en-us/articles/9904949915795-FineReader-PDF-16-specification
[omni-product]: https://www.tungstenautomation.com/products/omnipage/ultimate
[omni-watch]: https://docshield.tungstenautomation.com/OPU/en_US/19.2.0-x8jtnwx31a/help/OPU/Watched_folders.htm
