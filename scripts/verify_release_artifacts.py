#!/usr/bin/env python3
"""Inspect final packages and smoke desktop executables outside the build tree."""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


@contextlib.contextmanager
def hide_build_libraries(root):
    sibling = root / '_sibling'
    hidden = root / '_sibling.qualification-hidden'
    if hidden.exists():
        raise RuntimeError('Previous library isolation was not restored')
    if sibling.exists():
        sibling.rename(hidden)
    try:
        yield
    finally:
        if hidden.exists():
            hidden.rename(sibling)


def verify_mac_app(app):
    """Verify a stapled Developer ID app extracted from the actual release asset."""
    team = os.environ.get('APPLE_TEAM_ID', '')
    fingerprint = os.environ.get('DEVELOPER_ID_SHA1', '').upper()
    if not re.fullmatch(r'[A-Z0-9]{10}', team) or not re.fullmatch(r'[A-F0-9]{40}', fingerprint):
        raise ValueError('Expected Developer ID team and certificate fingerprint are required')
    subprocess.run(['codesign', '--verify', '--deep', '--strict', '--verbose=2', str(app)], check=True)
    details = subprocess.check_output(['codesign', '-dv', '--verbose=4', str(app)], stderr=subprocess.STDOUT, text=True)
    if not all(part in details for part in ['TeamIdentifier=' + team, 'Authority=Developer ID Application:', 'Timestamp=', 'runtime']):
        raise ValueError('App lacks Developer ID, expected team, timestamp, or hardened runtime')
    with tempfile.TemporaryDirectory(prefix='crispsorter-cert-') as directory:
        subprocess.run(['codesign', '-d', '--extract-certificates', str(app)], cwd=directory, check=True)
        certificate = Path(directory) / 'codesign0'
        output = subprocess.check_output(['openssl', 'x509', '-inform', 'DER', '-in', str(certificate),
                                          '-noout', '-fingerprint', '-sha1'], text=True)
        actual = output.strip().split('=', 1)[-1].replace(':', '').upper()
        if actual != fingerprint:
            raise ValueError('Developer ID signing certificate fingerprint does not match')
    subprocess.run(['xcrun', 'stapler', 'validate', str(app)], check=True)
    subprocess.run(['spctl', '--assess', '--type', 'execute', '--verbose=2', str(app)], check=True)
    match = re.search(r'^CDHash=([0-9a-fA-F]+)$', details, re.MULTILINE)
    if not match:
        raise ValueError('App signature has no CDHash')
    return match.group(1).lower()


def verify_mac_dmg(artifact):
    subprocess.run(['codesign', '--verify', '--verbose=2', str(artifact)], check=True)
    subprocess.run(['xcrun', 'stapler', 'validate', str(artifact)], check=True)
    with tempfile.TemporaryDirectory(prefix='crispsorter-dmg-') as directory:
        mount = Path(directory) / 'mount'
        mount.mkdir()
        subprocess.run(['hdiutil', 'attach', '-readonly', '-nobrowse', '-mountpoint', str(mount), str(artifact)], check=True)
        try:
            apps = list(mount.glob('*.app'))
            if len(apps) != 1:
                raise ValueError('DMG must contain exactly one application')
            cdhash = verify_mac_app(apps[0])
            info = plistlib.loads((apps[0] / 'Contents/Info.plist').read_bytes())
            return {'signature_verification': 'Developer ID signature, hardened runtime, timestamp, stapled ticket and Gatekeeper assessment passed',
                    'bundle_id': info['CFBundleIdentifier'], 'version': info['CFBundleShortVersionString'], 'signature_cdhash': cdhash}
        finally:
            subprocess.run(['hdiutil', 'detach', str(mount)], check=True)


