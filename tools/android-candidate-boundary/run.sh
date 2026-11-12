#!/usr/bin/env bash
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
staged="$RUNNER_TEMP/candidate-replay"
# Revalidate source/lock/runtime receipt and APK bytes immediately before use.
python3 "$repo/scripts/mobile_ocr_run_identity.py" --source "$OCR_REPLAY_APP_SOURCE" \
  --runtime-lock scripts/ocr-mobile-error-propagation-candidate-lock.json \
  --build-receipt "$staged/build-receipt.json" --runtime-receipt "$staged/runtime-build.json"
mapfile -t identity < <(python3 - "$repo" "$staged" <<'PY'
import hashlib,json,pathlib,subprocess,sys
repo=pathlib.Path(sys.argv[1]); staged=pathlib.Path(sys.argv[2])
pin=json.loads(subprocess.check_output(['git','-C',str(repo),'show','HEAD:tools/android-candidate-boundary/artifact.json']))
data=json.loads((staged/'replay-input.json').read_text())
files={'CrispSorter-ocr.apk':pin['apk_sha256'],'build-receipt.json':pin['build_receipt_sha256'],
       **{entry['destination']:entry['sha256'] for entry in pin['staged_members'].values()}}
for name,expected in files.items():
    path=staged/name
    if path.is_symlink() or not path.is_file(): raise ValueError('Invalid staged replay file')
    with path.open('rb') as source:
        if hashlib.file_digest(source,'sha256').hexdigest()!=expected:
            raise ValueError('Staged replay APK, receipt or fixture changed: '+name)
if data['application_source']!=pin['application_source'] or data['native_source']!=pin['native_source']:
    raise ValueError('Replay input identities changed')
for key in ('application_source','native_source','harness_source'): print(data[key])
PY
)
[[ ${#identity[@]} == 3 ]]
[[ "${identity[2]}" == "$(git -C "$repo" rev-parse HEAD)" ]]
python3 "$repo/scripts/mobile_simulator_smoke.py" --platform android --target emulator-5554 \
  --artifact "$staged/CrispSorter-ocr.apk" --output "$RUNNER_TEMP/candidate-replay-startup"
node "$repo/scripts/mobile_ocr_webview_smoke.mjs" --serial emulator-5554 \
  --apk "$staged/CrispSorter-ocr.apk" --inputs "$staged/inputs" \
  --output "$RUNNER_TEMP/candidate-replay-evidence" \
  --source-commit "${identity[0]}" --embed-commit "${identity[1]}" \
  --harness-source-commit "${identity[2]}" --qualification candidate-boundary \
  --build-receipt "$staged/build-receipt.json" --runtime-receipt "$staged/runtime-build.json"
