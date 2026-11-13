# Scoped Qwen internal evaluation smoke

The operator explicitly accepted the Qwen Research License for
Qwen2.5-VL-3B **internal non-commercial evaluation only**. This preparation does
not authorize commercial use, publication, candidate promotion, or other
restricted models. The subsequent smoke result appears below.

The existing CI workflow's proposed `scope=qwen-live` selects only its Linux
Rust job. `qwen_noncommercial_evaluation=true` records this explicit evaluation
scope; the early preflight fails before expensive setup if it is absent.
Selecting a different scope never enables Qwen acceptance. No concurrency block
is changed, and no GitHub push or workflow dispatch was performed here.

The actual test is
`extractors::ocr_crispembed::tests::qwen_internal_noncommercial_smoke_live`.
It uses the unchanged production **multilingual** preset (Qwen engine,
`qwen2vl-3b`, 4096-token ceiling, full fixture, no reduced pixel budget). This
is the narrowest existing preset sufficient for a Qwen wiring smoke. The
existing four-preset `ocr_task_presets_live` test, its package sealer and public
qualification requirements are unchanged and remain separate.

The reviewed production runtime source remains
`c364feb974b91e14cbf328c6949f3df36e89ce81`. Its registry selects one integrated
vision/decoder GGUF, not a split mmproj pair:

| Input | Reviewed identity |
| --- | --- |
| Model | `qwen2vl-3b` / `qwen2.5-vl-3b-q4_k.gguf` |
| Model bytes | 2,742,343,200 |
| Model SHA-256 | `4cc16e226005a2d7ecce7749d5a876704a499ca301428167f5ceb3b75b967312` |
| Fixture | `tests/quality/ocr-task.png` |
| Fixture SHA-256 | `25f0e5ee8d518403fc97a94435f88d525526649adbc08c2b937c3469ff4fb4ef` |
| Expected words | cobalt, warranty, November, 2031 |

The model hash comes from the pinned native registry. A read-only model HEAD
request independently returned the same content hash and byte count; weights
were not downloaded. `scripts/qwen-internal-smoke-profile.json` records these
pins and explicitly sets `satisfies_publication_gate=false`.

The child process alone receives the model-specific app consent
`CRISPSORTER_ACCEPT_MODEL_LICENSE=qwen2vl-3b` and the native engine acknowledgment
`CRISPEMBED_ACCEPT_QWEN_RESEARCH_LICENSE=1`. The production app resolver passes
that model's exact native license tag through its existing per-model API; no
blanket native download consent is added. The ignored Rust test itself grants
no consent. It checks the explicit child context, native registry identity,
full downloaded/cached model hash and size, fixture hash, then runs real
production inference. It records the source, preset, configuration and known
word checks. Any inherited blanket app consent is replaced in the child;
the parent process environment stays unchanged.

The runner preserves frontend/native/toolchain setup and warm dependency cache
from the existing Linux job. It executes exactly one ignored test with
`--locked --exact`, two Cargo jobs, one test thread and CPU forcing. The shared
90-minute command/8 MiB diagnostic bounds and failure-safe source checks remain.
A cold cache still compiles the feature-rich library test binary; no unrelated
CLI build, full suite, platform job, cloud test or GPU comparison runs.

Model cache and raw intermediate receipt stay in separate runner temporary
paths outside the artifact directory. Only a validated, minimized native
observation plus source receipt and sanitized diagnostics are uploaded for
14 days. Models and raw intermediate records are not uploaded. Unknown native
receipt fields, changed source/model/fixture identity, zero matching tests,
failed recognition or capture failure cannot claim a pass. Diagnostic and text
withholding remain explicit. Every outer/native receipt states the internal
non-commercial scope and that it does not satisfy the publication gate.

The VPS cache inspection found no exact Qwen file. Loading/downloading this
model locally would burden its constrained RAM/disk, so no inference or app
compilation ran there. Existing Kaggle CPU definitions lacked offline model and
toolchain inputs; that route was excluded under the usage guide. Existing
Paddle work is unrelated and is not duplicated. At preparation, execution was
pending review and integration. The later smoke result is recorded below; the
user's acceptance applies only to the stated internal evaluation.

Local validation: seventeen Qwen model-free tests and twenty-eight shared
selector/source/capture/privacy tests pass, plus actionlint and Rust syntax
parsing. At preparation a full application typecheck and live inference had not
been run. Any later run must use the intended reviewed immutable PR revision,
`scope=qwen-live`, and `qwen_noncommercial_evaluation=true`; no other scope
receives model acceptance.

Raw intermediate cleanup failures also fail closed: the outer failed source
receipt is persisted and a pending success return is canceled. Cleanup failure
cannot make the CLI print success; raw exception text is not retained.

If cleanup also fails after a primary validation/capture failure, the original
failure category remains and a separate cleanup failure category is recorded.

## Subsequent smoke result

The source-bound CPU smoke subsequently passed for the authorized internal,
non-commercial evaluation. Its native evidence contains the expected fixture
text, “Cobalt warranty expires November 2031.”, with about 62 seconds spent in
the inference call. An independent audit checked the retained artifact digest,
source and input hashes, model size and digest, and the production OCR call.
The privacy filter withheld the raw Cargo diagnostics, so the single-test count
is supported by the source-bound runner receipt and successful job, rather than
an independently readable Cargo transcript. The generated license-inventory
exception is recorded in that evidence. This one fixture does not qualify the
full preset suite, general OCR quality, mobile or GPU behavior, commercial use,
or publication.