def check_ocr_worker(command, destination, env):
    """Exercise the packaged worker entry point without initializing OCR models."""
    worker_flag = '--crispsorter-ocr-worker'
    missing = subprocess.run([*command, worker_flag], cwd=destination, env=env,
                             stdin=subprocess.DEVNULL, text=True, capture_output=True, timeout=20)
    if missing.returncode != 1 or 'OCR worker requires request and response paths' not in missing.stderr:
        raise ValueError('Packaged OCR worker did not reject missing paths: '
                         + missing.stderr[-4000:] + missing.stdout[-4000:])
    with tempfile.TemporaryDirectory(prefix='ocr-worker-smoke-', dir=destination) as work:
        request = Path(work) / 'invalid-request.json'
        response = Path(work) / 'response.json'
        request.write_text('not json', encoding='utf-8')
        invalid = subprocess.run([*command, worker_flag, str(request), str(response)],
                                 cwd=destination, env=env, stdin=subprocess.DEVNULL,
                                 text=True, capture_output=True, timeout=20)
        if invalid.returncode != 1 or 'parse OCR worker request' not in invalid.stderr or response.exists():
            raise ValueError('Packaged OCR worker did not reject malformed input without a response: '
                             + invalid.stderr[-4000:] + invalid.stdout[-4000:])
    return {'arguments': [worker_flag],
            'result': 'missing paths and malformed request rejected before model initialization',
            'exit_codes': [missing.returncode, invalid.returncode]}


