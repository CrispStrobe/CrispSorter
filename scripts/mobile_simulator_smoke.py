#!/usr/bin/env python3
"""Install and launch an actual mobile build on a selected, booted simulator.

This checks startup and captures a screen; it does not replace interactive file
picker, search, review or physical-device testing. Never clears simulator data.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import time
import zipfile

from ocr_mobile_build_receipt import bundle_identity

PACKAGE = 'com.crispstrobe.crispsorter'
MAX_DIAGNOSTIC_TEXT = 2 * 1024 * 1024


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=120, **kwargs).stdout.strip()


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def command_failure(error):
    """Preserve the actual failing command's output without changing its result."""
    if not isinstance(error, (subprocess.CalledProcessError, subprocess.TimeoutExpired)):
        return None
    def text(value):
        return (value.decode('utf-8', errors='replace') if isinstance(value, bytes) else value or '')[:65536]
    return {'command': error.cmd, 'returncode': getattr(error, 'returncode', None),
            'timeout_seconds': getattr(error, 'timeout', None),
            'stdout': text(error.stdout), 'stderr': text(error.stderr),
            'output_truncated': any(len(value or b'') > 65536 for value in (error.stdout, error.stderr))}


def android_failure_evidence(serial, output):
    """Observe only the explicitly selected emulator; never retry or clear data."""
    evidence = {'status': 'partial', 'budget_seconds': 30, 'commands': []}
    if not re.fullmatch(r'emulator-\d+', serial):
        return {**evidence, 'skipped': 'The selected target is not an emulator serial'}
    adb = ['adb', '-s', serial]
    deadline = time.monotonic() + evidence['budget_seconds']
    try:
        verified = subprocess.run(adb + ['shell', 'getprop', 'ro.kernel.qemu'],
                                  check=True, capture_output=True, text=True, timeout=5)
        if verified.stdout.strip() != '1':
            return {**evidence, 'skipped': 'The selected target did not identify itself as an emulator'}
    except Exception as error:
        return {**evidence, 'verification_error': str(error)}
    commands = [
        ('launcher-resolution.txt', ['shell', 'cmd', 'package', 'resolve-activity', '--brief',
                                    '-a', 'android.intent.action.MAIN', '-c', 'android.intent.category.LAUNCHER', '-p', PACKAGE]),
        ('package-manager.txt', ['shell', 'dumpsys', 'package', PACKAGE]),
        ('activity-state.txt', ['shell', 'dumpsys', 'activity', 'activities']),
        ('process-id.txt', ['shell', 'pidof', PACKAGE]),
        ('logcat.txt', ['logcat', '-d', '-b', 'all', '-v', 'threadtime', '-t', '1000']),
    ]
    for name, arguments in commands:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            evidence['budget_exhausted'] = True
            break
        record = {'file': name, 'command': adb + arguments}
        evidence['commands'].append(record)
        try:
            result = subprocess.run(record['command'], capture_output=True, text=True,
                                    timeout=min(8, remaining), check=False)
            stdout, stderr = result.stdout or '', result.stderr or ''
            content = 'STDOUT\n' + stdout[:MAX_DIAGNOSTIC_TEXT] + '\nSTDERR\n' + stderr[:65536]
            (output / name).write_text(content)
            record.update(returncode=result.returncode, sha256=digest(output / name),
                          output_truncated=len(stdout) > MAX_DIAGNOSTIC_TEXT or len(stderr) > 65536)
        except Exception as error:
            record.update(error=str(error), failed_command=command_failure(error))
    remaining = deadline - time.monotonic()
    if remaining > 0:
        try:
            result = subprocess.run(adb + ['exec-out', 'screencap', '-p'], check=True,
                                    capture_output=True, timeout=min(8, remaining))
            if not result.stdout.startswith(b'\x89PNG\r\n\x1a\n') or len(result.stdout) > 16 * 1024 * 1024:
                raise RuntimeError('Failure screenshot is not a bounded PNG')
            (output / 'failure-screen.png').write_bytes(result.stdout)
            evidence['screenshot_sha256'] = digest(output / 'failure-screen.png')
        except Exception as error:
            evidence['screenshot_error'] = str(error)
    if not evidence.get('budget_exhausted'):
        evidence['status'] = 'collected'
    return evidence


