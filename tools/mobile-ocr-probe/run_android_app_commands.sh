#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
sorter_commit=$(git -C "$repo" rev-parse HEAD)
embed_commit=$(python3 "$repo/scripts/mobile_ocr_run_identity.py" \
  --source "$repo" \
  --runtime-lock "${OCR_MOBILE_RUNTIME_LOCK:-scripts/ocr-mobile-runtime-lock.json}" \
  --expected-source "${OCR_MOBILE_RUNTIME_SOURCE:-}" \
  --build-receipt "$RUNNER_TEMP/ocr-mobile-app/build-receipt.json" \
  --runtime-receipt "$RUNNER_TEMP/ocr-mobile-runtime/runtime-build.json")
node "$repo/scripts/mobile_ocr_webview_smoke.mjs" \
  --serial emulator-5554 \
  --apk "$RUNNER_TEMP/ocr-mobile-app/CrispSorter-ocr.apk" \
  --inputs "$RUNNER_TEMP/ocr-mobile-inputs" \
  --output "$RUNNER_TEMP/ocr-mobile-evidence/app-native-commands" \
  --source-commit "$sorter_commit" --embed-commit "$embed_commit" \
  --build-receipt "$RUNNER_TEMP/ocr-mobile-app/build-receipt.json" \
  --runtime-receipt "$RUNNER_TEMP/ocr-mobile-runtime/runtime-build.json"
