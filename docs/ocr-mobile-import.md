# Mobile document import

The OCR workspace uses Android `PdfRenderer` and iOS Core Graphics for mobile
PDFs; the mobile path does not require PDFium. Phone image conversion uses
Android `ImageDecoder` or iOS Image I/O.
Codec discovery controls the formats offered by the workspace: a platform that
does not advertise HEIC, HEIF, or AVIF decoding does not advertise that format
as an import option. The native layer still validates each actual source.
HEIC/HEIF/AVIF do not require prior conversion on a device with the corresponding
decoder. A JPEG/PNG copy is requested when native support is absent or a source
cannot be decoded. The workspace fails closed if the capability query fails.

## Preservation and limits

Camera/provider bytes are identified by their signature and archived under a
SHA-256 filename before decoding. The input filename cannot choose a decoder or
an output path. `source_uri` retains the exact provider identity;
`archive_uri` is the canonical absolute path of the private immutable original.
The project stores them as `sourceUri` and `archiveUri`. Existing projects without
an archive field remain compatible. Archive-aware duplicate checks do not rewrite
provider URI case, escaping, or separators.

| Boundary | Limit or behavior |
| --- | --- |
| Raw native import | Nonempty input, at most 32 MiB |
| Document-provider transfer | At most 16 MiB; unknown sizes are enforced while reading |
| PDF document | 1–10,000 pages; failure of any page rejects the entire document |
| PDF physical page | Positive dimensions, each no larger than 14,400 points |
| Native PDF/phone-container raster | At most 16 million pixels and 16,384 pixels along either axis |
| Native phone source image | At most 100 million encoded pixels before bounded decoding |
| Native rendered document | At most 512 MiB of PNG output |
| Concurrency | One OCR import at a time in the app |
| Protected PDF | Import an unlocked copy; no blank-success fallback |
| Multi-image phone container | Primary still image only, with a visible import warning |

PDF rendering targets 200 DPI. When the raster cap requires downsampling,
physical paper dimensions remain independent of pixel dimensions and a warning
asks the user to inspect small text. Phone image orientation is baked into the
working pixels. Native rasters use sRGB and a white background; originals retain
their encoded bytes, metadata, and orientation. Auxiliary images, depth planes,
and animation frames are not turned into separate OCR pages.
The 16-million-pixel and 512-MiB renderer limits apply to PDF/HEIC/HEIF/AVIF
conversion. JPEG, PNG, TIFF, BMP, and WebP use the shared image importer and its
100-million-pixel workspace page ceiling; provider/camera byte limits still apply.

## Native boundary

Tauri's Android bridge uses JSON postMessage and cannot deliver a raw request
body. Workspace imports and general image OCR therefore send `{ base64 }` on
Android, or while the native platform capability is still unknown. Known desktop
and iOS platforms retain raw byte transport. Metadata stays in the UTF-8 base64
`x-ocr-metadata` header, including the exact name, provider URI and request identity.
The native decoder checks encoded length and the padding-derived decoded length
before reserving memory; decoded input remains limited to 32 MiB.

A strict JSON byte-array compatibility path accepts at most 1 MiB and rejects
noninteger or out-of-range elements. Tauri has already parsed such arrays before
this check, so that compatibility limit cannot prevent the initial JSON parse
allocation. New Android callers and the real-app probe use base64 to avoid the
large number-array expansion. Full 32 MiB device memory behavior still requires
a rebuilt Android app and targeted device qualification; decoder tests alone do
not establish that qualification.

Only the Rust app can call `rasterize_ocr_document`; it is absent from the
webview command and ACL list. The registered mobile plugin independently checks
that the source is a hashed file under the private OCR `sources` directory and
the output is an empty `import-staging/import-*` directory in the same workspace.
Canonical-path checks reject symlink escapes and reused staging directories.

Native code writes one complete PNG at a time, then releases that page's raster.
It writes progress atomically and checks a cancellation marker between pages and
while encoding. Rust waits for the native worker to finish even after a cancel,
so it never removes staging under a running renderer. It then verifies every
output path, PNG signature, full image decode, pixel count, physical dimensions,
and total byte count before returning an imported document. Verified PNGs are
streamed into content-addressed assets without retaining a decoded document in
memory. Existing assets and originals are checked against their content hashes
before reuse. Normal completion, failure, and cancellation release temporary
staging; immutable original archives are retained. The separate **Manage model
storage** controls clean or remove eligible downloads in `ocr-models`; they do
not delete source archives, working page assets, projects, or reviewed text.

