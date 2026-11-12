# Ovis native failure propagation: consumer update gates

The current immutable runtime remains CrispEmbed **v0.17.19**, source
`c364feb974b91e14cbf328c6949f3df36e89ce81`. It can turn a native Ovis image
admission failure into successful empty OCR. This document tracks a candidate
consumer update; the production runtime remains unchanged and no release or
real-page OCR quality claim has been qualified.

## Reuse the merged runtime fix

[CrispEmbed commit 497301606f2913c266f39ba8157f2d4769bb27e5](https://github.com/CrispStrobe/CrispEmbed/commit/497301606f2913c266f39ba8157f2d4769bb27e5)
preserves reported failures through the native orchestrator and C API. It also
updates the Rust wrapper used by this app. A failure has NULL `full_text`; a
positive blank recognition has a non-NULL empty string. The Rust wrapper returns
`Err` for the former and preserves `Ok` for the latter. It checks the existing
sentinel without requiring the new diagnostic symbols to link.

The combined runtime merged through
[PR 227](https://github.com/CrispStrobe/CrispEmbed/pull/227), merge commit
`2d2ab3275022e095f4a8f580e276bb2e3aa3f59c`. HTTP failure propagation is separately
included at `9d2c876d0bf694c95174cb9e87950c3362399ed6`. These source references
are preparation inputs, not published runtime assets. The observed main tip at
review was `54567c9b8b553097282359f5c7abc38e36624276`.

The app already propagates `pipeline.run(...)` errors in plain text, region,
structured-render and review extraction. A text-length check cannot recover the
failure erased by the old native library, and would incorrectly reject valid
blank pages. Reuse the upstream implementation rather than introducing that
heuristic. Rebuild both the Rust wrapper and the native runtime: replacing only
the native library leaves an already compiled old wrapper's NULL-to-empty
conversion intact. A new wrapper linked against v0.17.19 remains link compatible
but retains that native runtime's false-success behavior.

## Publish and verify immutable artifacts before repinning

1. Select and seal an exact release source containing the combined fix, including
   the release version synchronization changes. Record its ggml commit and source
   tree. Follow the upstream release procedure; do not equate an earlier prototype
   receipt with final-source qualification.
2. Build and qualify the platform archives below on hosted workers. Retain source,
   compiler, architecture, backend, dependency, archive byte count and SHA-256
   provenance. Inspect the actual packaged libraries and their dependency closure;
   run the packaged assets rather than only build-tree libraries.
3. Publish the qualified runtime under an immutable version. Populate
   [the runtime lock](../scripts/crispembed-runtime-lock.json) from those exact
   archives. Keep source checkout pins, workflow version/commit values and
   verification helpers consistent. Never combine old archive hashes with a new
   source label.
4. Rebuild and exercise the app against those locked libraries and matching Rust
   sources, retaining exact app/runtime/model/input/scorer provenance. Only then
   change the release runtime lock and report the consumer fixed. Update
   [the independent mobile source lock](../scripts/ocr-mobile-runtime-lock.json)
   only after its paired mobile build and device gates pass.

| Locked archive target | Required consumer qualification |
| --- | --- |
| Linux x86_64 CPU | Packaged app CLI and Rust extraction; dynamic dependencies and baseline CPU compatibility |
| Linux arm64 CPU | Native arm64 load/link and the same extraction contract |
| macOS arm64 | Packaged dylib load, app extraction and the actually shipped backend |
| Windows x86_64 | Packaged DLL dependency closure and app extraction on the supported CPU baseline |
| Android arm64-v8a | JNI/Rust packaged library load and device OCR/import/review/export error reporting |
| Android armeabi-v7a | 32-bit build/load plus the same device contract; 64-bit receipts do not qualify it |
| iOS arm64 | Packaged native link/load and signed device OCR/import/review/export error reporting |

CUDA, Vulkan and Metal app variants require separate packaged-backend controls
when shipped. For strict CUDA, record the actual GPU, graph placement and absence
of CPU fallback on a successful decoded control. Allocation or CUDA initialization
alone cannot establish that an oversized page executed inference. Do not infer
Metal, Vulkan, other GPU architecture or mobile parity from the retained T4 result.
Keep unsupported/unrun matrix entries explicitly unqualified.

## Boundary tests for the rebuilt consumer

Execute the real app/Rust wrapper/native boundary, using small bounded fixtures.
Upstream already has native selection and binding tests; retain them rather than
creating a second fallback implementation in the app.

- A reported native failure returns an app error and a nonzero CLI status. It must
  not create successful empty text, a successful review record or a successful
  structured export. Error logs should retain a safe failure code without input
  text or private paths.
- A positively recognized blank page remains successful. Exercise non-NULL empty
  native text rather than substituting an empty Rust string after the boundary.
- Run failure then success on the same cached pipeline. Stale text, regions,
  language or review state must not appear after failure; recovery must work.
- Exercise failed-first then successful-stage fallback, and known blank followed
  by failure. Preserve the successful result's engine/stage/geometry provenance.
  An unknown legacy empty stage must not hide a preceding reported failure;
  standalone permissive legacy blank behavior remains covered upstream.
- Verify explicit app engine selection still reports failure without silently
  loading another engine. Native configured stage fallback and app engine
  selection are distinct contracts.
- Replay the frozen small decoded control and rejected real-image control with
  the unchanged model, token cap and preprocessing settings. Exact positive
  output plus an accurately reported rejection establishes this error fix; it
  does not establish OCR quality for previously rejected pages.

Model-free source or mock controls cannot qualify the old runtime as fixed. The
existing bounded CPU and T4 controls support the upstream change, but a final
immutable release and rebuilt consumer still need the gates above. This
preparation adds no duplicate app test that merely asserts Rust `Result`
propagation.

## Preserve the negative benchmark

The original source-pinned panel requested eight pages. One image failed its
pinned SHA check identically on CPU and GPU and was skipped in every arm. Retain
that omission and the seven admitted pages; restore the originally pinned bytes
or review a replacement as a new corpus revision. Two later independent read-only
fetches recovered bytes matching the original pinned hash; stage and verify those
exact bytes before any corrected run. Do not silently replace the expected hash.
The earlier download receipts establish a reproducible mismatch, not its cause.

All 21 Ovis records reported empty success despite producing no text. Preserve
their CER/WER of 1 and word recall of 0, and retain the baseline failure penalties.
Their roughly three-second page latencies include rejection/startup and do not
measure useful inference throughput. A corrected run must preserve failures in
aggregate denominators and report both process status and semantic outcome. Keep
the original reports; never relabel them as successful recognition or remove
them when newer evidence arrives.

## Separate image admission work

The retained direct diagnostic rejects a 1280-by-2077 image before vision with
`image exceeds native patch bound`, zero selected tokens and error stop 4. A
448-by-448 control decodes identically on CPU and GPU. Error propagation exposes
this rejection; it does not resize the input or extend the native bound.

A future preparation change must derive checked patch counts from the actual
native preprocessing rules, use overflow-safe dimension arithmetic, and enforce
explicit memory/pixel limits. Compare aspect-preserving downscaling and bounded
overlapping tiles with a ground-truth panel including small text, tables,
receipts and multiple columns. Tiling needs verified reading order, overlap
deduplication and coordinates mapped back into the prepared-image space before
review or searchable export. Retain original images and record the applied
transform. Measure accuracy and end-to-end latency on admitted images before any
default change. The opt-in resize candidate described below remains unqualified;
no tiling, larger patch bound or quality claim is part of this update.

## Mobile candidate build evidence

The [candidate mobile build/link run](https://github.com/CrispStrobe/CrispSorter/actions/runs/43299077945)
passed Android arm64, iOS arm64 and iOS simulator arm64. Its workflow did not
retain the built libraries, so those results establish build/link compatibility
only and cannot supply runtime assets. Subsequent qualification runs retain the
candidate libraries, native notices and source-bound build receipts for 14 days.
They still do not establish application behavior or physical-device OCR.

The retained candidate builds for [Android arm64](https://github.com/CrispStrobe/CrispSorter/actions/runs/43302210129)
and [Android armv7](https://github.com/CrispStrobe/CrispSorter/actions/runs/43302786120)
have passed build/link. Their retained libraries, source receipts and notice
hashes were independently verified; the armv7 link probe also reports the ARM
architecture. The iOS arm64 and simulator arm64 candidate build/link jobs also passed; their
libraries, source receipts and notice hashes were independently verified.
These are candidate libraries, not finished application binaries, application
runtime error-path tests, or physical-device qualification. Artifacts expire after
14 days; keep verified copies outside Git until a reviewed release packages them.

The [unpublished desktop candidate run](https://github.com/CrispStrobe/CrispEmbed/actions/runs/43302121233)
passed the four CPU archive builds: Linux x86_64 and arm64, Windows x86_64,
and macOS arm64. Its manifest identifies the same runtime source as the mobile
candidate and separately identifies the workflow revision. The four retained
archives were independently checked against the manifest's archive hashes and
sizes. GPU variants were intentionally skipped; archive builds and hashes do
not establish app integration, runtime error handling, or OCR quality.

An explicit resize budget of `1024 * 256 = 262144` pixels, using the existing
square-root budget calculation and floor-to-32 dimensions, proposes **384 by
640**, or **960 patches**. This retains only **9.24%** of the original pixels;
admission is not evidence that small text remains readable. The final dimensions
need rechecking through the native rounding and minimum-pixel rules. Do not
change the model metadata or globally lower its declared maximum pixels to
introduce an app preparation option.

The existing default-off 2048-patch native experiment is also insufficient for
10,400 patches, and is explicitly unqualified: six composed arms in its retained
boundary audit fail numerical checks. Enabling that gate is not a substitute for
a checked resize candidate.

### Why a scalar resize insertion is insufficient

The existing app contract is **prepared-image pixels**. `prepare_ocr_page` may
normalize orientation, dewarp, restore, upscale, crop or deskew before OCR. Its
`PreparedOcrPage` retains a `cleanup_scale` dimension ratio, not an invertible
transform history. In particular, that ratio cannot invert translation, rotation
or nonlinear dewarping into archived original pixels. `ocr_regions_for_review`
deliberately disables repeated native cleanup and rejects a native snapshot that
declares invalid input coordinates. Structured export uses the prepared image as
its searchable image layer. Workspace zone/layout recognition adds separate crop
translations. Review regions, optional word evidence and raw native snapshots
also have distinct geometry/provenance fields.

Consequently, putting resize into shared preparation would change the displayed
and searchable image layer; scaling only the outer region boxes would leave
other geometry and provenance inconsistent. A review-only implementation would
leave plain text, detailed regions, worker and layout paths with different input
policies. No behavior change is made by this inspection.

A safe first implementation needs a shared recognition-input guard below those
entry points. It must retain the original/prepared image for display and export,
own a separate temporary recognition raster, and carry named coordinate spaces
plus exact input/output dimensions and independent x/y scales. If the requirement
is archived original pixels, first reject every nonidentity existing preparation
transform until a complete inverse chain is available. Native snapshots should
remain immutable in a clearly named recognition-input coordinate space, with an
explicit transform to app geometry; never relabel unmodified snapshots as
original-image geometry.

Restrict the first candidate to a verified Ovis2 profile with a single stage,
router/layout/zone/cleanup/repair disabled and explicit opt-in. A path substring
or the generic `unified` engine name does not prove a model's metadata profile.
Reject unknown profiles, Ovis3, mixed-stage fallback, unsupported transform
combinations and unavailable geometry provenance. Refuse invalid/zero dimensions,
overflow, excessive source dimensions/pixels/aspect ratio, a target outside the
patch/minimum-pixel bounds, nonfinite or out-of-bounds returned geometry, and any
unmapped nested word geometry. Report effective dimensions/scales and the
experimental quality warning in diagnostics. Defaults and original assets remain
unchanged.

Before wiring that guard, focused model-free tests should cover ties-to-even at
16/48 modulo 64, 1024-patch equality and one-step-over refusal, minimum-pixel
upscaling after target calculation, extreme aspect ratios, dimension overflow,
identity/no-op input, unequal x/y scale factors, full-page and fractional boxes,
word boxes and invalid native coordinates. Integration controls must additionally
prove image-layer preservation, native snapshot coordinate provenance, worker
serialization, crop translations, and refusal of transforms without inverses.
Those contracts cannot be established by testing a target-size calculator alone.

### Prepared-image candidate handoff

The subsequent candidate preserves the app's established **prepared-image**
contract, rather than attempting archived-original coordinates. Set
`CRISPSORTER_OVIS_RESIZE_INPUT=1` explicitly to opt in; unset, empty, `0`, `false`,
`off` and `no` leave it disabled. This is an experimental execution option, not a
quality recommendation or a new default. It does not enable the unqualified
native 2048-patch path.

`run_prepared_pipeline` is the shared execution boundary for document text,
detailed regions, structured-render regions and review; isolated workers call
those same entry points. It permits only a single `unified` stage selecting the
known `ovisocr2-f16` registry alias, without source routing or native cleanup.
Custom paths, other unified model aliases, multiple-stage fallback and unavailable
native geometry diagnostics are refused. Other engine families retain their
existing input behavior. The existing model manager and native profile checks
remain authoritative; this preparation code does not qualify arbitrary GGUFs.

For oversized admitted source dimensions, the guard owns a separate temporary
PNG. It verifies both decoded source dimensions and the actual written PNG's
dimensions against the checked plan. Named temporary files retain private file
permissions and their guards remove them on success and every returned error.
The source/prepared path is never written, substituted as the display asset or
used as the inference copy's storage location.

Native snapshots must explicitly assert valid input coordinates. Every returned
region box must have finite, nonnegative coordinates and lie within the inference
image before independent x/y inverse scaling. The app's returned region boxes
are mapped to the input prepared image; workspace crop/zone callers can then
apply their existing local-to-page translations. Native snapshots remain raw and
are explicitly labeled `inference_input_pixels` in the added diagnostics, with
the transform dimensions and scales. Unknown nested native word evidence is
refused rather than silently relabeled. No word geometry is synthesized.

Review diagnostics and operation events identify the explicit opt-in, whether a
resize occurred, effective prepared/inference dimensions, scale factors and the
returned prepared-image coordinate space. Review adds an unqualified-quality
warning. No-op opted-in inputs also require valid native diagnostics and geometry;
absence of resize does not waive those provenance checks.

Four dependency-free arithmetic tests passed locally before the shared-host load
restriction: pinned rounding/patch edges and native minimum resizing, real-page
dimension planning and unequal-axis geometry, invalid dimension/coordinate
refusals, and explicit gate spellings. The subsequent PNG and shared-boundary
wiring has received source review only. Full app feature compilation,
native-schema compatibility, temporary-file lifecycle, crop/zone integration and
packaged consumer execution remain **unverified**; no inference or hosted job was
launched. Keep this candidate out of release/default claims until those checks
run against compatible immutable assets. The old locked runtime's native-error
status loss remains a separate unresolved shipping dependency.


### Source-bound execution contract follow-up

[The narrow contract](../tests/ovis-input-contract/README.md) now extracts and
compiles the actual shared execution helper and all four changed conversion paths
against production app/config/document/review/word/renderer types and the exact
locked CrispEmbed wrapper result types and method signatures. Only native
execution/cache and log sinks are fixtures; no native model is loaded.

Both diagnostics feature states pass **6/6 tests** with one compilation job and
serial tests. The diagnostics-off run verifies default conversions do not request
snapshots and opted-in resize refuses before native execution. The diagnostics-on
run exercises real PNG decoding/resizing/writing, inverse region mapping across
all four paths, no-op inputs, raw snapshot coordinate retention, explicit opt-in
metadata, unchanged source bytes, private temporary permissions and cleanup after
success or each tested refusal/native failure. Unsupported recipes, missing/bad
snapshots, nested word evidence and invalid boxes are refused. Five included
arithmetic controls cover rounding/admission, independent-axis mapping, bad
inputs, explicit gate spellings and tiny positive boxes that collapse in f32.

The follow-up fixes two review findings: non-review callers only capture a native
snapshot when the resize option is active, preserving the previous disabled
path's snapshot work, and language is queried only for consumers that use it;
mapped f32 boxes receive a second finite/positive-area/bounds check after endpoint arithmetic. Full app/package compilation, actual native
schema/error behavior, worker/crop integration and OCR quality remain unqualified.
This contract does not satisfy immutable runtime release or consumer gates.

The [packaged desktop consumer run](https://github.com/CrispStrobe/CrispEmbed/actions/runs/43309558037)
passed the model-free native and Rust failure/legacy-empty controls on Linux
x86_64, Linux arm64 and macOS arm64 using the verified candidate archives. The
Windows job failed before loading the archive because its checkout line endings
did not match the committed controller bytes; it is not a Windows runtime
result. A follow-up checkout and exact Windows packaged-header line-ending fix
is under review in [PR 230](https://github.com/CrispStrobe/CrispEmbed/pull/230);
the [Windows-only rerun](https://github.com/CrispStrobe/CrispEmbed/actions/runs/43313165931)
passed its native C controls but its Rust control failed before execution because
Cargo selected a non-MSVC `link.exe`. After the Windows-only linker correction,
[the targeted rerun](https://github.com/CrispStrobe/CrispEmbed/actions/runs/43316615562)
passed the packaged native and Rust model-free failure and legacy-empty controls.
The source/archive/header checks remained in place, and the result receipt was
retained. [PR 230](https://github.com/CrispStrobe/CrispEmbed/pull/230) is merged. None of these controls
establishes decoded OCR, a positively recognized blank page, CrispSorter
application behavior or GPU execution.

The [Android x86_64 candidate app run](https://github.com/CrispStrobe/CrispSorter/actions/runs/43313212722)
built the app and reached its emulator boundary check. The retained native log
shows the deliberate DBNet/unified mismatch being refused for missing
encoder/decoder tensors, followed by `unified OCR model load failed`. The actual
WebView rejection ends with `Recognize page: CrispEmbed OCR pipeline run failed:
OCR pipeline recognition failed; see native diagnostics`. The harness expected
the obsolete `OCR recognition failed` application context and rejected that
genuine native failure. The harness now requires the exact current execution
context plus the unchanged native wrapper sentinel; its regression follows the
actual Rust `pipeline.run` error context and replays the sanitized retained
rejection. Earlier model/config/constructor errors and successful empty replies
remain failures of the negative gate.

The [sanitized observation](../tests/results/ocr-mobile-candidate/run-43313212722-boundary.json)
binds the run, archive, APK, source commits, native receipt, raw rejection and
log hashes. The whole run remains failed: the blank control and later checks
did not execute. Its retained evidence establishes this deliberate native
initialization rejection crossing the rebuilt Rust wrapper and app IPC; it does
not establish valid blank behavior, Ovis inference, same-context recovery or
physical-device behavior. Local validator replay is not emulator execution.

The retained APK bytes match the build receipt's SHA-256, so correcting this
external harness does not require rebuilding the app. An emulator-only replay
must bind original application source `a26ea39776b5510f55a73f0a0fde3f54b0724b20`
and corrected harness source separately, retaining exact APK/native/lock checks.
The existing runner assumes its checkout is the app source and cannot directly
replay this artifact. The full quality/document fixture directory and installed
private baseline models were not retained; a replay needs exact fixture
reconstruction or a narrow boundary runner, and available verified baseline
model bytes in the fresh emulator. No replay workflow or execution is added here.