def inspect_package(platform, artifact, destination):
    mac_cdhash = None
    if artifact.suffix == '.deb':
        subprocess.run(['dpkg-deb', '-x', str(artifact), str(destination)], check=True)
        executable = next((destination / 'usr/bin').glob('*'))
    elif artifact.suffix in ('.zip', '.apk', '.ipa'):
        with zipfile.ZipFile(artifact) as archive:
            archive.extractall(destination)
        if platform == 'windows-x86_64':
            executable = next(destination.rglob('crispsorter.exe'))
            if os.environ.get('CRISPSORTER_REQUIRE_DESKTOP_SIGNING') == '1':
                script = Path(__file__).resolve().with_name('sign_windows_release.ps1')
                subprocess.run(['pwsh', '-NoProfile', '-File', str(script), '-Stage', str(executable.parent), '-VerifyOnly'], check=True)
        elif platform == 'android-arm64':
            libraries = list(destination.glob('lib/arm64-v8a/*.so'))
            if not libraries or not (destination / 'AndroidManifest.xml').is_file():
                raise ValueError('APK lacks the arm64 application or manifest')
            from sign_android_apk import signer_path
            signature = subprocess.check_output([signer_path(None), 'verify', '--verbose', '--print-certs', str(artifact)], text=True)
            return {'inspection': 'APK arm64 native libraries, manifest and verified signature present',
                    'native_libraries': [p.name for p in libraries], 'signature_verification': signature,
                    'device_execution': 'not exercised by signature/archive inspection'}
        else:
            app = next((destination / 'Payload').glob('*.app'))
            info = plistlib.loads((app / 'Info.plist').read_bytes())
            executable = app / info['CFBundleExecutable']
            if not executable.is_file() or executable.stat().st_size == 0:
                raise ValueError('IPA lacks its application executable')
            return {'inspection': 'IPA application executable and metadata present', 'bundle_id': info['CFBundleIdentifier'],
                    'version': info['CFBundleShortVersionString'], 'embedded_profile': (app / 'embedded.mobileprovision').is_file(),
                    'device_execution': 'not exercised by archive inspection'}
    else:
        if platform == 'macos-arm64':
            # BSD tar restores Apple metadata used by stapled tickets.
            subprocess.run(['tar', '-xzf', str(artifact), '-C', str(destination)], check=True)
        else:
            with tarfile.open(artifact) as archive:
                archive.extractall(destination, filter='data')
        app = next(destination.glob('*.app'))
        if platform == 'macos-arm64' and os.environ.get('CRISPSORTER_REQUIRE_DESKTOP_SIGNING') == '1':
            mac_cdhash = verify_mac_app(app)
        info = plistlib.loads((app / 'Contents/Info.plist').read_bytes()) if (app / 'Contents/Info.plist').exists() else plistlib.loads((app / 'Info.plist').read_bytes())
        executable = app / ('Contents/MacOS' if platform == 'macos-arm64' else '') / info['CFBundleExecutable']
        if platform == 'ios-arm64':
            if not executable.is_file():
                raise ValueError('Unsigned iOS archive lacks executable')
            return {'inspection': 'unsigned iOS app archive present', 'device_execution': 'requires signing and installation'}
    if not executable.is_file():
        raise ValueError('Packaged executable missing')
    env = os.environ.copy()
    for key in ['LD_LIBRARY_PATH', 'DYLD_LIBRARY_PATH', 'DYLD_FALLBACK_LIBRARY_PATH']:
        env.pop(key, None)
    checks = []
    launcher = [str(executable)]
    if platform == 'linux-x86_64':
        headers = subprocess.check_output(['readelf', '-l', str(executable)], text=True)
        interpreter = re.search(r'Requesting program interpreter:\s*([^\]]+)', headers)
        if not interpreter:
            raise ValueError('Packaged ELF lacks its interpreter')
        # A global ggml in ld.so.cache can conceal a broken package, as the
        # development host preflight demonstrated. Use only packaged rpaths
        # and standard distribution library directories for the smoke.
        launcher = [interpreter.group(1).strip(), '--inhibit-cache', '--library-path',
                    '/lib/x86_64-linux-gnu:/usr/lib/x86_64-linux-gnu:/lib64:/usr/lib64', str(executable)]
    for arguments in [['--format', 'json', 'version'], ['--format', 'json', 'doctor']]:
        result = subprocess.run([*launcher, *arguments], cwd=destination, env=env, text=True, capture_output=True, timeout=90, check=True)
        payload = json.loads(result.stdout)
        checks.append({'arguments': arguments, 'result': payload, 'stderr': result.stderr[-4000:]})
    # The worker launches current_exe(), so test the packaged executable
    # directly rather than the Linux interpreter wrapper used for CLI smokes.
    checks.append(check_ocr_worker([str(executable)], destination, env))
    # Use the real executable for the OCR probe: invoking ld.so explicitly
    # changes current_exe(), which would invalidate resource-dir discovery.
    fixture = Path(__file__).resolve().parents[1] / 'tests/quality/ocr-task.pdf'
    arguments = ['ocr', str(fixture), '--engine', 'dbnet_trocr', '--cleanup', 'false', '--render', 'hocr', '--min-confidence', '0', '--min-chars', '0']
    result = subprocess.run([str(executable), *arguments], cwd=destination, env=env, text=True, capture_output=True, timeout=600, check=True)
    text = result.stdout.lower()
    if 'ocr_page' not in text or not all(word in text for word in ['cobalt', 'warranty', '2031']):
        raise ValueError('Packaged scanned-PDF OCR missed its known evidence: ' + result.stdout[-4000:] + result.stderr[-4000:])
    checks.append({'arguments': arguments, 'result': 'image-only PDF rasterized and expected text retained',
                   'fixture_sha256': sha(fixture), 'stdout_sha256': hashlib.sha256(result.stdout.encode()).hexdigest(),
                   'stderr': result.stderr[-4000:]})
    result = {'inspection': 'packaged executable starts with build-tree libraries hidden', 'checks': checks}
    if mac_cdhash:
        result['signature_cdhash'] = mac_cdhash
        result['bundle_id'] = info['CFBundleIdentifier']
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', required=True, choices=['linux-x86_64', 'windows-x86_64', 'macos-arm64', 'android-arm64', 'ios-arm64'])
    args = parser.parse_args()
    root = Path.cwd()
    patterns = {
        'linux-x86_64': ['target/**/bundle/deb/*.deb'],
        'windows-x86_64': ['CrispSorter_*windows-portable*.zip'],
        'macos-arm64': ['target/**/bundle/macos/CrispSorter*.app.tar.gz', 'target/**/bundle/dmg/*.dmg'],
        'android-arm64': ['CrispSorter_*android*.apk'],
        'ios-arm64': ['CrispSorter_*ios*.ipa', 'CrispSorter_*ios*.app.tar.gz'],
    }
    artifacts = sorted({p.resolve() for pattern in patterns[args.platform] for p in root.glob(pattern)})
    if not artifacts:
        raise RuntimeError('No final artifacts found for ' + args.platform)
    require_signing = os.environ.get('CRISPSORTER_REQUIRE_DESKTOP_SIGNING') == '1'
    if require_signing and args.platform == 'macos-arm64':
        if sum(p.suffix == '.dmg' for p in artifacts) != 1 or sum(p.name.endswith('.app.tar.gz') for p in artifacts) != 1:
            raise ValueError('Exactly one final DMG and one app archive are required')
    if require_signing and args.platform == 'windows-x86_64' and len(artifacts) != 1:
        raise ValueError('Exactly one final Windows ZIP is required')
    if require_signing and any('-unsigned' in p.name for p in artifacts):
        raise ValueError('Unsigned-labeled artifact cannot satisfy the publication gate')
    receipt = {'schema': 'crispsorter-release-receipt-v1', 'platform': args.platform,
               'app_source': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
               'version': json.loads((root / 'src-tauri/tauri.conf.json').read_text())['version'],
               'runtime_lock': json.loads((root / 'scripts/crispembed-runtime-lock.json').read_text()),
               'native_crispembed_included': args.platform in ['linux-x86_64', 'windows-x86_64', 'macos-arm64'], 'artifacts': []}
    receipt['toolchains'] = {tool: subprocess.check_output([tool, '--version'], text=True).strip() for tool in ['rustc', 'cargo', 'node']}
    receipt['dependency_sources'] = {name: subprocess.check_output(['git', '-C', str(root / '_sibling' / name), 'rev-parse', 'HEAD'], text=True).strip()
                                     for name in ['CrispEmbed', 'CrispASR', 'crisp-docx'] if (root / '_sibling' / name).is_dir()}
    with hide_build_libraries(root):
        for artifact in artifacts:
            entry = {'filename': artifact.name, 'bytes': artifact.stat().st_size, 'sha256': sha(artifact)}
            if artifact.suffix != '.dmg':
                with tempfile.TemporaryDirectory(prefix='crispsorter-packaged-') as work:
                    entry.update(inspect_package(args.platform, artifact, Path(work)))
                    if 'checks' in entry and entry['checks'][0]['result']['version'] != receipt['version']:
                        raise ValueError('Packaged executable reports a different version')
            else:
                entry['inspection'] = 'checksum; executable smoke uses the app archive'
                if require_signing and args.platform == 'macos-arm64':
                    entry.update(verify_mac_dmg(artifact))
                    if entry['version'] != receipt['version']:
                        raise ValueError('DMG application reports a different version')
            if require_signing and args.platform == 'windows-x86_64':
                entry['signature_verification'] = 'Extracted application passed expected trusted, timestamped Authenticode verification'
            if require_signing and args.platform == 'macos-arm64' and artifact.suffix != '.dmg':
                entry['signature_verification'] = 'Extracted app passed Developer ID, hardened runtime, timestamp, stapled ticket and Gatekeeper checks'
            if require_signing and args.platform == 'macos-arm64' and entry.get('bundle_id') != 'com.crispstrobe.crispsorter':
                raise ValueError('Unexpected macOS application bundle ID')
            receipt['artifacts'].append(entry)
    if require_signing and args.platform == 'macos-arm64':
        hashes = {entry.get('signature_cdhash') for entry in receipt['artifacts']}
        if None in hashes or len(hashes) != 1:
            raise ValueError('DMG and archive contain different signed applications')
    if not any('checks' in a for a in receipt['artifacts']) and args.platform in ['linux-x86_64', 'windows-x86_64', 'macos-arm64']:
        raise RuntimeError('Desktop package was not executed')
    name = 'CrispSorter_' + receipt['version'] + '_' + args.platform
    (root / (name + '.receipt.json')).write_text(json.dumps(receipt, indent=2) + '\n')
    (root / (name + '.sha256')).write_text(''.join(a['sha256'] + '  ' + a['filename'] + '\n' for a in receipt['artifacts']))
    print('Verified:', ', '.join(a['filename'] for a in receipt['artifacts']))


if __name__ == '__main__':
    main()