## UI and command contracts

- `ocr_workspace_capabilities` reports `native_document_import` on mobile.
- `plugin:mobile-provider|document_capabilities` reports native PDF and actual
  image decoder support. If discovery fails, the workspace disables those
  capabilities and gives a retry/import-images explanation.
- `ocr_workspace_import_bytes` and `ocr_workspace_import_image` keep their raw
  `Uint8Array` payloads. Their base64 UTF-8 `x-ocr-metadata` header accepts
  `{name, sourceUri?, requestId?}`; camera imports do not accept a provider identity.
- `ocr_workspace_import` accepts a local location plus optional `requestId`.
- Progress events named `ocr-workspace-import` include `request_id`, `phase`,
  `completed`, and `total`. Events are correlated before they update the UI.
- `ocr_workspace_cancel_import({requestId})` also handles a cancel arriving just
  before import registration. Early requests are bounded and expire.

The Cancel control remains available while document actions are locked. It
reports that stopping waits for the current step. Completed documents are
saved individually; cancelling a later document does not remove earlier work.
If a project checkpoint fails, importing stops before the next document. The
pages remain in the pending project for **Save**/**Retry**, and the workspace
reports that the save failed instead of claiming that those pages are durable.

## Qualification

`tests/fixtures/mobile-documents/manifest.json` pins byte counts and SHA-256
digests for self-authored PDF, rotated PDF, password-protected PDF, EXIF-oriented
JPEG, HEIC, and AVIF fixtures. Binary fixtures use base64 text for reviewable transport.
The copies in the Swift test bundle must match the canonical fixture directory.
The HEIC fixture contains a single HEVC Main image generated by the offline
`tests/scripts/generate_mobile_heic_fixture.py` utility. An independent libheif
decode verifies its dimensions and color bounds before it is pinned. Native
tests decode these same immutable bytes on both platforms when HEIC is advertised.

Android instrumentation and iOS simulator tests exercise actual platform
decoders: page count, nonblank raster output, physical dimensions, rotation,
oriented image pixels, source-byte preservation, bounded sizes, unsafe paths,
locked/damaged PDFs, and cancellation before and between pages. AVIF tests skip
when the runtime does not advertise the decoder. The iOS HEIC round-trip test
requires both an encoder and a decoder and records a skip when either is absent.
These tests do not establish physical-camera or physical-device qualification.
Source-specific outcomes, failures, corrections, and artifact digests are kept in
`tests/results/ocr-mobile-import/qualification.json`.

The [corrected provider run](https://github.com/CrispStrobe/CrispSorter/actions/runs/42609744777)
at `dd8d9b1419e5b5d339c8a432118cb94218cdbce3` passed 18 Android 10/API 29 emulator
tests with one AVIF skip, and all 18 iOS simulator tests using Xcode 16.4 and the
iPhoneSimulator 18.5 SDK. The iOS HEIC, AVIF, EXIF, and incomplete-image tests
executed and passed. The preceding iOS run had exposed tolerated JPEG truncation
and a sparse PDF pixel-sampling assertion; the corrected run adds structural
image checks and counts all dark pixels. The [qualification receipt](../tests/results/ocr-mobile-import/qualification.json)
preserves both source revisions, test-level results, failures, and artifact
digests. These outcomes cover the registered provider's framework decoders and
access controls; the complete application-command path is a separate gate.

The subsequent [shared-fixture run](https://github.com/CrispStrobe/CrispSorter/actions/runs/42612700801)
at `76de6dad49f9474aa1034e3030f95cd3f4a6d144` passed 19 of 20 Android tests
(the same unsupported-AVIF skip) and all 19 iOS tests. Both native decoders
accepted the identical, independently decoded HEIC fixture, produced its expected
120 × 80 dimensions and corner colors, and preserved the original bytes. Both
downloaded test artifacts were checked against their SHA-256 digests; the receipt
retains their exact test lists and source identity. The provider's JPEG-helper
checks alone do not establish completeness checking in the application's
separate shared Rust JPEG import path.

Rust tests cover bounded ISO BMFF signature parsing, private archive identity,
native result containment, corrupt/truncated rasters, hash mismatches, and pinned
PDF source integrity. Client tests cover format gating, metadata transport,
progress correlation, cancellation races, failure preservation, and archive
identity after save/restore. The browser `import-control` scenario holds native
responses to verify phone Cancel reachability, stop-state locking, and retention
of completed documents; this scenario uses the synthetic adapter and does not
replace native decoder tests.
