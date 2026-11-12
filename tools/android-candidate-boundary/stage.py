#!/usr/bin/env python3
"""Stage the pinned candidate APK/PNG and keep app and harness sources distinct."""
import argparse
import hashlib
import json
from pathlib import Path
import runpy
import stat
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts'))
from mobile_ocr_run_identity import verified_embed_commit
from ocr_mobile_build_receipt import digest, git

PIN = 'tools/android-candidate-boundary/artifact.json'


def stage(archive, metadata, output, app_source, harness=ROOT):
    harness, app_source, output = harness.resolve(), app_source.resolve(), output.resolve()
    committed = subprocess.check_output(['git', '-C', str(harness), 'show', 'HEAD:' + PIN])
    if (harness / PIN).read_bytes() != committed:
        raise ValueError('Replay artifact pin differs from its committed bytes')
    pin = json.loads(committed)
    if (git(app_source, 'rev-parse', 'HEAD') != pin['application_source']
            or git(app_source, 'rev-parse', 'HEAD^{tree}') != pin['application_tree']
            or git(app_source, 'status', '--porcelain', '--untracked-files=all')):
        raise ValueError('Replay application source checkout differs from its clean pinned source')
    if git(harness, 'status', '--porcelain', '--untracked-files=all'):
        raise ValueError('Replay harness source checkout must be clean')
    # Existing staging validates the full archive digest, metadata, exact APK
    # bytes, original app tree and build receipt. No arbitrary paths extract.
    retained = runpy.run_path(str(harness / 'tools/android-startup-probe/stage.py'))
    with zipfile.ZipFile(archive) as zipped:
        selected = {}
        for name, expected in pin['staged_members'].items():
            info = zipped.getinfo(name)
            if (stat.S_IFMT(info.external_attr >> 16) not in (0, stat.S_IFREG)
                    or info.file_size != expected['bytes'] or not 0 < info.file_size <= 2 * 1024 * 1024):
                raise ValueError('Replay fixture/receipt is not the bounded pinned regular file')
            data = zipped.read(info)
            if hashlib.sha256(data).hexdigest() != expected['sha256']:
                raise ValueError('Replay fixture/receipt hash differs from its committed pin')
            destination = Path(expected['destination'])
            if destination.is_absolute() or '..' in destination.parts:
                raise ValueError('Replay pin names an unsafe destination')
            selected[destination] = data
    retained['stage'](archive, metadata, output, pin)
    for destination, data in selected.items():
        path = output / destination
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    embed = verified_embed_commit(app_source, pin['runtime_lock'], output / 'build-receipt.json',
                                  output / 'runtime-build.json', pin['native_source'])
    build = json.loads((output / 'build-receipt.json').read_text())
    if build['ggml_commit'] != pin['ggml_source'] or not build.get('candidate_reuse'):
        raise ValueError('Replay receipt is not the source-pinned candidate runtime')
    inputs = json.loads((output / 'inputs/inputs.json').read_text())
    baseline = json.loads((app_source / 'tools/mobile-ocr-probe/models.json').read_text())
    harness_baseline = json.loads((harness / 'tools/mobile-ocr-probe/models.json').read_text())
    if (inputs['models'] != baseline['models'] or inputs['models'] != harness_baseline['models']
            or digest(output / 'inputs/page.png') != inputs['fixture']['png_sha256']):
        raise ValueError('Replay PNG or baseline model pins differ from original app and harness sources')
    result = {'schema_version': 1, 'kind': 'android-candidate-boundary-replay-input',
              'application_source': pin['application_source'], 'application_tree': pin['application_tree'],
              'harness_source': git(harness, 'rev-parse', 'HEAD'),
              'harness_tree': git(harness, 'rev-parse', 'HEAD^{tree}'),
              'apk_sha256': pin['apk_sha256'], 'native_source': embed,
              'runtime_lock': build['runtime_lock'], 'build_receipt_sha256': digest(output / 'build-receipt.json'),
              'artifact_pin_sha256': digest(harness / PIN), 'artifact': pin,
              'application_rebuilt': False, 'native_runtime_rebuilt': False,
              'qualification': 'Input staging only; fresh emulator execution is required.'}
    (output / 'replay-input.json').write_text(json.dumps(result, indent=2) + '\n')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('archive', 'metadata', 'output', 'app-source'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    stage(args.archive, json.loads(args.metadata.read_text()), args.output, args.app_source)


if __name__ == '__main__':
    main()
