# Original macOS package validation replay

The original `b739a3c0` macOS build uploaded unsigned-labelled DMG and app
archive assets, then failed the final packaged scanned-PDF OCR command with exit
1. Version, doctor and the worker protocol had completed. The verifier used
`capture_output=True, check=True` without retaining the failing stdout/stderr;
the log therefore does not establish the actual rejection. Missing models are a
plausible explanation, not a verified diagnosis: the original noninteractive
command supplies neither download confirmation nor prior model provisioning.

The `macos_validate_b739` selector in the existing Release workflow runs only a
macOS package observation job. It requires a manual dispatch with
`publish=false`, `mac_appstore=false`, and `mobile_profile=standard`. It checks
out original source `b739a3c0b70f39d3ae19e1bfcb412f3b44f77794` separately from the
workflow/tooling source. Contradictory flags fail the replay guard before original
source checkout or package/token access. The replay selector is explicitly excluded
from the App Store job even when `mac_appstore=true`; every unrelated build,
signing and publication job stays excluded. No concurrency block is added or changed.

The replay job alone requests `contents: write`: GitHub hides draft releases
from callers without push access, even though published-release reads accept
read permission. See [GitHub's release API documentation](https://docs.github.com/en/rest/releases/releases#list-releases).
The initial read-scoped replay failed its first lookup while the exact draft was
still present. This permission enables draft visibility; helper GitHub operations
remain GET/list/view/download only. Both checkouts disable persisted credentials,
and the packaged application receives no GitHub token. No other job permission
is changed.

Before execution, the helper dynamically verifies the unique unpublished
`v0.15.1` draft, exact original target/tag, and both exact asset names, byte
counts and SHA-256 digests. Downloaded bytes must independently match those
pins. The helper checks again before execution and after probing; it never
writes to the draft, removes assets, signs, notarizes, compiles, or creates a
release receipt. Execution identifiers are not committed.

The tar is bounded and checked for duplicate/unsafe members before extraction;
only contained application symlinks are allowed. The DMG is mounted read-only.
Both apps must have the expected bundle identifier/version/executable and the
same file-content/symlink inventory. This comparison does not assess signatures,
notarization, extended attributes, or Gatekeeper. Asset names retain their
unsigned labels throughout.

Both packages run the original OCR command first, using the exact committed
scanned PDF. Version, doctor and missing-path worker probes also run. The app
receives a fresh isolated home/cache and a small environment without GitHub,
proxy, signing, consent or model-download environment variables. Each command
retains bounded sanitized stdout and stderr, exit status, timeout/truncation
flags and output hashes, including failures. Timeouts and truncated output fail
the observation. Credentials in URLs and credential-shaped output are redacted.
JSON objects, nested/serialized JSON strings and escaped credential keys are
decoded before recursive redaction. Sensitive HTTP headers expressed as nested
name/value arrays or objects are also redacted, as are recognized Bearer
credentials in free text or JSON string values. Malformed structured output, ambiguous
credential assignments, excessive nesting and truncated output are omitted and
make the command evidence fail closed. Ordinary native `[graph]`/`[info]`
markers, tensor slices, whitespace numeric vectors and bounded numeric model-pull
counters such as `[1/2]` are recognized as log notation; they are not treated as
malformed JSON. The committed model installer emits those counters with carriage
returns. A successful pull previously had only its stderr rejected; the omitted
raw stderr cannot be recovered, so the counter explanation is source-backed
rather than a reconstruction of that hosted output. Credential-bearing markers
and malformed JSON objects/arrays remain rejected. Nonblocking bounded stream collection
preserves sanitized partial records even when descendants keep pipes open after
the main process exits; that condition is explicitly recorded as a failure.
The retained JSON uses a distinct observation schema which cannot satisfy the
release publication gate.

Only if every failing original OCR attempt explicitly reports missing OCR
models, and the packaged CLI plan declares exactly `dbnet-det` (Apache-2.0) and
`trocr-printed` (MIT) without consent requirements, may the helper explicitly
invoke `models pull dbnet_trocr` through the packaged application's normal
licensing and download gates. It independently checks the resulting two GGUF
files against reviewed byte counts and SHA-256 pins in the isolated cache. It
then retries each packaged OCR command once. No Qwen model, consent flag or
blanket license acceptance is permitted. The original failures remain in the
retained evidence; a successful retry is recorded separately. Failure unrelated
to missing models never authorizes provisioning.

Success requires real hOCR with `ocr_page`, `cobalt`, `warranty` and `2031` from
both package origins, plus the version/doctor/worker controls. Synthetic unit
test responses validate orchestration only and are never reported as native
execution. Hosted replay has not been dispatched as part of preparing this
change. A later passing replay would establish only this packaged CLI fixture
control, not GUI behavior, device testing, corpus accuracy, signing, publication
readiness, or a reconstructed original compiled-source/dependency attestation.
The original packages lack a completed original release receipt; source binding
remains their original build/draft association and exact immutable package pins.

## Offline review of retained successful CLI outputs

The original `cmd_doctor` JSON producer has eight boolean fields **and** a
`lance_dir` field containing a path string or null. The replay incorrectly
required exactly eight booleans. Its corrected schema now accepts only the nine
source-defined fields with their actual types and still requires PDF support.
The committed sanitized doctor fixture covers the real retained output.

One reviewed replay retained both original missing-model failures, explicit
permissive model provisioning with verified GGUF hashes, and two successful OCR
retries with expected hOCR. Its overall observation remained failed because of
the incorrect doctor check. It also recorded `detach_failed=true`; that cleanup
failure remains unresolved and must not be discarded when interpreting the CLI
results.

`reevaluate_macos_retained_validation.py` operates offline on that reviewed
workflow artifact. A committed evidence lock pins the exact ZIP digest/size and
all 38 retained file hashes, original tooling source/tree and observed app
content identity. The verifier checks the original source fixture/lock bytes,
exact package identities, command sequence/arguments, complete diagnostic flags,
per-stream hashes, original missing-model rejections, permissive plan/model
identities and both successful retry outputs under the corrected doctor schema.
Changing an output and its editable declared hash cannot replace the independent
committed evidence pins.

After the corrected tooling is committed, use:

```sh
python scripts/reevaluate_macos_retained_validation.py \
  --archive /path/to/retained-artifact.zip \
  --output /path/to/macos-cli.reevaluation.observation.json
```

The output records corrected tooling identity separately from the original
workflow tooling and app association. It verifies **retained CLI controls**,
preserves the original failed observation/error and unresolved DMG detach, and
cannot satisfy publication gates. It does not rerun package checks, infer
current draft state, download models, assess signatures, or reconstruct missing
original compiled-source receipts. The original ZIP and observation remain
unchanged. This is a review of existing sanitized evidence, not a new successful
macOS replay, signing result or release receipt.
