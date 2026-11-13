# Linux package verification and licensed OCR qualification

The Linux draft package control runs independently of Qwen Research License
consent. It executes the final DEB outside the build tree, checks version/doctor
with the dynamic-loader cache inhibited, exercises the OCR-worker protocol, and
recognizes the committed scanned PDF through DBNet/TrOCR. On a clean runner the
packaged CLI explicitly provisions only `dbnet-det` (Apache-2.0) and
`trocr-printed` (MIT). Its model plan must declare exactly those models and no
consent requirement before any download. The normal application licensing and
hash-verification paths remain active; a changed or restrictive model plan fails.

Only after these package controls pass does the release recipe write/upload its
source/version/package-hash receipt and checksum. The Linux receipt separately
states that licensed native presets were **not run** and remain **unqualified**.
Draft package success cannot be interpreted as Qwen consent, successful Qwen
inference, corpus accuracy, or permission to publish.

Tag builds and manual builds with `publish=true` still run the unchanged ignored
`ocr_task_presets_live` test after the package receipt is retained. That test
requires the app's existing Qwen consent gate and verifies quick, multilingual,
tables and difficult presets. No workflow step sets consent or adds an
`--accept-license` argument. Without consent the test fails and publication stays
blocked, while the earlier truthful package receipt survives. Draft-only builds
do not attempt that licensed test, even if a runner happens to hold consent.

A successful native preset report is sealed separately as
`CrispSorter_VERSION_linux-x86_64.ocr.json`. The sealer checks every required
preset and model identity, verifies the actual final DEB bytes, and binds the
report to the exact application source, version, package hash and package-receipt
hash. This is synthetic **native app-binding** qualification, not a claim that
Qwen inference ran through the packaged executable. Publication independently
requires this complete report and rechecks its binding to the downloaded Linux
receipt and DEB. Missing, skipped, stale or mismatched qualification fails closed;
the existing macOS/Windows signing and mobile publication checks remain required.

These controls change future release preparation. They do not grant consent,
rebuild or relabel an existing draft artifact, reconstruct an earlier job's
missing receipt, or prove its unretained post-build dependency-lock bytes.
