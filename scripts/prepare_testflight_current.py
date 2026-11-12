#!/usr/bin/env python3
"""Select a source-bound draft IPA for the current-source upload-only route.

Only GitHub reads/downloads occur here. Platform signature verification and ASC
upload/distribution remain separate workflow steps after successful selection.
"""
import argparse
import json
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

from check_desktop_release_assets import gh
from check_release_target import validate_release_target
from check_release_versions import check_versions
from check_testflight_ipa import check_ipa


def prepare(repo, source, project, output, publish=False, mac_appstore=False):
    if publish is not False or mac_appstore is not False:
        raise ValueError('Current-source TestFlight requires publish=false and mac_appstore=false')
    version = check_versions(project)
    tag = 'v' + version
    validate_release_target(repo, tag, source)
    release = json.loads(gh('release', 'view', tag, '--repo', repo, '--json', 'isDraft,assets'))
    if release.get('isDraft') is not True:
        raise ValueError('Current-source TestFlight requires an unpublished draft')
    filename = f'CrispSorter_{version}_ios-arm64.ipa'
    receipt_name = f'CrispSorter_{version}_ios-arm64.receipt.json'
    assets = release.get('assets', [])
    if not isinstance(assets, list) or not all(isinstance(asset, dict) for asset in assets):
        raise ValueError('Draft asset listing is invalid')
    names = [asset.get('name') for asset in assets]
    if any(names.count(name) != 1 for name in [filename, receipt_name]):
        raise ValueError('Draft must contain exactly one current IPA and iOS receipt')
    output = Path(output).resolve()
    if output.exists() and any(output.iterdir()):
        raise ValueError('Current-source IPA output directory must be empty')
    with tempfile.TemporaryDirectory(prefix='crispsorter-testflight-selection-') as directory:
        work = Path(directory)

        def download(name):
            gh('release', 'download', tag, '--repo', repo, '--pattern', name, '--dir', str(work))
            return work / name

        receipt_path = download(receipt_name)
        if receipt_path.stat().st_size > 1024 * 1024:
            raise ValueError('iOS release receipt exceeds 1 MiB')
        receipt = json.loads(receipt_path.read_text())
        if (not isinstance(receipt, dict) or receipt.get('schema') != 'crispsorter-release-receipt-v1'
                or receipt.get('platform') != 'ios-arm64' or receipt.get('version') != version
                or receipt.get('app_source') != source):
            raise ValueError('iOS receipt does not match the exact current source/version')
        profile = receipt.get('mobile_release_profile', {})
        if not isinstance(profile, dict) or profile.get('recipe') != 'standard' or profile.get('requested') != 'standard':
            raise ValueError('Current-source TestFlight requires a standard release receipt')
        artifacts = receipt.get('artifacts', [])
        if not isinstance(artifacts, list) or len(artifacts) != 1 or not isinstance(artifacts[0], dict) or artifacts[0].get('filename') != filename:
            raise ValueError('iOS receipt does not identify exactly the expected IPA')
        item = artifacts[0]
        if (item.get('embedded_profile') is not True or item.get('version') != version
                or item.get('bundle_id') != 'com.crispstrobe.crispsorter'
                or not isinstance(item.get('sha256'), str)
                or not re.fullmatch(r'[0-9a-f]{64}', item['sha256'])):
            raise ValueError('iOS receipt lacks expected provisioned application identity/hash')
        ipa = download(filename)
        if type(item.get('bytes')) is not int or item['bytes'] <= 0 or item['bytes'] != ipa.stat().st_size:
            raise ValueError('IPA byte count does not match the release receipt')
        identity = check_ipa(ipa, version, item['sha256'])
        output.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ipa, output / filename)
        shutil.copyfile(receipt_path, output / receipt_name)
    selection = {**identity, 'source': source, 'tag': tag, 'ipa': str(output / filename), 'receipt': receipt_name}
    (output / 'selection.json').write_text(json.dumps(selection, indent=2) + '\n')
    return selection


def write_environment(selection, destination):
    values = {'IPA_VERSION': selection['version'], 'IPA_FILE': selection['ipa'], 'IPA_SHA256': selection['sha256']}
    if any('\n' in value or '\r' in value for value in values.values()):
        raise ValueError('Invalid upload selection environment value')
    with Path(destination).open('a') as stream:
        for key, value in values.items():
            stream.write(f'{key}={value}\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--project', type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--publish', choices=['true', 'false'], required=True)
    parser.add_argument('--mac-appstore', choices=['true', 'false'], required=True)
    parser.add_argument('--github-env', type=Path)
    args = parser.parse_args()
    try:
        selection = prepare(args.repo, args.source, args.project, args.output,
                            args.publish == 'true', args.mac_appstore == 'true')
        if args.github_env:
            write_environment(selection, args.github_env)
    except (OSError, RuntimeError, ValueError, KeyError, TypeError, subprocess.SubprocessError,
            zipfile.BadZipFile, plistlib.InvalidFileException) as error:
        print(f'::error::{error}', file=sys.stderr)
        sys.exit(1)
    print(json.dumps(selection, sort_keys=True))


if __name__ == '__main__':
    main()