def android_launcher(adb):
    """Resolve the installed package's actual launcher, then start it explicitly.

    Launcher intent filters need not contain CATEGORY_DEFAULT, which an implicit
    ActivityManager start may require even when the package resolver finds them.
    """
    result = run(adb + ['shell', 'cmd', 'package', 'resolve-activity', '--brief',
                        '-a', 'android.intent.action.MAIN', '-c', 'android.intent.category.LAUNCHER', '-p', PACKAGE])
    pattern = re.compile(rf'{re.escape(PACKAGE)}/[A-Za-z0-9_.$]+')
    components = [line.strip() for line in result.splitlines() if pattern.fullmatch(line.strip())]
    if len(components) != 1:
        raise RuntimeError('Could not resolve exactly one launcher activity for the installed app: ' + result[:1000])
    return components[0]


def android(serial, apk, output, wait):
    if not re.fullmatch(r'emulator-\d+', serial):
        raise ValueError('Android smoke requires an explicitly selected emulator serial')
    adb = ['adb', '-s', serial]
    if run(adb + ['shell', 'getprop', 'ro.kernel.qemu']) != '1':
        raise ValueError('The selected Android target is not an emulator')
    abis = run(adb + ['shell', 'getprop', 'ro.product.cpu.abilist']).split(',')
    sdk = os.environ.get('ANDROID_HOME') or os.environ.get('ANDROID_SDK_ROOT')
    tools = sorted((Path(sdk) / 'build-tools').glob('*/aapt'),
                   key=lambda path: tuple(int(n) for n in re.findall(r'\d+', path.parent.name))) if sdk else []
    aapt = str(tools[-1]) if tools else shutil.which('aapt')
    if not aapt:
        raise RuntimeError('Android SDK build-tools aapt is required to verify the APK identity')
    badging = run([aapt, 'dump', 'badging', str(apk)])
    (output / 'apk-badging.txt').write_text(badging + '\n')
    package = re.search(r"^package: name='([^']+)'", badging, re.M)
    if not package or package.group(1) != PACKAGE:
        raise ValueError('APK does not have the expected CrispSorter application ID')
    with zipfile.ZipFile(apk) as archive:
        apk_abis = {name.split('/')[1] for name in archive.namelist()
                    if name.startswith('lib/') and name.endswith('.so') and len(name.split('/')) == 3}
        if not apk_abis.intersection(abis):
            raise ValueError(f'APK ABIs {sorted(apk_abis)} are not supported by emulator ABIs {abis}')
        if archive.testzip() is not None:
            raise ValueError('APK archive integrity failed')
    run(adb + ['install', '-r', str(apk)])
    installed = run(adb + ['shell', 'pm', 'path', PACKAGE]).splitlines()
    if len(installed) != 1 or not re.fullmatch(r'package:/data/app/[A-Za-z0-9_./+~=-]+/base\.apk', installed[0]):
        raise RuntimeError('Expected one installed base APK in the application directory')
    installed_hash = run(adb + ['shell', 'sha256sum', installed[0][8:]]).split()[0]
    if installed_hash != digest(apk):
        raise RuntimeError('Installed application differs from the supplied APK')
    (output / 'installed-identity.json').write_text(json.dumps({
        'package': PACKAGE, 'target': serial, 'apk_sha256': digest(apk),
        'installed_apk_sha256': installed_hash, 'supported_abis': abis,
        'scope': 'Installed package byte identity before launch; no successful startup claim.',
    }, indent=2) + '\n')
    run(adb + ['shell', 'am', 'force-stop', PACKAGE])
    component = android_launcher(adb)
    launch = run(adb + ['shell', 'am', 'start', '-W', '-a', 'android.intent.action.MAIN',
                        '-c', 'android.intent.category.LAUNCHER', '-n', component])
    (output / 'launch.txt').write_text(launch + '\n')
    if 'Error:' in launch or 'Status: ok' not in launch:
        raise RuntimeError('Android activity did not start: ' + launch)
    initial = set(run(adb + ['shell', 'pidof', PACKAGE]).split())
    time.sleep(wait)
    pid = run(adb + ['shell', 'pidof', PACKAGE])
    if not initial.intersection(pid.split()):
        raise RuntimeError('The launched Android process exited during the startup check')
    foreground = run(adb + ['shell', 'dumpsys', 'activity', 'activities'])
    if PACKAGE not in '\n'.join(line for line in foreground.splitlines()
                                  if 'mResumedActivity' in line or 'topResumedActivity' in line):
        raise RuntimeError('App is not the resumed Android activity after startup')
    shot = subprocess.run(adb + ['exec-out', 'screencap', '-p'], check=True, capture_output=True, timeout=30).stdout
    if not shot.startswith(b'\x89PNG\r\n\x1a\n'):
        raise RuntimeError('Android screenshot was not PNG')
    (output / 'screen.png').write_bytes(shot)
    return {'target': serial, 'package': PACKAGE, 'pid': pid, 'apk_sha256': digest(apk),
            'installed_apk_sha256': installed_hash, 'supported_abis': abis}


