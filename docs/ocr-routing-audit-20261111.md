# OCR routing and pipeline audit — 2026-11-11

## Result

OCR selection now travels through a shared policy instead of being determined
by which screen or extractor happened to receive the document. Application
image/PDF extraction uses the native OCR service; saved engine, language,
models, stages, script selection and CPU profile reach the same dispatcher used
by ingestion. Workbench projects retain explicit overrides.

The policy is implemented in [ocr_policy.rs](../src-tauri/src/extractors/ocr_policy.rs).
The frontend [settings serializer](../src/lib/extractors/ocrSettings.ts),
[dispatcher](../src/lib/extractors/ocrDispatcher.ts) and shared
[engine catalog](../src/lib/ocr/engineCatalog.ts) keep transport, settings and
capability presentation consistent. This is a routing/correctness update, not
a new accuracy or latency comparison across models.

## Findings and corrections

| Entry point | Previous gap | Shared behavior |
| --- | --- | --- |
| Frontend raster images | Always used Tesseract.js with fixed English/German languages. | Native runtime sends bounded binary input and a captured OCR policy to `ocr_extract_image`; results retain engine/warning metadata. |
| Frontend scanned PDF pages | Created recognizers per page, fixed languages, swallowed OCR errors, and missed cleanup on error paths. | One policy per document, shared dispatcher, reusable browser worker, bounded rasters and deterministic resource cleanup; OCR failures abort the extraction. |
| Native images and multipage TIFF | Explicit pipeline failures could fall through to another engine; partial pages could become successful text. | Selected engine failures propagate with page context; multipage decoding and OCR failures cannot silently truncate a document. |
| Native PDFs | Aggregate document text could hide scanned pages; failed OCR/rasterization could return inadequate text or attempt a PDF sidecar fallback. | Evaluate text by page, retain useful digital text in order, and recognize deficient pages; forced OCR remains explicit. |
| Archive/mail members | OCR configuration serialization diverged from Settings. | One serializer and immutable selection snapshot reaches native member extraction. |
| Background indexing | `tier4` was mapped to Automatic; settings were reconstructed at separate call sites. | One saved `ExtractOptions` snapshot includes all tiers and complete policy. |
| Foreground ingestion and promotion | Some commands used extraction defaults or disabled OCR inadvertently. | Snapshot saved OCR policy; explicit OCR actions enable the operation while retaining selected models and parameters. |
| Template text zones | Hardcoded system Tesseract and converted errors to empty strings. | Use the shared dispatcher and propagate errors. Checkbox OMR remains a separate operation and avoids unnecessary temporary PNGs. |
| Text-based KIE and batch KIE | Forced OCR discarded settings; batch errors could become empty fields and image extensions were omitted. | Use saved/request-specific policy, include supported images, and surface extraction failures. |
| Workbench | Engine catalog differed from Settings; configured stages and project/global defaults could conflict. | Shared capability catalog, explicit project precedence, active-stage controls and model/preparation selection. |
| CLI OCR, KIE, tables and zones | Partial engine enumeration and default-only helper calls. | Shared engine catalog, language/CPU options, and read-only saved-policy access for KIE/tables/zones. |
| Structured OCR export and specialist adapters | Direct calls could bypass selection or replace incompatible models. | Validate selected policy and output capability before recognition; no hidden engine substitution. |

## Selection rules

Explicit per-request or per-project choices take precedence over global saved
defaults. A configured stage retains its model paths, detector settings, prompt
and acceptance thresholds. Automatic language does not replace a custom model.
An enabled advanced pipeline keeps its chain; disabled retained stages do not
become active merely because a file is opened through another route.

Explicit engines take precedence over legacy tier choices. With the Smart
pipeline disabled, an explicit legacy tier remains a meaningful choice.
Unconfigured Automatic uses embedded Tesseract when available, otherwise the
available native legacy route. Unsupported choices and selected-model failures
produce errors rather than falling through to a different recognizer.

