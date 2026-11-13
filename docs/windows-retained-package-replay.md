# Original Windows ZIP validation replay

The original-source Windows retry uploaded an unsigned portable ZIP, then failed
the final packaged DBNet/TrOCR scanned-PDF command with exit 1. Version, doctor
and worker protocol controls had completed. The original verifier discarded the
captured stderr on failure, so the actual OCR rejection is unknown. Missing
models are plausible, not established by the old log.

The `windows_validate_b739` selector reuses exactly
`CrispSorter_0.15.1_x64-windows-portable-unsigned.zip`, 238,419,528 bytes, SHA-256
`07a0960f62fb95f4c84fcd9f52118d9f094736151cc550edc0f43babe21d4f95`.
Its source association is original app commit
`b739a3c0b70f39d3ae19e1bfcb412f3b44f77794`, with the separately retained original
build-workflow commit `3f28c0d10fd303a1a59290872d6a8c50a38431e6`. Reviewed original
preflight bytes, source input hashes and both source/tree identities are pinned
in committed tooling. Corrected replay tooling is checked out at, and must match,
the actual `github.workflow_sha`; that workflow source/tree identity is recorded
separately from both original app and original build-workflow identity.
This is original build/draft association; the failed original verification did
not produce a completed final release receipt or compiled-source attestation.
The replay never reconstructs one.

Only the new existing-workflow Windows job runs for this selector. Contradictory
publication/App Store/profile flags fail before original checkouts or package
access; the selector is explicitly excluded from the App Store job. The job
sets Git LF checkout policy before all three credential-free checkouts. It
installs Python, reads the draft and downloads the exact ZIP; it does not invoke
Cargo/npm, build runtimes, sign, delete/upload release assets or publish. No
concurrency block is added or edited. Job-local `contents: write` enables draft
visibility; actual GitHub operations remain list/view/download only.

The helper dynamically requires the unique unpublished `v0.15.1` draft and exact
original target/tag/asset digest before and after execution. ZIP byte hashes are
checked independently. Extraction rejects path traversal, symlinks, duplicate
case-insensitive Windows names, alternate data streams, reserved device names,
trailing-dot/space names, excessive entries/bytes and encrypted members before
writing. The root executable must be an x86_64 PE. Every extracted file is hashed
before and after execution.

The real packaged executable runs version, source-defined doctor, missing-path
worker and the original scanned-PDF OCR invocation first. No GitHub, proxy,
license acceptance or inherited model configuration reaches it. Its Windows
profile/data/cache/temp locations are explicitly isolated. Windows capture uses
private temporary files, polls size/timeout limits, and retains only bounded
sanitized stdout/stderr snapshots with exit/failure flags and hashes. Raw capture
is not uploaded. Timeout termination targets only the invoked process tree;
incomplete termination or excessive/rejected output fails the observation.
The file-capture branch has local subprocess regressions; actual Windows runner
behavior remains unverified until an authorized hosted execution.

Only an explicit original missing-model rejection plus the exact app-declared
permissive DBNet/TrOCR pair can authorize an explicit model pull through the
normal app license/download gate. Both resulting GGUF hashes/byte counts are
independently pinned before one OCR retry. Qwen and blanket consent are excluded.
The reviewed sanitizer and model controls are shared with the macOS replay;
only executable path and source-defined platform/architecture expectations differ.
Both original rejection and any later retry result are retained separately.

Success requires real hOCR with `ocr_page`, `cobalt`, `warranty` and `2031`, plus
the startup controls. The result is a distinct workflow-artifact observation with
`satisfies_publication_gate=false`, not a release receipt, signing result, GUI or
quality-corpus qualification. Tests that simulate consumer responses check
orchestration only and do not claim Windows/native execution. The original
failure remains part of the record.

## Retained-package replay result

A subsequent source-bound Windows replay completed successfully. It retained the
initial missing-model rejection, explicitly installed the hash-pinned permissive
model pair and passed the scanned-PDF hOCR control on retry. Independent review
checked the command records, diagnostic hashes, source identities and every
retained ZIP member. This establishes one packaged CLI fixture result; the
historical release failure remains unexplained, the package is unsigned and no
publication receipt or physical-device, GUI or general OCR qualification follows.
