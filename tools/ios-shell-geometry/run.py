#!/usr/bin/env python3
"""Run source-matched shell geometry controls in iOS WebKit, without the OCR app."""
import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
from pathlib import Path
from prepare import generate, ROOT

HERE = Path(__file__).resolve().parent


def command(args, timeout=60, cwd=None):
    result = subprocess.run(args, check=True, text=True, capture_output=True, timeout=timeout, cwd=cwd)
    return result.stdout.strip()


def inventory(kind, receipt):
    for timeout in (90, 180):
        try:
            value = json.loads(command(['xcrun', 'simctl', 'list', kind, '--json'], timeout))
            receipt.setdefault('discovery', []).append({'kind': kind, 'timeout': timeout, 'status': 'passed'})
            return value
        except subprocess.TimeoutExpired:
            receipt.setdefault('discovery', []).append({'kind': kind, 'timeout': timeout, 'status': 'timed_out'})
    raise RuntimeError('Both bounded CoreSimulator discovery attempts timed out')


def recover(simulator, package, folder, destination):
    if destination.exists():
        return
    container = Path(command(['xcrun', 'simctl', 'get_app_container', simulator, package, 'data'], 30))
    source = container / 'Documents' / folder
    files = list(source.rglob('*'))
    if (not source.is_dir() or source.is_symlink() or len(files) > 100
            or any(p.is_symlink() for p in files)
            or sum(p.stat().st_size for p in files if p.is_file()) > 80 * 1024 * 1024):
        raise RuntimeError('Missing or out-of-bounds geometry evidence')
    shutil.copytree(source, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    out = args.output.resolve()
    if out.exists() and any(out.iterdir()):
        raise ValueError('Use a new geometry evidence directory')
    out.mkdir(parents=True, exist_ok=True)
    sources = generate(out / 'Resources')
    receipt = {'schema_version': 1, 'kind': 'ios-shell-geometry-control', 'status': 'failed',
               'source': sources, 'github_run': os.environ.get('GITHUB_RUN_ID'),
               'scope': 'Source CSS/viewport in a small native WebKit fixture; no actual OCR application navigation or recognition.',
               'helper_sha256': {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest()
                                 for p in sorted(HERE.rglob('*')) if p.suffix in ('.py', '.swift', '.yml')}}
    simulator = None
    def save(): (out / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    save()
    try:
        if platform.system() != 'Darwin' or platform.machine() != 'arm64':
            raise RuntimeError('This fixture requires an arm64 macOS simulator host')
        receipt['xcode'] = command(['xcodebuild', '-version'])
        runtimes = [r for r in inventory('runtimes', receipt)['runtimes']
                    if r.get('isAvailable') and r.get('version') == '18.5' and '.iOS-' in r['identifier']]
        devices = [d for d in inventory('devicetypes', receipt)['devicetypes'] if d['name'] == 'iPhone 16 Pro']
        if len(runtimes) != 1 or len(devices) != 1:
            raise RuntimeError('The pinned iPhone 16 Pro / iOS 18.5 simulator is unavailable')
        simulator = command(['xcrun', 'simctl', 'create', 'OCR shell geometry', devices[0]['identifier'], runtimes[0]['identifier']])
        receipt['simulator'] = {'udid': simulator, 'device': 'iPhone 16 Pro', 'runtime': '18.5'}
        save()
        command(['xcrun', 'simctl', 'boot', simulator])
        command(['xcrun', 'simctl', 'bootstatus', simulator, '-b'], 180)
        os.environ['OCR_SHELL_RESOURCES'] = str(out / 'Resources')
        command(['xcodegen', 'generate', '--spec', str(HERE / 'project.yml')], cwd=HERE)
        with (out / 'xcodebuild.log').open('w') as log:
            result = subprocess.run(['xcodebuild', 'test', '-project', str(HERE / 'ShellGeometry.xcodeproj'),
                                     '-scheme', 'ShellGeometry', '-configuration', 'Debug',
                                     '-destination', f'platform=iOS Simulator,id={simulator}',
                                     '-derivedDataPath', str(out / 'DerivedData'),
                                     '-resultBundlePath', str(out / 'Geometry.xcresult'),
                                     '-parallel-testing-enabled', 'NO', 'CODE_SIGNING_ALLOWED=NO'],
                                    stdout=log, stderr=subprocess.STDOUT, timeout=600)
        receipt['xcodebuild_exit_code'] = result.returncode
        recover(simulator, 'com.crispstrobe.shellGeometryTests.xctrunner', 'geometry-tests', out / 'evidence')
        expected = ['baseline-portrait', 'current-portrait-menu', 'current-landscape', 'current-large-text-menu']
        if result.returncode or any(not (out / 'evidence' / f'{name}.json').is_file() for name in expected):
            raise RuntimeError('The complete baseline/current native geometry suite did not pass')
        receipt['status'] = 'passed'
    except Exception as error:
        receipt['error'] = str(error)
    finally:
        if simulator:
            for package, folder, destination in [('com.crispstrobe.shellGeometryTests.xctrunner', 'geometry-tests', 'evidence'),
                                                  ('com.crispstrobe.shellGeometry', 'geometry', 'host-metrics')]:
                try: recover(simulator, package, folder, out / destination)
                except Exception as error: receipt[destination + '_recovery_error'] = str(error)
            try: command(['xcrun', 'simctl', 'shutdown', simulator])
            except Exception as error: receipt['shutdown_error'] = str(error)
        result_bundle = out / 'Geometry.xcresult'
        if result_bundle.exists():
            try:
                with (out / 'attachment-export.log').open('w') as log:
                    result = subprocess.run(['xcrun', 'xcresulttool', 'export', 'attachments', '--path', str(result_bundle),
                                             '--output-path', str(out / 'attachments')], stdout=log, stderr=subprocess.STDOUT, timeout=60)
                receipt['attachment_export_exit_code'] = result.returncode
            except Exception as error: receipt['attachment_export_error'] = str(error)
        save()
    print(json.dumps({'status': receipt['status'], 'error': receipt.get('error')}))
    return 0 if receipt['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
