#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
device=emulator-5554
remote="/data/local/tmp/crispsorter-ocr-ci-$GITHUB_RUN_ID-$GITHUB_RUN_ATTEMPT"
mkdir -p "$RUNNER_TEMP/ocr-mobile-evidence"
adb -s "$device" shell mkdir -p "$remote/inputs"
OCR_PROBE_REMOTE="$remote" python3 - <<'PY'
import hashlib, json, os, pathlib, subprocess, zipfile
root = pathlib.Path(os.environ['RUNNER_TEMP'])
remote = os.environ['OCR_PROBE_REMOTE']
output = root / 'ocr-mobile-evidence'
build = json.loads((root / 'ocr-mobile-app/build-receipt.json').read_text())
runtime = build['onnx_runtime']['runtime']
identity = build['onnx_runtime']['linkage']
with zipfile.ZipFile(root / 'ocr-mobile-app/CrispSorter-ocr.apk') as archive:
    packaged = archive.read(identity['packaged_library'])
assert hashlib.sha256(packaged).hexdigest() == identity['packaged_sha256']
library = output / 'libonnxruntime-from-apk.so'
library.write_bytes(packaged)
subprocess.run(['adb', '-s', 'emulator-5554', 'push', str(library), remote + '/libonnxruntime.so'], check=True, timeout=60)
subprocess.run(['adb', '-s', 'emulator-5554', 'push', str(root / 'mobile-ort-probe'), remote + '/mobile-ort-probe'], check=True, timeout=60)
subprocess.run(['adb', '-s', 'emulator-5554', 'shell', 'chmod', '700', remote + '/mobile-ort-probe'], check=True, timeout=30)
with (output / 'onnx-runtime.log').open('w') as log:
    for version, name, expected_success in [('0.0.0', 'onnx-wrong-version.json', False), (runtime['version'], 'onnx-runtime.json', True)]:
        command = f"LD_LIBRARY_PATH='{remote}' '{remote}/mobile-ort-probe' '{version}' '{remote}/{name}'"
        result = subprocess.run(['adb', '-s', 'emulator-5554', 'shell', command], stdout=log, stderr=subprocess.STDOUT, timeout=90)
        destination = output / name
        subprocess.run(['adb', '-s', 'emulator-5554', 'pull', remote + '/' + name, str(destination)], check=True, timeout=30)
        receipt = json.loads(destination.read_text())
        if expected_success:
            assert result.returncode == 0 and receipt['status'] == 'passed'
            assert receipt['runtime_version'] == runtime['version'] and receipt['api_version'] == 23
            assert receipt['output'] == [11, 22, 33, 44] and receipt['iterations'] == 3
        else:
            assert result.returncode != 0 and receipt['status'] == 'failed' and 'version differs' in receipt['error']
PY
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

bash "$repo/tools/mobile-ocr-probe/run_android_app_commands.sh"
