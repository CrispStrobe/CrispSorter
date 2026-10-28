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


def inspect_package(platform, artifact, destination):
    if artifact.suffix == '.deb':
        subprocess.run(['dpkg-deb', '-x', str(artifact), str(destination)], check=True)
        executable = next((destination / 'usr/bin').glob('*'))
    elif artifact.suffix in ('.zip', '.apk', '.ipa'):
        with zipfile.ZipFile(artifact) as archive:
            archive.extractall(destination)
        if platform == 'windows-x86_64':
            executable = next(destination.rglob('crispsorter.exe'))
        elif platform == 'android-arm64':
            libraries = list(destination.glob('lib/arm64-v8a/*.so'))
            if not libraries or not (destination / 'AndroidManifest.xml').is_file():
                raise ValueError('APK lacks the arm64 application or manifest')
            return {'inspection': 'APK arm64 native libraries and manifest present', 'native_libraries': [p.name for p in libraries]}
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
        with tarfile.open(artifact) as archive:
            archive.extractall(destination, filter='data')
        app = next(destination.glob('*.app'))
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
    return {'inspection': 'packaged executable starts with build-tree libraries hidden', 'checks': checks}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--platform', required=True, choices=['linux-x86_64', 'windows-x86_64', 'macos-arm64', 'android-arm64', 'ios-arm64'])
    args = parser.parse_args()
    root = Path.cwd()
    patterns = {
        'linux-x86_64': ['target/**/bundle/deb/*.deb'],
        'windows-x86_64': ['CrispSorter_*windows-portable.zip'],
        'macos-arm64': ['target/**/bundle/macos/CrispSorter*.app.tar.gz', 'target/**/bundle/dmg/*.dmg'],
        'android-arm64': ['CrispSorter_*android*.apk'],
        'ios-arm64': ['CrispSorter_*ios*.ipa', 'CrispSorter_*ios*.app.tar.gz'],
    }
    artifacts = sorted({p.resolve() for pattern in patterns[args.platform] for p in root.glob(pattern)})
    if not artifacts:
        raise RuntimeError('No final artifacts found for ' + args.platform)
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
                entry['inspection'] = 'checksum; executable smoke uses the matching app archive'
            receipt['artifacts'].append(entry)
    if not any('checks' in a for a in receipt['artifacts']) and args.platform in ['linux-x86_64', 'windows-x86_64', 'macos-arm64']:
        raise RuntimeError('Desktop package was not executed')
    name = 'CrispSorter_' + receipt['version'] + '_' + args.platform
    (root / (name + '.receipt.json')).write_text(json.dumps(receipt, indent=2) + '\n')
    (root / (name + '.sha256')).write_text(''.join(a['sha256'] + '  ' + a['filename'] + '\n' for a in receipt['artifacts']))
    print('Verified:', ', '.join(a['filename'] for a in receipt['artifacts']))


if __name__ == '__main__':
    main()
