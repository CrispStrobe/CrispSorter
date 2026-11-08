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

PACKAGE = 'com.crispstrobe.crispsorter'


def run(args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=120, **kwargs).stdout.strip()


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


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
    package = re.search(r"^package: name='([^']+)'", run([aapt, 'dump', 'badging', str(apk)]), re.M)
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
    run(adb + ['shell', 'am', 'force-stop', PACKAGE])
    launch = run(adb + ['shell', 'am', 'start', '-W', '-a', 'android.intent.action.MAIN',
                        '-c', 'android.intent.category.LAUNCHER', '-p', PACKAGE])
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
    return {'target': serial, 'package': PACKAGE, 'pid': pid, 'apk_sha256': digest(apk), 'supported_abis': abis}


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
    result = android(args.target, args.artifact.resolve(), args.output, args.wait_seconds) if args.platform == 'android' else ios(args.target, args.artifact.resolve(), args.output, args.wait_seconds)
    result.update(platform=args.platform, screenshot_sha256=digest(args.output / 'screen.png'),
                  scope='App installed, launched, remained running, and screenshot captured; user flows and physical devices are not covered')
    (args.output / 'receipt.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
