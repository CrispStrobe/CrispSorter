#!/usr/bin/env python3
"""Run a small XCUITest target against the already-built, verified application."""
import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))
from ocr_mobile_build_receipt import bundle_identity

RUNNER_PACKAGE = 'com.crispstrobe.ocrShellProbeTests.xctrunner'


def output(args, *, cwd=None, timeout=60):
    result = subprocess.run(args, cwd=cwd, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f'{args[0]} exited {result.returncode}: {result.stderr[-4000:]}')
    return result.stdout.strip()


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(64 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--staged', required=True, type=Path)
    args = parser.parse_args()
    staged = args.staged.resolve()
    input_receipt = json.loads((staged / 'input-receipt.json').read_text())
    pin = input_receipt['pin']
    receipt = {
        'schema_version': 1, 'kind': 'ios-shell-probe-run', 'status': 'failed',
        'recorded_at': datetime.now(timezone.utc).isoformat(),
        'application_source': pin['application_source'], 'application_rebuilt': False,
        'artifact_id': pin['artifact_id'], 'artifact_sha256': pin['artifact_sha256'],
        'source_bundle_sha256': pin['bundle_sha256'],
        'probe_source': output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD']),
        'probe_files': {str(path.relative_to(ROOT)): digest(path)
                        for path in [*sorted(HERE.rglob('*.py')), *sorted(HERE.rglob('*.swift')),
                                     HERE / 'project.yml', HERE / 'artifact.json']},
        'github': {key: os.environ.get(key) for key in ('GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_SHA')},
        'scope': 'Retained application byte identity plus simulator UI navigation; no native rebuild, OCR accuracy test, or physical phone test.',
    }
    simulator = None
    test_exit = None
    try:
        if platform.system() != 'Darwin' or platform.machine() != 'arm64':
            raise RuntimeError('This retained app requires an arm64 macOS simulator host')
        receipt['xcode_version'] = output(['xcodebuild', '-version'])
        receipt['xcodegen_version'] = output(['xcodegen', '--version'])
        receipt['simulator_sdk'] = output(['xcrun', '--sdk', 'iphonesimulator', '--show-sdk-version'])
        runtime_list = json.loads(output(['xcrun', 'simctl', 'list', 'runtimes', '--json']))
        runtimes = [item for item in runtime_list['runtimes']
                    if item.get('isAvailable') and item.get('version') == pin['simulator_runtime']
                    and '.iOS-' in item['identifier']]
        devices = json.loads(output(['xcrun', 'simctl', 'list', 'devicetypes', '--json']))
        device_types = [item for item in devices['devicetypes'] if item['name'] == pin['simulator_device']]
        if len(runtimes) != 1 or len(device_types) != 1:
            raise RuntimeError('The exact retained-build simulator device/runtime is unavailable')
        simulator = output(['xcrun', 'simctl', 'create', 'OCR shell navigation probe',
                            device_types[0]['identifier'], runtimes[0]['identifier']])
        receipt['simulator'] = {'udid': simulator, 'device': pin['simulator_device'],
                                'runtime': runtimes[0], 'fresh_device': True}
        output(['xcrun', 'simctl', 'boot', simulator])
        output(['xcrun', 'simctl', 'bootstatus', simulator, '-b'], timeout=180)
        app = staged / 'CrispSorter.app'
        if bundle_identity(app, 'iphonesimulator')['sha256'] != pin['bundle_sha256']:
            raise RuntimeError('Staged application changed before installation')
        output(['xcrun', 'simctl', 'install', simulator, str(app)], timeout=120)
        installed = Path(output(['xcrun', 'simctl', 'get_app_container', simulator, pin['bundle_identifier'], 'app']))
        installed_hash = bundle_identity(installed, 'iphonesimulator')['sha256']
        receipt['installed_bundle_sha256_before_test'] = installed_hash
        if installed_hash != pin['bundle_sha256']:
            raise RuntimeError('Installed app differs from the immutable retained build')
        output(['xcodegen', 'generate', '--spec', str(HERE / 'project.yml')], cwd=HERE)
        with (staged / 'xcodebuild.log').open('w') as log:
            result = subprocess.run([
                'xcodebuild', 'test', '-project', str(HERE / 'OcrShellProbe.xcodeproj'),
                '-scheme', 'OcrShellProbe', '-configuration', 'Debug',
                '-destination', f'platform=iOS Simulator,id={simulator}',
                '-derivedDataPath', str(staged / 'DerivedData'),
                '-resultBundlePath', str(staged / 'ShellProbe.xcresult'),
                '-parallel-testing-enabled', 'NO', '-maximum-concurrent-test-simulator-destinations', '1',
                '-only-testing:ShellNavigationTests/ShellNavigationTests/testToolsOpensOcrWorkspace',
                'CODE_SIGNING_ALLOWED=NO',
            ], cwd=HERE, stdout=log, stderr=subprocess.STDOUT, timeout=600)
            test_exit = result.returncode
        receipt['xcodebuild_exit_code'] = test_exit
        runner_data = Path(output(['xcrun', 'simctl', 'get_app_container', simulator, RUNNER_PACKAGE, 'data']))
        evidence = runner_data / 'Documents/ocr-shell-probe'
        if not evidence.is_dir() or evidence.is_symlink():
            raise RuntimeError('The UI test did not preserve its navigation evidence')
        files = list(evidence.rglob('*'))
        if (len(files) > 40 or any(item.is_symlink() for item in files)
                or sum(item.stat().st_size for item in files if item.is_file()) > 80 * 1024 * 1024):
            raise RuntimeError('Unexpected UI-test evidence size or member type')
        shutil.copytree(evidence, staged / 'evidence')
        navigation = json.loads((staged / 'evidence/navigation.json').read_text())
        receipt['navigation'] = navigation
        after_hash = bundle_identity(installed, 'iphonesimulator')['sha256']
        receipt['installed_bundle_sha256_after_test'] = after_hash
        if after_hash != pin['bundle_sha256']:
            raise RuntimeError('Installed application bytes changed during the UI probe')
        if test_exit or navigation.get('status') != 'passed':
            raise RuntimeError(navigation.get('failure', 'The navigation test failed'))
        receipt['status'] = 'passed'
    except Exception as error:
        receipt['error'] = str(error)
    finally:
        if simulator:
            try:
                output(['xcrun', 'simctl', 'io', simulator, 'screenshot', str(staged / 'simulator-final.png')])
            except Exception as error:
                receipt['final_screenshot_error'] = str(error)
            try:
                with (staged / 'application.log').open('w') as log:
                    subprocess.run(['xcrun', 'simctl', 'spawn', simulator, 'log', 'show', '--last', '3m',
                                    '--style', 'compact', '--predicate', 'process == "CrispSorter"'],
                                   stdout=log, stderr=subprocess.STDOUT, timeout=45)
            except Exception as error:
                receipt['application_log_error'] = str(error)
            try:
                output(['xcrun', 'simctl', 'shutdown', simulator])
            except Exception as error:
                receipt['simulator_shutdown_error'] = str(error)
        result_bundle = staged / 'ShellProbe.xcresult'
        if result_bundle.exists():
            try:
                with (staged / 'attachment-export.log').open('w') as log:
                    exported = subprocess.run(['xcrun', 'xcresulttool', 'export', 'attachments',
                                               '--path', str(result_bundle), '--output-path', str(staged / 'attachments')],
                                              stdout=log, stderr=subprocess.STDOUT, timeout=60)
                receipt['attachment_export_exit_code'] = exported.returncode
            except Exception as error:
                receipt['attachment_export_error'] = str(error)
        (staged / 'probe-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
        print(json.dumps({'status': receipt['status'], 'error': receipt.get('error'),
                          'application_source': pin['application_source'], 'xcodebuild_exit_code': test_exit}))
    return 0 if receipt['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
