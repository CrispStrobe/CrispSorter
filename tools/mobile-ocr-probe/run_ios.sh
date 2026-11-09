#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$RUNNER_TEMP/ocr-mobile-evidence" "$RUNNER_TEMP/ocr-mobile-app"
simulator=$(xcrun simctl list devices available --json | python3 -c 'import json,sys; d=json.load(sys.stdin)["devices"]; print(next(x["udid"] for runtime,devices in d.items() if "iOS" in runtime for x in devices if x["name"].startswith("iPhone")))')
state=$(xcrun simctl list devices available --json | OCR_SIMULATOR="$simulator" python3 -c 'import json,os,sys; print(next(x["state"] for devices in json.load(sys.stdin)["devices"].values() for x in devices if x["udid"]==os.environ["OCR_SIMULATOR"]))')
if [[ "$state" != Booted ]]; then xcrun simctl boot "$simulator"; fi
xcrun simctl bootstatus "$simulator" -b
codesign --force --sign - "$RUNNER_TEMP/ocr-mobile-probe"
OCR_SIMULATOR="$simulator" python3 - <<'PY'
import os, pathlib, subprocess
root = pathlib.Path(os.environ['RUNNER_TEMP'])
with (root / 'ocr-mobile-evidence/native.log').open('w') as log:
    subprocess.run(['xcrun', 'simctl', 'spawn', os.environ['OCR_SIMULATOR'],
                    str(root / 'ocr-mobile-probe'), str(root / 'ocr-mobile-inputs'),
                    str(root / 'ocr-mobile-evidence/native.json')],
                   stdout=log, stderr=subprocess.STDOUT, timeout=300, check=True)
PY
app=$(OCR_REPO="$repo" python3 - <<'PY'
import os,pathlib
root = pathlib.Path(os.environ['OCR_REPO']) / 'src-tauri/gen/apple'
apps = list(root.rglob('CrispSorter.app'))
if not apps:
    raise SystemExit('No OCR simulator application was built')
apps.sort(key=lambda p: ('xcarchive' in str(p), len(str(p))))
print(apps[0])
PY
)
python3 "$repo/scripts/ocr_mobile_build_receipt.py" --platform ios-simulator \
  --artifact "$app" --runtime-receipt "$RUNNER_TEMP/ocr-mobile-runtime/runtime-build.json" \
  --output "$RUNNER_TEMP/ocr-mobile-app/build-receipt.json"
python3 "$repo/scripts/mobile_simulator_smoke.py" \
  --platform ios --target "$simulator" --artifact "$app" \
  --output "$RUNNER_TEMP/ocr-mobile-evidence/app-startup"
ditto -c -k --sequesterRsrc --keepParent "$app" "$RUNNER_TEMP/ocr-mobile-app/CrispSorter-ocr-simulator.zip"