def ios(udid, app, output, wait):
    listing = json.loads(run(['xcrun', 'simctl', 'list', 'devices', '--json']))
    devices = [d for group in listing['devices'].values() for d in group if d['udid'] == udid]
    if len(devices) != 1 or devices[0]['state'] != 'Booted':
        raise ValueError('Select an explicitly identified, booted iOS simulator')
    if not app.is_dir() or app.suffix != '.app':
        raise ValueError('iOS simulator smoke requires a simulator .app directory, not a device IPA')
    info = plistlib.loads((app / 'Info.plist').read_bytes())
    if info.get('CFBundleIdentifier') != PACKAGE or info.get('DTPlatformName') != 'iphonesimulator':
        raise ValueError('App bundle must be CrispSorter built for the iOS simulator')
    run(['xcrun', 'simctl', 'install', udid, str(app)])
    source_identity = bundle_identity(app, 'iphonesimulator')
    installed_app = Path(run(['xcrun', 'simctl', 'get_app_container', udid, PACKAGE, 'app']))
    installed_identity = bundle_identity(installed_app, 'iphonesimulator')
    if installed_identity != source_identity:
        raise RuntimeError('Installed simulator bundle differs from the supplied application')
    launch = run(['xcrun', 'simctl', 'launch', '--terminate-running-process', udid, PACKAGE])
    match = re.search(r':\s*(\d+)\s*$', launch)
    if not match:
        raise RuntimeError('simctl did not report the launched app PID: ' + launch)
    pid = int(match.group(1))
    time.sleep(wait)
    os.kill(pid, 0)  # Simulator apps are host processes; fail if it exited.
    run(['xcrun', 'simctl', 'io', udid, 'screenshot', str(output / 'screen.png')])
    if not (output / 'screen.png').read_bytes().startswith(b'\x89PNG\r\n\x1a\n'):
        raise RuntimeError('iOS screenshot was not PNG')
    return {'target': udid, 'package': PACKAGE, 'pid': pid,
            'source_bundle_sha256': source_identity['sha256'], 'installed_bundle_sha256': installed_identity['sha256'],
            'bundle_version': info.get('CFBundleShortVersionString'), 'simulator_name': devices[0]['name']}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--platform', required=True, choices=['android', 'ios'])
    p.add_argument('--target', required=True, help='Android emulator serial or iOS simulator UDID')
    p.add_argument('--artifact', required=True, type=Path, help='signed APK or iOS simulator .app')
    p.add_argument('--output', required=True, type=Path, help='new directory for screenshot and receipt')
    p.add_argument('--wait-seconds', type=int, default=10)
    args = p.parse_args()
    if not 1 <= args.wait_seconds <= 60:
        p.error('--wait-seconds must be between 1 and 60')
    args.output.mkdir(parents=True, exist_ok=False)
    result = {'status': 'running', 'platform': args.platform, 'target': args.target,
              'scope': 'App identity, startup and screenshot only; user flows and physical devices are not covered'}
    try:
        result.update(android(args.target, args.artifact.resolve(), args.output, args.wait_seconds)
                      if args.platform == 'android' else ios(args.target, args.artifact.resolve(), args.output, args.wait_seconds))
        result.update(status='passed', screenshot_sha256=digest(args.output / 'screen.png'))
    except Exception as error:
        result.update(status='failed', error=f'{type(error).__name__}: {error}')
        failure = command_failure(error)
        if failure:
            result['failed_command'] = failure
        # Preserve the original failure before any optional diagnostic command.
        (args.output / 'receipt.json').write_text(json.dumps(result, indent=2) + '\n')
        if args.platform == 'android':
            try:
                result['failure_evidence'] = android_failure_evidence(args.target, args.output)
            except Exception as diagnostic_error:
                result['failure_evidence_error'] = str(diagnostic_error)
        raise
    finally:
        (args.output / 'receipt.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