Settings and extraction share numeric conversion. Valid zero confidence,
character and detector thresholds survive serialization. Custom model paths,
including restoration, layout and language-selection models, are retained.

File/PDF extraction snapshots the complete policy before processing. Workbench
batches and specialist actions capture their effective project configuration.
CLI KIE, table and zone commands read saved settings without acquiring scheduler
ownership, recovering jobs, or changing in-flight background queue entries.

## Optimizations

- Browser-only OCR reuses one worker for a matching language set, serializes
  requests, and releases the worker after 30 seconds idle. Cancelling a queued
  document does not terminate another document's active recognition.
- Native application image data travels as a bounded binary payload rather than
  a base64 image. Temporary staged files are removed on recognition failure as
  well as success. Content determines the file format; the supplied display name
  is never treated as a filesystem path.
- PDF extraction avoids OCR on usable digital text. The frontend also avoids
  rasterizing short text-only and blank pages, caps raster dimensions, and stops
  at requested page/character limits.
- Layout and ocrs caches are keyed by their actual selected model configuration,
  preventing reuse of an earlier model after settings change.
- The Qwen2 VL CPU worker retains complete document text/postprocessing or rich
  review results according to the requested output, instead of losing those
  fields through a lossy region conversion.
- Template checkbox processing avoids OCR staging; text zones share one decoded
  source image, then crop and prepare each zone with the same selected policy.

These changes remove repeated setup and unnecessary work by construction.
No throughput, memory or accuracy improvement percentage is claimed without a
separate measurement.

## Capability boundaries

The shared catalog describes engines a particular build can execute. Native
models are not JavaScript browser models. In a browser-only runtime, Automatic
uses Tesseract.js; an explicit native engine, native tier, native pipeline or
CPU profile fails with a capability explanation. The current index server
accepts extracted text/embeddings; it is not an OCR model execution service.
Desktop remote-index workflows still extract on the local desktop.

Ad-hoc CLI OCR uses its command-line flags and defaults through the same native
policy. Formula recognition remains a distinct LaTeX-producing operation with
its own selected formula model. A preferred document converter can provide PDF
text before native extraction; explicit forced PDF OCR bypasses that preference.
The per-page text/OCR merge described above applies to the native PDF route.

Plain-text ocrs/Paddle adapters cannot promise verified word coordinates or
confidence. Workbench can review their output as an unscored page block;
structured export and coordinate-dependent legacy endpoints reject unsupported
combinations instead of manufacturing word evidence.

LiLT's pinned native binding performs DBNet+TrOCR recognition internally.
Native table structure extraction uses a Tesseract recognizer. Truly
unconfigured Automatic can select the compatible specialist default; explicit
incompatible engines, chains, models or unsupported processing options must
produce an error. Text-based KIE uses the general OCR dispatcher and supports
the wider engine catalog.

Workbench recognition applies supported preparation to a reviewed image
snapshot so returned coordinates refer to those pixels. Zone recognition
preserves the current page geometry. A multistage review uses the first active
stage's preparation for the shared image and reports that constraint. Legacy
region-only commands cannot return a changed image and reject transformations
they cannot represent accurately.

Experimental models retain their runtime enablement flags. Custom-model choices
retain their file requirements and licence checks. The isolated CPU profile
requires one Qwen2 VL stage and a supported desktop sidecar build; its pixel
budget is not a process-memory cap.

## Verification

The update adds executable unit regressions for serialization, selection
precedence, native transport, worker ownership/cancellation, PDF decisions and
cleanup, model/cache identity, zone dispatch, saved-policy persistence, CLI
parsing and specialist compatibility. Native tests do not require downloading
OCR models for their configuration and orchestration cases.

Final suite counts and hosted results are recorded in the pull request after
the integration checks. The historical six-page System Tesseract measurement
remains a separate result tied to its unchanged production adapter; no new
cross-engine measurement is claimed.
