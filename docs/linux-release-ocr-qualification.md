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

Tag builds and manual builds with `publish=true` still run the existing ignored
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
hash. The native producer records its checkout source, the hash of the actual
PNG used, and the package-receipt identity captured before inference. Sealing and
publication compare these raw identities with the expected source, committed PNG
bytes and exact package receipt; a raw report from another context cannot be
relabeled. The workflow removes old raw output before running the test. The
source statement relies on the trusted workflow executing Cargo against that
checkout, rather than providing a cryptographic compiled-binary attestation.
The test's licensing and inference assertions are unchanged.
This is synthetic **native app-binding** qualification, not a claim that
Qwen inference ran through the packaged executable. Publication independently
requires this complete report and rechecks its binding to the downloaded Linux
receipt and DEB. Missing, skipped or mismatched-source/fixture/package
qualification fails closed;
the existing macOS/Windows signing and mobile publication checks remain required.

These controls change future release preparation. They do not grant consent,
rebuild or relabel an existing draft artifact, reconstruct an earlier job's
missing receipt, or prove its unretained post-build dependency-lock bytes.

## Existing original-source package observations

`record_existing_linux_package.py` provides a separate read-only check of the
already retained version 0.15.1 DEB associated with original source `b739a3c0`.
It requires that exact unpublished draft/tag target and pinned asset digest,
checks the retained DEB bytes, and validates the existing startup, worker and
offline DBNet/TrOCR observations against their source, model, fixture and output
hashes. A committed evidence lock independently pins all eight reviewed sanitized
input/output files; editable hashes inside the reports are insufficient. Doctor
evidence must also contain the expected boolean fields and PDF support. Replacing
hOCR and its declared digest together, or supplying an empty doctor object,
cannot pass. It reads committed, clean verifier tooling and records its source/tree
separately from the original application source association.

After reviewing the retained observations, invoke the recorder without rebuilding
or rerunning inference:

```sh
python scripts/record_existing_linux_package.py --repo CrispStrobe/CrispSorter \
  --evidence-dir /path/to/retained-linux-evidence \
  --output /path/to/CrispSorter_0.15.1_linux-x86_64.observation.json
```

The only permitted output name ends in `.observation.json`. The distinct schema
explicitly keeps Qwen unqualified and cannot pass the release publication gate.
This recorder does not execute retained programs, download models, infer consent,
upload assets, or reconstruct the original job's missing release receipt. Its
source statement is the original build-log/draft-target association; it does not
attest to unretained post-build lock bytes or reconstruct the compiled dependency
graph. Changed draft assets or tampered/incomplete retained evidence fail closed.
The reviewed evidence lock anchors these local operator observations; it is not
a third-party build or execution attestation.

The user declined the Qwen test. Leave it unrun and do not set its license
acceptance; the Qwen-dependent publication qualification remains open unless the
user later changes that decision. The separate permissive-model package
observation does not satisfy that gate.
