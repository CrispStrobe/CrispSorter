#!/usr/bin/env python3
"""Run a small XCUITest target against the already-built, verified application."""
import argparse
import hashlib
import json
import os
import platform
import re
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


def simulator_inventory(kind, receipt):
    # A fresh hosted macOS image can spend more than a minute starting
    # CoreSimulator and mounting its existing runtimes. Retry that discovery
    # once, with both attempts bounded and retained; do not relax UI assertions.
    attempts = receipt.setdefault('simulator_inventory_attempts', [])
    for attempt, timeout in enumerate((90, 180), start=1):
        try:
            value = json.loads(output(['xcrun', 'simctl', 'list', kind, '--json'], timeout=timeout))
            attempts.append({'kind': kind, 'attempt': attempt, 'timeout_seconds': timeout, 'status': 'passed'})
            return value
        except subprocess.TimeoutExpired:
            attempts.append({'kind': kind, 'attempt': attempt, 'timeout_seconds': timeout, 'status': 'timed_out'})
    raise RuntimeError(f'CoreSimulator {kind} discovery timed out on both bounded startup attempts')


def validate_build(app, build, application_source):
    """Bind this UI run to the actual compiled bundle, never just its pathname."""
    if not re.fullmatch(r'[0-9a-f]{40}', application_source):
        raise ValueError('The application source must be an exact commit SHA')
    if (build.get('kind') != 'ocr-mobile-app-build' or build.get('platform') != 'ios-simulator'
            or build.get('package') != 'com.crispstrobe.crispsorter'
            or build.get('sorter_commit') != application_source
            or build.get('application_source_clean') is not True):
        raise ValueError('The build receipt does not identify this clean simulator application source')
    actual = bundle_identity(app, 'iphonesimulator')
    if actual != build.get('artifact'):
        raise ValueError('The compiled application differs from its build receipt')
    return actual


def prepare_inputs(args):
    if args.staged:
        if args.app or args.build_receipt or args.output:
            raise ValueError('Use either retained --staged input or compiled --app input')
        staged = args.staged.resolve()
        pin = json.loads((staged / 'input-receipt.json').read_text())['pin']
        app = staged / 'CrispSorter.app'
        if args.application_source and args.application_source != pin['application_source']:
            raise ValueError('The requested source differs from the retained artifact source')
        return staged, app, pin
    if not (args.app and args.build_receipt and args.output and args.application_source):
        raise ValueError('Compiled input requires --app, --build-receipt, --output and --application-source')
    # Keep a symlink visible to the bundle validator rather than resolving it.
    app = args.app.absolute()
    build = json.loads(args.build_receipt.read_text())
    identity = validate_build(app, build, args.application_source)
    staged = args.output.resolve()
    if staged.exists() and any(staged.iterdir()):
        raise ValueError('Choose a new or empty navigation evidence directory')
    staged.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(args.build_receipt, staged / 'build-receipt.json')
    pin = {
        'application_source': args.application_source,
        'bundle_identifier': identity['bundle_identifier'], 'bundle_sha256': identity['sha256'],
        'build_receipt_sha256': digest(args.build_receipt),
        'simulator_runtime': '18.5', 'simulator_device': 'iPhone 16 Pro',
    }
    (staged / 'input-receipt.json').write_text(json.dumps({
        'schema_version': 1, 'kind': 'compiled-ios-shell-input', 'pin': pin,
        'application_rebuilt_by_probe': False,
        'verified_bundle': identity,
    }, indent=2) + '\n')
    return staged, app, pin


def select_simulator(pin, receipt, requested):
    runtime_list = simulator_inventory('runtimes', receipt)
    if requested:
        devices = simulator_inventory('devices', receipt)
        matches = [(runtime, device) for runtime, items in devices['devices'].items()
                   for device in items if device['udid'] == requested and device.get('isAvailable')]
        if len(matches) != 1:
            raise ValueError('The requested simulator is unavailable or ambiguous')
        runtime_id, device = matches[0]
        runtimes = [item for item in runtime_list['runtimes']
                    if item['identifier'] == runtime_id and item.get('isAvailable')]
        if len(runtimes) != 1 or '.iOS-' not in runtime_id:
            raise ValueError('The requested simulator does not have an available iOS runtime')
        if pin.get('artifact_id') and (runtimes[0]['version'] != pin['simulator_runtime']
                                      or device['name'] != pin['simulator_device']):
            raise ValueError('The requested simulator differs from the retained artifact qualification target')
        simulator, name = requested, device['name']
        needs_boot = device['state'] != 'Booted'
    else:
        runtimes = [item for item in runtime_list['runtimes']
                    if item.get('isAvailable') and item.get('version') == pin['simulator_runtime']
                    and '.iOS-' in item['identifier']]
        devices = simulator_inventory('devicetypes', receipt)
        types = [item for item in devices['devicetypes'] if item['name'] == pin['simulator_device']]
        if len(runtimes) != 1 or len(types) != 1:
            raise RuntimeError('The exact retained-build simulator device/runtime is unavailable')
        simulator = output(['xcrun', 'simctl', 'create', 'OCR shell navigation probe',
                            types[0]['identifier'], runtimes[0]['identifier']])
        name, needs_boot = pin['simulator_device'], True
    receipt['simulator'] = {
        'udid': simulator, 'device': name, 'fresh_device': requested is None,
        'owned_by_probe': requested is None,
        'runtime': {key: runtimes[0][key] for key in ('identifier', 'name', 'version', 'buildversion')
                    if key in runtimes[0]},
    }
    return simulator, needs_boot


