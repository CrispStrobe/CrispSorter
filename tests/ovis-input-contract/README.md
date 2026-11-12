# Ovis inference-copy source contract

This narrow crate compiles the app's actual changed pipeline execution and all
four conversion paths, selected from source by `build.rs`. Config, document,
review, word-evidence and renderer types also come from production source.
CrispEmbed region/result types and execution method signatures come from the
exact commit in `scripts/crispembed-runtime-lock.json`, read from a local sibling
CrispEmbed Git checkout (`CRISPEMBED_SOURCE` can select that checkout). Missing
items or source fail preparation; no source is fetched.

Only model execution/cache and log sinks are fixtures. Real image decoding,
resizing, PNG I/O, temporary-file ownership, geometry mapping and conversions run.
The fixture is not native inference, native schema qualification or OCR quality
evidence. It does not verify the complete application or packaged runtime.

When host load/memory permit, execute sequentially with one compilation job:

```sh
cargo test --manifest-path tests/ovis-input-contract/Cargo.toml --offline -j 1 -- --test-threads 1
cargo test --manifest-path tests/ovis-input-contract/Cargo.toml --offline -j 1 --features ocr-review-diagnostics -- --test-threads 1
```

Controls cover disabled snapshot/language-call/path behavior, diagnostics-feature refusal,
actual inference PNG dimensions, positive mapping across all four conversions,
identity opt-in, source bytes, temporary permission/lifetime, native failure,
invalid/missing snapshots, unsupported nested word evidence, invalid coordinates,
unsupported recipes, unrelated engines, rounding/bounds and f32 collapse.
