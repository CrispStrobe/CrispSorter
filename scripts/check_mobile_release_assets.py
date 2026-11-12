#!/usr/bin/env python3
"""Require source-bound mobile packages in the final draft before publication."""
import argparse
import json
from pathlib import Path
import re
import tempfile

from check_desktop_release_assets import gh, sha


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['tag', 'repo', 'source', 'version']:
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    release = json.loads(gh('release', 'view', args.tag, '--repo', args.repo, '--json', 'isDraft,assets'))
    if release.get('isDraft') is not True:
        raise ValueError('Refusing to publish a release that is already public')
    names = {entry['name'] for entry in release['assets']}
    expected = {
        'android-arm64': f'CrispSorter_{args.version}_android-aarch64.apk',
        'ios-arm64': f'CrispSorter_{args.version}_ios-arm64.ipa',
    }
    downloads = {name for name in names if name.endswith(('.apk', '.ipa'))
                 or ('ios' in name.lower() and name.endswith('.app.tar.gz'))}
    if downloads != set(expected.values()):
        raise ValueError('Missing or stale mobile packages in release draft')
    with tempfile.TemporaryDirectory(prefix='crispsorter-mobile-release-check-') as directory:
        work = Path(directory)

        def download(name):
            if name not in names:
                raise ValueError('Missing mobile release asset: ' + name)
            gh('release', 'download', args.tag, '--repo', args.repo,
               '--pattern', name, '--dir', str(work))
            return work / name

        for platform, filename in expected.items():
            receipt = json.loads(download(f'CrispSorter_{args.version}_{platform}.receipt.json').read_text())
            if (receipt.get('schema') != 'crispsorter-release-receipt-v1'
                    or receipt.get('platform') != platform or receipt.get('version') != args.version
                    or receipt.get('app_source') != args.source):
                raise ValueError('Mobile receipt does not match this source/version: ' + platform)
            artifacts = receipt.get('artifacts', [])
            if len(artifacts) != 1 or artifacts[0].get('filename') != filename:
                raise ValueError('Unexpected mobile artifact count or filename: ' + platform)
            item = artifacts[0]
            digest = item.get('sha256', '')
            if not re.fullmatch(r'[0-9a-f]{64}', digest) or sha(download(filename)) != digest:
                raise ValueError('Mobile asset does not match verified receipt: ' + filename)
            if platform == 'ios-arm64':
                if (item.get('embedded_profile') is not True or item.get('version') != args.version
                        or item.get('bundle_id') != 'com.crispstrobe.crispsorter'):
                    raise ValueError('iOS receipt lacks expected provisioned application metadata')
            else:
                signing = json.loads(download(filename.removesuffix('.apk') + '.signing.json').read_text())
                if (signing.get('schema') != 'crispsorter-apk-signing-v1'
                        or signing.get('filename') != filename or signing.get('sha256') != digest
                        or signing.get('signing') != 'provided production certificate'
                        or not signing.get('verification') or not item.get('signature_verification')):
                    raise ValueError('Android asset lacks matching production signing evidence')
    print('Mobile draft packages match verified release receipts for this source.')


if __name__ == '__main__':
    main()
