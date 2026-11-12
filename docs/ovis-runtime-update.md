# Ovis native failure propagation: consumer update gates

The current immutable runtime remains CrispEmbed **v0.17.19**, source
`c364feb974b91e14cbf328c6949f3df36e89ce81`. It can turn a native Ovis image
admission failure into successful empty OCR. This document prepares a consumer
update; it does not change that runtime, qualify a release, or fix admission of
large images.

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
or review a replacement as a new corpus revision. Do not silently replace the
expected hash. The download receipts establish a reproducible mismatch, not its
cause.

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
default change. No resize, tiling, larger patch bound or quality claim is part of
this update plan.

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
Desktop candidate builds remain pending. These are candidate libraries, not
finished application binaries, runtime error-path tests, or physical-device
qualification. Artifacts expire after 14 days; keep
verified copies outside Git until a reviewed release packages them.
