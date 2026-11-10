# Reproduce the native system OCR panel

This procedure runs the production `ocr_word_evidence.rs` adapter and validates its same-run word evidence on every page in the frozen plan. It requires an installed Tesseract 4 or newer executable, Rust and Python 3.11 or newer. The measurement wrapper uses POSIX process-group timeout handling; the native module also has separate platform contract tests.

Install the pinned Python requirement in an isolated environment:

```sh
python3 -m pip install -r tools/quality/system-ocr-requirements.txt
cargo test --locked --manifest-path tests/ocr-word-contract/Cargo.toml
cargo build --locked --manifest-path tests/ocr-word-contract/Cargo.toml
python3 tools/quality/system_ocr_panel.py \
  --input-dir /absolute/evaluation-inputs \
  --output /absolute/new-evaluation-result \
  --executable /absolute/path/to/ocr-word-contract
```

The input and output directories must be outside this checkout. An existing result is never overwritten. Add `--prepare-only` to download and verify the frozen images, reference manifest, scorer and language packs without inference. The plan includes exact URLs, revisions, sizes and hashes; the upstream manifest retains image and reference licenses. The receipt image is extracted from its pinned public archive row and verified independently.

The executable is a thin adapter around the actual production recognizer. The wrapper sets `TESSDATA_PREFIX`, a private temporary directory and the frozen thread limit. It attempts all six pages, including failures, and records each language setting, native wall time, score, word-evidence count and binary/source hash. Native confidence remains uncalibrated. Recognized text and raw image data are not included in the shareable summary.

For the partial receipt reference, report word recall as the primary measure. Other error rates are retained for inspection but do not measure a complete transcription. `launcher_peak_rss_mb` includes the measurement launcher's inherited memory and is not an isolated engine allocation figure. A single page per category is diagnostic evidence, not release qualification or grounds to change a default model.
