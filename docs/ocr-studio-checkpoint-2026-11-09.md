# OCR studio checkpoint — 9 November 2026

This is the first recovery checkpoint, retained as historical evidence. See the
[continuation record](ocr-studio-continuation-2026-11-09.md) for later implementation
and executed checks. The remaining-work section below describes this checkpoint,
not the current feature branch.

The implementation is saved on **feat/ocr-document-studio** in CrispSorter and
**feat/ocr-photo-preprocessing** in CrispEmbed. The reviewable changes are
[draft PR #29](https://github.com/CrispStrobe/CrispSorter/pull/29) and
[draft PR #189](https://github.com/CrispStrobe/CrispEmbed/pull/189).

Both paired Sorter build lanes use CrispEmbed commit
**d49a8967f94e41061492a825c6c2b88a68228c75**. This checkpoint changes the metadata
contract tests, workflow failure propagation, and retained evidence; its native
production sources and bindings are unchanged from the tested fde19180 revision.
The normal release runtime lock remains separate.

## Delivered workflow

The document workspace includes multi-document page management, region and
reading-order editing, confidence review, extraction editors, approval-aware
exports, bounded undo, and durable selected-batch continuation. The phone layout
has explicit page-list, image, and text/tools views. Camera import preserves
original bytes, applies orientation, and supports manual corner correction,
measured/manual tilt, shadow flattening, and original comparison.

The paired mobile profile adds private model installation through Rust HTTPS,
artifact/companion SHA-256 checks, atomic version activation, progress, and
cached-only recognition. Android's installer uses a pinned public TLS root
bundle with certificate and hostname verification. The iOS build handoff retains
the exact native library path through Xcode's nested Cargo call. Android build
flags retain both Tauri's baseline libraries and 16 KiB alignment.

See [the workspace guide](ocr-workspace.md) for user flows and
[mobile qualification](mobile-qualification.md) for exact build commands,
capability limits, and device work.

## Executed verification

| Area | Evidence | Scope |
| --- | --- | --- |
| Full frontend | 207 tests in 28 files; zero Svelte/TypeScript errors; production build passed | Source 81bc2c4d; 54 existing warnings in eight other files |
| Build/fixture helpers | 21 Python tests; workflow YAML and Node/Bash syntax; strict C++17 probe syntax | Helper source d3e90340; syntax checks do not establish mobile execution |
| Browser workspace | [20 scenarios passed](https://github.com/CrispStrobe/CrispSorter/actions/runs/42584160924); five screenshots inspected | Source 292742a5; synthetic Tauri adapter; latest 23 scenarios remain pending |
| Mobile providers | [10 Android and eight iOS tests passed](https://github.com/CrispStrobe/CrispSorter/actions/runs/42586987764) | Source de40879f; provider/export contracts, not physical system-picker or camera interaction |
| Native photo geometry | [Native controls, ASan/UBSan, three Rust wrapper tests, five OCR cases passed](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42590335637) | fde19180-equivalent tree; includes 18 blank-image controls across widths 3–160 |
| Native prebuilt policy | [Six Rust policy and four build-script configuration controls passed](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42590335633) | Configuration contracts, not real mobile cross-link or inference |

Frontend files are unchanged by the later native, TLS, workflow, and evidence
checkpoints. The current local Chromium environment rejects process socket
creation before interactions run. Its setup failure is retained and is not
counted as a successful browser test. The Rust compiler/toolchain was unavailable
in the recovered workspace; the new installer Rust test remains a hosted gate.

### Phone-photo text results

The latest complete [five-case report](https://github.com/CrispStrobe/CrispEmbed/blob/d49a8967f94e41061492a825c6c2b88a68228c75/tests/results/ocr-phone-photo/quality-fde19180.json)
uses the public photo API and independent Tesseract 5.3.4. Character error rate
(CER) is measured against known synthetic text:

| Synthetic input | Raw CER | Prepared public-API CER |
| --- | ---: | ---: |
| Aligned page | 0% | 0% |
| 4.2° clockwise | 2.6971% | 0% |
| 6.7° counterclockwise with shadow | 2.0747% | 0% |
| 10° rotation with shadow | 12.4481% | 0% |
| Perspective with shadow | 14.5228% | 1.2448% |

The shadowed rotation results include background whitening. Deskew alone produced
66.3900% and 77.5934% CER in those two cases. The perspective case's internal
projective and legacy arms measured 0.4149% and 0.8299%, respectively; neither
substitutes for the public API result. All positive and negative arms are retained.
This panel does not qualify native recognition models, physical cameras, or a
representative document corpus.

### Corrected metadata gate

The original [metadata run](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42590335645)
reported success while its ordinary native and sanitizer tests actually aborted.
The test incorrectly required every registry entry to have a downloadable HTTPS
URL, including five intentional local-only entries. Log capture through tee
masked the process failures. Its four Rust wrapper tests did pass.

Commit d49a8967 adds an explicit local-only metadata control and reliable pipeline
failure propagation, without inventing URLs or hashes or changing the production
API. The [original failed evidence and audit](https://github.com/CrispStrobe/CrispEmbed/blob/d49a8967f94e41061492a825c6c2b88a68228c75/tests/results/ocr-phone-photo/hosted-contract-review-fde19180.json)
remain available. The [corrected metadata run](https://github.com/CrispStrobe/CrispEmbed/actions/runs/42596751398)
was queued when this checkpoint was written; no native metadata pass is claimed.

## Remaining qualification

The complete source-pinned Sorter Rust checks and Android/iOS app builds still
need successful hosted results. The defined mobile workflow builds Android
ARM64/x86_64 and iOS ARM64 simulator apps, runs native synthetic inference, and
drives real Android commands for model installation, import, preparation,
recognition, saved settings, and reviewed-PDF staging. Those definitions are
separate from execution evidence.

Physical camera/picker interactions, low-memory and thermal behavior, larger
documents, multilingual and complex layouts, and real-camera corpora remain
qualification work. The mobile profile currently imports images/TIFF and exports
reviewed PDFs; it does not include PDF import. HEIC/HEIF/AVIF require conversion.
Reviewed PDF export does not advertise PDF/A conformance or unsupported
right-to-left text mapping. Old mobile model versions are retained.

The [machine-readable receipt](../tests/results/ocr-studio/verification-2026-11-09.json)
records exact source revisions, artifact IDs and digests, failures, and scope.
For continuation, fetch both feature branches and inspect any local changes
before switching revisions; preserve interrupted work rather than replacing it
with an older checkpoint.
