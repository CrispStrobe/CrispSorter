#!/usr/bin/env bash
set -euo pipefail
python3 scripts/mobile_simulator_smoke.py --platform android --target emulator-5554 \
  --artifact "$RUNNER_TEMP/retained-app/CrispSorter-ocr.apk" --output "$RUNNER_TEMP/retained-startup"
app_source=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["sorter_commit"])' "$RUNNER_TEMP/retained-app/build-receipt.json")
embed_source=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["crispembed_commit"])' "$RUNNER_TEMP/retained-app/build-receipt.json")
node scripts/mobile_ocr_webview_smoke.mjs --serial emulator-5554 \
  --apk "$RUNNER_TEMP/retained-app/CrispSorter-ocr.apk" --inputs "$RUNNER_TEMP/ocr-mobile-inputs" \
  --output "$RUNNER_TEMP/retained-app-flow" --source-commit "$app_source" --embed-commit "$embed_source" \
  --build-receipt "$RUNNER_TEMP/retained-app/build-receipt.json" --runtime-receipt "$RUNNER_TEMP/retained-app/runtime-build.json"
