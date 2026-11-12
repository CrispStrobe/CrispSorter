#!/usr/bin/env python3
"""Check the exact IPA archive identity before an authorized TestFlight upload.

This does not replace codesign verification or source-bound build receipts.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import plistlib
import re
import sys
import zipfile

from check_release_versions import VERSION


def check_ipa(ipa, version, expected_sha=None):
    ipa = Path(ipa)
    if not isinstance(version, str) or not VERSION.fullmatch(version):
        raise ValueError('Expected IPA version must be numeric major.minor.patch')
    digest = hashlib.sha256()
    with ipa.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    actual_sha = digest.hexdigest()
    if expected_sha is not None and (not re.fullmatch(r'[0-9a-f]{64}', expected_sha) or expected_sha != actual_sha):
        raise ValueError('IPA does not match the expected SHA-256')
    with zipfile.ZipFile(ipa) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)) or any(name.startswith('/') or '\\' in name or '..' in PurePosixPath(name).parts for name in names):
            raise ValueError('IPA has ambiguous or unsafe archive entries')
        roots = {PurePosixPath(name).parts[1] for name in names
                 if len(PurePosixPath(name).parts) >= 2 and PurePosixPath(name).parts[0] == 'Payload'
                 and PurePosixPath(name).parts[1].endswith('.app')}
        if len(roots) != 1:
            raise ValueError('IPA must contain exactly one Payload application')
        app = 'Payload/' + roots.pop() + '/'
        metadata = archive.getinfo(app + 'Info.plist')
        if metadata.file_size > 1024 * 1024:
            raise ValueError('IPA application metadata exceeds 1 MiB')
        info = plistlib.loads(archive.read(metadata))
        if not isinstance(info, dict) or info.get('CFBundleIdentifier') != 'com.crispstrobe.crispsorter' or info.get('CFBundleShortVersionString') != version:
            raise ValueError('IPA bundle identity/version does not match the requested application')
        build = info.get('CFBundleVersion')
        if not isinstance(build, str) or not re.fullmatch(r'[0-9]+(?:\.[0-9]+){0,2}', build):
            raise ValueError('IPA must declare an exact numeric build number')
        executable = info.get('CFBundleExecutable')
        if not isinstance(executable, str) or not executable or executable in ('.', '..') or any(c in executable for c in '/\\\0'):
            raise ValueError('IPA executable name is invalid')
        for member in [app + executable, app + 'embedded.mobileprovision']:
            item = archive.getinfo(member)
            if item.is_dir() or item.file_size == 0:
                raise ValueError('IPA lacks a provisioned application executable')
    return {'version': version, 'build_number': build, 'bundle_id': info['CFBundleIdentifier'], 'sha256': actual_sha}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ipa', type=Path, required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--sha256', default=None)
    args = parser.parse_args()
    try:
        result = check_ipa(args.ipa, args.version, args.sha256)
    except (OSError, ValueError, KeyError, zipfile.BadZipFile, plistlib.InvalidFileException) as error:
        print(f'::error::{error}', file=sys.stderr)
        sys.exit(1)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
