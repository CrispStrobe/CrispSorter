#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$RUNNER_TEMP/ocr-mobile-evidence" "$RUNNER_TEMP/ocr-mobile-app"
# Preserve the completed build before any simulator operation can fail or hang.
# This archive is evidence to inspect, not a successful application result.
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
ditto -c -k --sequesterRsrc --keepParent "$app" "$RUNNER_TEMP/ocr-mobile-app/CrispSorter-ocr-simulator.zip"
OCR_REPO="$repo" python3 - <<'PY'
from datetime import datetime, timezone
import hashlib, json, os, pathlib, subprocess
root = pathlib.Path(os.environ['RUNNER_TEMP']) / 'ocr-mobile-app'
archive = root / 'CrispSorter-ocr-simulator.zip'
with archive.open('rb') as stream:
    sha256 = hashlib.file_digest(stream, 'sha256').hexdigest()
value = {'schema_version': 1, 'kind': 'preserved-ios-compiled-bundle',
         'recorded_at': datetime.now(timezone.utc).isoformat(),
         'source_commit': subprocess.check_output(['git', '-C', os.environ['OCR_REPO'], 'rev-parse', 'HEAD'], text=True).strip(),
         'archive': archive.name, 'bytes': archive.stat().st_size, 'sha256': sha256,
         'status_at_preservation': 'unqualified',
         'qualification': 'Preserved before simulator discovery. This does not establish build identity, native inference, or successful app startup; each requires its separate passing receipt.'}
with (root / 'compiled-bundle.json').open('x') as stream:
    json.dump(value, stream, indent=2)
    stream.write('\n')
PY
simulator=$(xcrun simctl list devices available --json | python3 -c 'import json,sys; d=json.load(sys.stdin)["devices"]; print(next(x["udid"] for runtime,devices in d.items() if "iOS" in runtime for x in devices if x["name"].startswith("iPhone")))')
state=$(xcrun simctl list devices available --json | OCR_SIMULATOR="$simulator" python3 -c 'import json,os,sys; print(next(x["state"] for devices in json.load(sys.stdin)["devices"].values() for x in devices if x["udid"]==os.environ["OCR_SIMULATOR"]))')
if [[ "$state" != Booted ]]; then xcrun simctl boot "$simulator"; fi
xcrun simctl bootstatus "$simulator" -b
codesign --force --sign - "$RUNNER_TEMP/ocr-mobile-probe"
codesign --force --sign - "$RUNNER_TEMP/mobile-ort-probe"
OCR_SIMULATOR="$simulator" python3 - <<'PY'
import json, os, pathlib, subprocess
root = pathlib.Path(os.environ['RUNNER_TEMP'])
runtime = json.loads((root / 'ocr-mobile-ort/ort-runtime.json').read_text())
with (root / 'ocr-mobile-evidence/onnx-runtime.log').open('w') as log:
    for version, name, expected_success in [('0.0.0', 'onnx-wrong-version.json', False), (runtime['version'], 'onnx-runtime.json', True)]:
        destination = root / 'ocr-mobile-evidence' / name
        result = subprocess.run(['xcrun', 'simctl', 'spawn', os.environ['OCR_SIMULATOR'], str(root / 'mobile-ort-probe'), version, str(destination)],
                                stdout=log, stderr=subprocess.STDOUT, timeout=90)
        receipt = json.loads(destination.read_text())
        if expected_success:
            assert result.returncode == 0 and receipt['status'] == 'passed'
            assert receipt['runtime_version'] == runtime['version'] and receipt['api_version'] == 23
            assert receipt['output'] == [11, 22, 33, 44] and receipt['iterations'] == 3
        else:
            assert result.returncode != 0 and receipt['status'] == 'failed' and 'version differs' in receipt['error']
with (root / 'ocr-mobile-evidence/native.log').open('w') as log:
    subprocess.run(['xcrun', 'simctl', 'spawn', os.environ['OCR_SIMULATOR'],
                    str(root / 'ocr-mobile-probe'), str(root / 'ocr-mobile-inputs'),
                    str(root / 'ocr-mobile-evidence/native.json')],
                   stdout=log, stderr=subprocess.STDOUT, timeout=300, check=True)
PY
python3 "$repo/scripts/ocr_mobile_build_receipt.py" --platform ios-simulator \
  --artifact "$app" --runtime-receipt "$RUNNER_TEMP/ocr-mobile-runtime/runtime-build.json" \
  --ort-runtime-receipt "$RUNNER_TEMP/ocr-mobile-ort/ort-runtime.json" \
  --output "$RUNNER_TEMP/ocr-mobile-app/build-receipt.json"
python3 "$repo/scripts/mobile_simulator_smoke.py" \
  --platform ios --target "$simulator" --artifact "$app" \
  --output "$RUNNER_TEMP/ocr-mobile-evidence/app-startup"