def collect_runner_evidence(simulator, staged):
    """Recover Swift-written reports even when xcodebuild did not return normally."""
    destination = staged / 'evidence'
    if destination.exists():
        return False
    runner_data = Path(output(['xcrun', 'simctl', 'get_app_container', simulator, RUNNER_PACKAGE, 'data'], timeout=30))
    evidence = runner_data / 'Documents/ocr-shell-probe'
    if not evidence.is_dir() or evidence.is_symlink():
        raise RuntimeError('The UI test did not preserve its navigation evidence')
    files = list(evidence.rglob('*'))
    if (len(files) > 40 or any(item.is_symlink() for item in files)
            or sum(item.stat().st_size for item in files if item.is_file()) > 80 * 1024 * 1024):
        raise RuntimeError('Unexpected UI-test evidence size or member type')
    shutil.copytree(evidence, destination)
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--staged', type=Path, help='Verified immutable artifact directory from stage.py')
    parser.add_argument('--app', type=Path, help='Already compiled simulator .app; never rebuilt by this probe')
    parser.add_argument('--build-receipt', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--application-source', help='Exact application commit, required for --app')
    parser.add_argument('--simulator', help='Reuse this caller-owned simulator without shutting it down')
    args = parser.parse_args()
    staged, app, pin = prepare_inputs(args)
    receipt = {
        'schema_version': 1, 'kind': 'ios-shell-probe-run', 'status': 'failed', 'phase': 'prepared',
        'recorded_at': datetime.now(timezone.utc).isoformat(),
        'application_source': pin['application_source'], 'application_rebuilt': False,
        'artifact_id': pin.get('artifact_id'), 'artifact_sha256': pin.get('artifact_sha256'),
        'build_receipt_sha256': pin.get('build_receipt_sha256'),
        'source_bundle_sha256': pin['bundle_sha256'],
        'probe_source': output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD']),
        'probe_files': {str(path.relative_to(ROOT)): digest(path)
                        for path in [*sorted(HERE.rglob('*.py')), *sorted(HERE.rglob('*.swift')),
                                     HERE / 'project.yml', HERE / 'artifact.json']},
        'github': {key: os.environ.get(key) for key in ('GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_SHA')},
        'scope': 'Compiled application byte identity plus simulator UI navigation; no native rebuild, OCR accuracy test, or physical phone test.',
    }
    simulator = None
    installed = None
    test_exit = None
    def save_receipt():
        (staged / 'probe-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    save_receipt()
    try:
        if platform.system() != 'Darwin' or platform.machine() != 'arm64':
            raise RuntimeError('This retained app requires an arm64 macOS simulator host')
        receipt['xcode_version'] = output(['xcodebuild', '-version'])
        receipt['xcodegen_version'] = output(['xcodegen', '--version'])
        receipt['simulator_sdk'] = output(['xcrun', '--sdk', 'iphonesimulator', '--show-sdk-version'])
        receipt['phase'] = 'simulator-discovery'
        save_receipt()
        simulator, needs_boot = select_simulator(pin, receipt, args.simulator)
        save_receipt()
        if needs_boot:
            output(['xcrun', 'simctl', 'boot', simulator])
        output(['xcrun', 'simctl', 'bootstatus', simulator, '-b'], timeout=180)
        if bundle_identity(app, 'iphonesimulator')['sha256'] != pin['bundle_sha256']:
            raise RuntimeError('Staged application changed before installation')
        output(['xcrun', 'simctl', 'install', simulator, str(app)], timeout=120)
        installed = Path(output(['xcrun', 'simctl', 'get_app_container', simulator, pin['bundle_identifier'], 'app']))
        installed_hash = bundle_identity(installed, 'iphonesimulator')['sha256']
        receipt['installed_bundle_sha256_before_test'] = installed_hash
        receipt['phase'] = 'application-installed'
        save_receipt()
        if installed_hash != pin['bundle_sha256']:
            raise RuntimeError('Installed app differs from the immutable retained build')
        output(['xcodegen', 'generate', '--spec', str(HERE / 'project.yml')], cwd=HERE)
        receipt['phase'] = 'ui-test'
        save_receipt()
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
        receipt['phase'] = 'collecting-evidence'
        save_receipt()
        collect_runner_evidence(simulator, staged)
        report = staged / 'evidence/navigation.json'
        if not report.is_file():
            raise RuntimeError(f'UI test exited {test_exit} without navigation.json; inspect xcodebuild.log and retained attachments')
        navigation = json.loads(report.read_text())
        receipt['navigation'] = navigation
        if test_exit or navigation.get('status') != 'passed':
            raise RuntimeError(navigation.get('failure', 'The navigation test failed'))
        receipt['status'] = 'passed'
        receipt['phase'] = 'complete'
    except Exception as error:
        receipt['error'] = str(error)
    finally:
        if simulator and not (staged / 'evidence').exists():
            try:
                receipt['recovered_runner_evidence'] = collect_runner_evidence(simulator, staged)
            except Exception as error:
                receipt['runner_evidence_recovery_error'] = str(error)
        if installed:
            try:
                after_hash = bundle_identity(installed, 'iphonesimulator')['sha256']
                receipt['installed_bundle_sha256_after_test'] = after_hash
                if after_hash != pin['bundle_sha256']:
                    raise RuntimeError('Installed application bytes changed during the UI probe')
            except Exception as error:
                receipt['status'] = 'failed'
                receipt['installed_identity_error'] = str(error)
        save_receipt()
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
            if args.simulator is None:
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
        save_receipt()
        print(json.dumps({'status': receipt['status'], 'error': receipt.get('error'),
                          'application_source': pin['application_source'], 'xcodebuild_exit_code': test_exit}))
    return 0 if receipt['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
