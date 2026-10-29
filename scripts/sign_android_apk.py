#!/usr/bin/env python3
"""Sign an existing APK without rebuilding it; explicitly distinguish development keys.

Production: set ANDROID_KEYSTORE, ANDROID_KEY_ALIAS,
ANDROID_KEYSTORE_PASSWORD and ANDROID_KEY_PASSWORD. Development: explicitly pass
--development-keystore; the retained private key permits subsequent local updates.
Never use a development certificate for a store release.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import zipfile


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def signer_path(explicit):
    if explicit:
        return explicit
    found = shutil.which('apksigner')
    if found:
        return found
    sdk = os.environ.get('ANDROID_HOME') or os.environ.get('ANDROID_SDK_ROOT')
    candidates = sorted((Path(sdk) / 'build-tools').glob('*/apksigner')) if sdk else []
    if not candidates:
        raise RuntimeError('Android SDK Build Tools apksigner is required')
    return str(candidates[-1])


def check_alignment(path):
    # Signing must follow alignment. Check the already-aligned Gradle output;
    # avoid rewriting the APK or its native-library offsets after signing.
    native = []
    with path.open('rb') as stream, zipfile.ZipFile(path) as archive:
        if archive.testzip() is not None:
            raise ValueError('APK ZIP integrity failed')
        for entry in archive.infolist():
            if entry.compress_type != zipfile.ZIP_STORED or entry.is_dir():
                continue
            stream.seek(entry.header_offset)
            header = stream.read(30)
            name, extra = struct.unpack_from('<HH', header, 26)
            offset = entry.header_offset + 30 + name + extra
            if offset % 4:
                raise ValueError('Run zipalign before signing: ' + entry.filename)
            if entry.filename.endswith('.so'):
                native.append({'name': entry.filename, 'offset': offset,
                               'zip_page_alignment_16k': offset % 16384 == 0})
    return native


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--apksigner')
    parser.add_argument('--development-keystore', type=Path)
    args = parser.parse_args()
    source, output = args.input.resolve(), args.output.resolve()
    if source == output or output.exists():
        raise ValueError('Choose a new output path; input and existing artifacts are preserved')
    native = check_alignment(source)
    signer = signer_path(args.apksigner)
    env = os.environ.copy()
    if args.development_keystore:
        key = args.development_keystore.resolve()
        key.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        env.update(ANDROID_KEY_ALIAS='androiddebugkey', ANDROID_KEYSTORE_PASSWORD='android', ANDROID_KEY_PASSWORD='android')
        if not key.exists():
            subprocess.run(['keytool', '-genkeypair', '-keystore', str(key), '-storetype', 'JKS',
                            '-storepass:env', 'ANDROID_KEYSTORE_PASSWORD', '-keypass:env', 'ANDROID_KEY_PASSWORD',
                            '-alias', env['ANDROID_KEY_ALIAS'], '-keyalg', 'RSA', '-keysize', '2048',
                            '-validity', '10000', '-dname', 'CN=Android Debug,O=Android,C=US'],
                           env=env, check=True, capture_output=True, text=True)
            key.chmod(0o600)
        signing = 'development certificate; not a production/store identity'
    else:
        for name in ['ANDROID_KEYSTORE', 'ANDROID_KEY_ALIAS', 'ANDROID_KEYSTORE_PASSWORD', 'ANDROID_KEY_PASSWORD']:
            if not env.get(name):
                raise RuntimeError(name + ' is required for production signing')
        key = Path(env['ANDROID_KEYSTORE']).resolve()
        signing = 'provided production certificate'
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='apk-sign-', dir=output.parent) as work:
        signed = Path(work) / output.name
        subprocess.run([signer, 'sign', '--ks', str(key), '--ks-key-alias', env['ANDROID_KEY_ALIAS'],
                        '--ks-pass', 'env:ANDROID_KEYSTORE_PASSWORD', '--key-pass', 'env:ANDROID_KEY_PASSWORD',
                        '--v4-signing-enabled', 'false', '--out', str(signed), str(source)], env=env, check=True)
        verified = subprocess.check_output([signer, 'verify', '--verbose', '--print-certs', str(signed)], text=True)
        signed_native = check_alignment(signed)
        if signed_native != native:
            raise ValueError('Signing changed native-library ZIP alignment')
        signed.replace(output)
    receipt = {'schema': 'crispsorter-apk-signing-v1', 'signing': signing,
               'input_filename': source.name, 'input_sha256': sha(source),
               'filename': output.name, 'bytes': output.stat().st_size, 'sha256': sha(output),
               'native_zip_alignment': native, 'verification': verified,
               'device_execution': 'not exercised by signature/archive verification'}
    output.with_suffix('.signing.json').write_text(json.dumps(receipt, indent=2) + '\n')
    output.with_suffix('.sha256').write_text(receipt['sha256'] + '  ' + output.name + '\n')
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
