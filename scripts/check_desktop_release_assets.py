#!/usr/bin/env python3
"""Recheck the desktop artifacts and receipts uploaded to a GitHub draft release."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile


def gh(*args):
    return subprocess.check_output(['gh', *args], text=True)


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tag', required=True)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--version', required=True)
    args = parser.parse_args()
    release = json.loads(gh('release', 'view', args.tag, '--repo', args.repo, '--json', 'isDraft,assets'))
    if not release['isDraft']:
        raise ValueError('Refusing to publish or modify a release that is already public')
    names = {entry['name'] for entry in release['assets']}
    required = set()
    with tempfile.TemporaryDirectory(prefix='crispsorter-release-check-') as work:
        work = Path(work)
        for platform in ['macos-arm64', 'windows-x86_64']:
            receipt_name = f'CrispSorter_{args.version}_{platform}.receipt.json'
            if receipt_name not in names:
                raise ValueError('Missing current desktop receipt: ' + receipt_name)
            gh('release', 'download', args.tag, '--repo', args.repo, '--pattern', receipt_name, '--dir', str(work))
            receipt = json.loads((work / receipt_name).read_text())
            if (receipt.get('schema') != 'crispsorter-release-receipt-v1' or receipt.get('platform') != platform
                    or receipt.get('version') != args.version or receipt.get('app_source') != args.source):
                raise ValueError('Desktop receipt does not match this source/version: ' + receipt_name)
            artifacts = receipt.get('artifacts', [])
            if len(artifacts) != (2 if platform == 'macos-arm64' else 1):
                raise ValueError('Unexpected desktop artifact count')
            for item in artifacts:
                name = item.get('filename', '')
                if (not re.fullmatch(r'CrispSorter_[A-Za-z0-9_.-]+', name) or '-unsigned' in name
                        or name not in names or name in required or not item.get('signature_verification')):
                    raise ValueError('Missing, unsigned, or invalid desktop artifact: ' + name)
                required.add(name)
                gh('release', 'download', args.tag, '--repo', args.repo, '--pattern', name, '--dir', str(work))
                if not re.fullmatch(r'[0-9a-f]{64}', item.get('sha256', '')) or sha(work / name) != item['sha256']:
                    raise ValueError('Published draft asset does not match verified receipt: ' + name)
        mac_dmg = {n for n in required if n.endswith('.dmg')}
        mac_tar = {n for n in required if n.endswith('.app.tar.gz')}
        win_zip = {n for n in required if n.endswith('windows-portable.zip')}
        if len(mac_dmg) != 1 or len(mac_tar) != 1 or len(win_zip) != 1:
            raise ValueError('Missing verified macOS DMG/app archive or Windows portable ZIP')
        desktop_downloads = {n for n in names if n.endswith(('.dmg', '.exe', '.msi'))
                             or (n.endswith('.app.tar.gz') and 'ios' not in n.lower())
                             or ('windows-portable' in n and n.endswith('.zip'))}
        if desktop_downloads != required:
            raise ValueError('Unexpected stale desktop assets remain: ' + ', '.join(sorted(desktop_downloads ^ required)))
    print('Desktop draft assets match verified, signed release receipts for this source.')


if __name__ == '__main__':
    main()
