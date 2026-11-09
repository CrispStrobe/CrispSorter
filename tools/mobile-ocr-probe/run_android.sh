#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
device=emulator-5554
remote="/data/local/tmp/crispsorter-ocr-ci-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT"
mkdir -p "$RUNNER_TEMP/ocr-mobile-evidence"
adb -s "$device" shell mkdir -p "$remote/inputs"
adb -s "$device" push "$RUNNER_TEMP/ocr-mobile-probe" "$remote/ocr-mobile-probe"
adb -s "$device" push "$CRISPEMBED_SYS_LIB_DIR/libcrispembed.so" "$remote/libcrispembed.so"
adb -s "$device" push "$RUNNER_TEMP/ocr-mobile-inputs/." "$remote/inputs/"
adb -s "$device" shell chmod 700 "$remote/ocr-mobile-probe"
OCR_PROBE_REMOTE="$remote" python3 - <<'PY'
import os, pathlib, subprocess
remote = os.environ['OCR_PROBE_REMOTE']
output = pathlib.Path(os.environ['RUNNER_TEMP']) / 'ocr-mobile-evidence'
try:
    with (output / 'native.log').open('w') as log:
        command = f"LD_LIBRARY_PATH='{remote}' '{remote}/ocr-mobile-probe' '{remote}/inputs' '{remote}/native.json'"
        subprocess.run(['adb', '-s', 'emulator-5554', 'shell', command],
                       stdout=log, stderr=subprocess.STDOUT, timeout=300, check=True)
finally:
    subprocess.run(['adb', '-s', 'emulator-5554', 'pull', remote + '/native.json', str(output / 'native.json')],
                   check=False, timeout=30)
PY
python3 "$repo/scripts/mobile_simulator_smoke.py" \
  --platform android --target "$device" \
  --artifact "$RUNNER_TEMP/ocr-mobile-app/CrispSorter-ocr.apk" \
  --output "$RUNNER_TEMP/ocr-mobile-evidence/app-startup"

sorter_commit=$(git -C "$repo" rev-parse HEAD)
embed_commit=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source_commit"])' "$repo/scripts/ocr-mobile-runtime-lock.json")
node "$repo/scripts/mobile_ocr_webview_smoke.mjs" \
  --serial "$device" \
  --apk "$RUNNER_TEMP/ocr-mobile-app/CrispSorter-ocr.apk" \
  --inputs "$RUNNER_TEMP/ocr-mobile-inputs" \
  --output "$RUNNER_TEMP/ocr-mobile-evidence/app-native-commands" \
  --source-commit "$sorter_commit" --embed-commit "$embed_commit" \
  --runtime-receipt "$RUNNER_TEMP/ocr-mobile-runtime/runtime-build.json"
